//! trace v2 amend v1.6 负向验收（对抗/负向语义）。红相冻结。
//!
//! 断言唯一依据：`.dev-team/trace-fix-contract.md` §C9 + amend v1.6（1–7、12 的消费者侧负向口径）。
//! 覆盖：脱敏不提前闭合 / `_` 前缀键保留与服务端保留名覆盖 / warnings 并集 / truncated 服务端判定 /
//! run_id 轮次级命中 / summary 无 note / llm_calls 归一 / payload_bytes HELP 文案。
//! 全黑盒：仅经进程级接缝（`DATABASE_URL`/`PORT`/`CLIENT_TOKEN`/`ADMIN_TOKEN`）+ HTTP；
//! 每用例独立端口 + 独立临时 SQLite 库。webhook 击穿由 server 单测覆盖，不在本文件重复。

#[path = "harness.rs"]
mod harness;

use harness::{Resp, Server, TEST_CLIENT_TOKEN};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

const TRACES: &str = "/api/v1/traces";

// ── helper ──────────────────────────────────────────────────────────────────
fn uniq(prefix: &str) -> String {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    format!("{prefix}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst))
}

fn post<A: Ext>(api: &A, body: Value) -> Resp {
    let r = api.post(body);
    assert!(r.status == 200 || r.status == 201, "[C5] POST {TRACES} 应 2xx：{}", r.describe());
    r
}

fn post_turns<A: Ext>(api: &A, session: &str, turns: Vec<Value>) -> Resp {
    post(api, json!({"session_id": session, "project": "job_copilot", "environment": "development", "release": "dev", "turns": turns}))
}

fn list_resp<A: Ext>(api: &A, query: &str) -> Resp {
    let r = api.get(&format!("{TRACES}?{query}"));
    assert_eq!(r.status, 200, "[C5] GET list 应 200：{}", r.describe());
    r
}

fn rows_of(resp: &Resp) -> Vec<Value> {
    resp.json.as_array().cloned().unwrap_or_else(|| panic!("[C5] GET list 必须返回数组：{}", resp.raw))
}

/// 该 session 的聚合行。
fn record<A: Ext>(api: &A, session: &str) -> Value {
    let rows = rows_of(&list_resp(api, &format!("session_id={session}")));
    assert_eq!(rows.len(), 1, "[C5] session {session} 应仅一行聚合记录：{} 行", rows.len());
    rows[0].clone()
}

fn turn_of(rec: &Value, turn_id: &str) -> Value {
    rec["payload"]["turns"].as_array().cloned().unwrap_or_default().into_iter()
        .find(|t| t["turn_id"] == turn_id).unwrap_or_else(|| panic!("[C9] payload.turns 缺少 turn `{turn_id}`：{rec}"))
}

fn tool_call(turn: &Value, idx: usize) -> Value {
    turn["tool_calls"].as_array().cloned().unwrap_or_default().into_iter()
        .nth(idx).unwrap_or_else(|| panic!("[C9] turn 缺少第 {idx} 个 tool_call：{turn}"))
}

fn header(resp: &Resp, name: &str) -> Option<String> {
    resp.headers.get(name).map(|v| v.to_str().unwrap_or_default().to_string())
}

/// 供本地 helper 复用的最小 HTTP 面（当前只有 harness `Server` 一种实现）。
trait Ext {
    fn get(&self, path: &str) -> Resp;
    fn post(&self, body: Value) -> Resp;
}

impl Ext for Server {
    fn get(&self, path: &str) -> Resp {
        Server::get_admin(self, path)
    }
    fn post(&self, body: Value) -> Resp {
        Server::send(self, reqwest::Method::POST, TRACES, &[("x-client-token", TEST_CLIENT_TOKEN)], Some(&body))
    }
}

/// 造一条 failed turn（带一个 tool_call），用于脱敏/截断类负向用例。
fn failed_call(turn_id: &str, call: Value) -> Value {
    json!({"turn_id": turn_id, "status": "failed", "tool_calls": [call]})
}

// ── v1.6-1 脱敏不提前闭合：值内出现另一种引号不得截断 ────────────────────────
#[test]
fn v16_1_sanitizer_matches_closing_quote_of_same_kind() {
    let server = Server::start();
    let cases = [
        ("password=\"ab'cd\"", "双引号包裹、值内单引号"),
        ("password='ab\"cd'", "单引号包裹、值内双引号"),
    ];
    for (i, (snippet, label)) in cases.iter().enumerate() {
        let session = uniq(&format!("n1-{i}"));
        let output = format!("cfg {snippet} end");
        post_turns(&server, &session, vec![failed_call("t1", json!({
            "tool_call_id": "c1", "tool_name": "load_cfg", "status": "error", "output": output}))]);
        let stored = tool_call(&turn_of(&record(&server, &session), "t1"), 0);
        let stored = stored["output"].as_str().unwrap_or_default().to_string();
        assert!(stored.contains("[REDACTED_SECRET]"), "[v1.6-1] {label} 必须遮蔽并留标记：{stored}");
        for fragment in ["ab", "cd"] {
            assert!(!stored.contains(fragment),
                "[v1.6-1] {label} 的值不得泄露任何片段（命中 `{fragment}`）：{stored}");
        }
    }
}

// ── v1.6-2 `_` 前缀键保留 + 服务端保留名覆盖 ────────────────────────────────
#[test]
fn v16_2_underscore_keys_kept_and_reserved_keys_overridden() {
    let server = Server::start();
    let session = uniq("n2");
    post(&server, json!({
        "session_id": session,
        "_sdk": "v1",
        "foo": 1,
        "_redactions": {"secrets": 99, "paths": 99},
        "_warnings": [{"turn_index": 9, "reason": "forged"}],
        "turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [{
            "tool_call_id": "c1", "tool_name": "read", "status": "error", "output": "open /home/bob/y.py"}]}],
    }));
    let rec = record(&server, &session);
    let payload = &rec["payload"];
    assert_eq!(payload["_unmapped"]["_sdk"], "v1", "[v1.6-2] `_` 前缀的未知顶层键必须保留到 _unmapped（不得特殊丢弃）");
    assert_eq!(payload["_unmapped"]["foo"], 1, "[v1.6-2] 普通未知键必须保留到 _unmapped");
    let redactions = payload["_redactions"].to_string();
    assert!(!redactions.contains("99"), "[v1.6-2] 客户端伪造的 _redactions 必须被服务端值覆盖：{redactions}");
    let warnings = payload["_warnings"].to_string();
    assert!(!warnings.contains("forged"), "[v1.6-2] 客户端伪造的 _warnings 必须被服务端值覆盖：{warnings}");
}

// ── v1.6-3 warnings 二次上报并集去重（按 turn_index+reason）──────────────────
#[test]
fn v16_3_warnings_are_union_merged() {
    let server = Server::start();
    let session = uniq("n3");
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "succeeded"}), json!("bad-1")]);
    post_turns(&server, &session, vec![json!("bad-0"), json!({"turn_id": "t1", "status": "succeeded"})]);
    let rec = record(&server, &session);
    let warnings = rec["payload"]["_warnings"].as_array().cloned().unwrap_or_default();
    let indexes: Vec<i64> = warnings.iter().filter_map(|w| w["turn_index"].as_i64()).collect();
    assert!(indexes.contains(&1), "[v1.6-3] 首轮的 turn_index=1 警告不得被二次上报覆盖：{warnings:?}");
    assert!(indexes.contains(&0), "[v1.6-3] 次轮的 turn_index=0 警告必须并入：{warnings:?}");
    let mut unique = indexes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), indexes.len(), "[v1.6-3] warnings 必须按 turn_index+reason 去重：{warnings:?}");
}

// ── v1.6-4 truncated 由服务端判定：客户端预置值忽略 ──────────────────────────
#[test]
fn v16_4_truncated_decided_by_server_only() {
    let server = Server::start();
    // (a) 超大 output + 客户端预置 false → 服务端必须置 true 且长度受限（先脱敏后截断）
    let session = uniq("n4-big");
    post_turns(&server, &session, vec![failed_call("t1", json!({
        "tool_call_id": "c1", "tool_name": "dump", "status": "error",
        "truncated": false, "output": "y".repeat(22_000)}))]);
    let big = tool_call(&turn_of(&record(&server, &session), "t1"), 0);
    let len = big["output"].as_str().map(|s| s.chars().count()).unwrap_or(0);
    assert_eq!(big["truncated"], true, "[v1.6-4a] 超 20000 字符必须由服务端置 truncated:true（实际 {:?}）", big["truncated"]);
    assert!(len <= 20_000, "[v1.6-4a] 截断后长度必须 <= 20000（实际 {len}）");

    // (b) 小 output + 客户端预置 true → 客户端判定必须被忽略（不得保留 true）
    let session = uniq("n4-small");
    post_turns(&server, &session, vec![failed_call("t1", json!({
        "tool_call_id": "c1", "tool_name": "read", "status": "error",
        "truncated": true, "output": "short"}))]);
    let small = tool_call(&turn_of(&record(&server, &session), "t1"), 0);
    assert_ne!(small["truncated"], true, "[v1.6-4b] 客户端预置的 truncated=true 必须被忽略（服务端判定）：{small}");
}

// ── v1.6-5 `?run_id` 轮次级命中（traces.run_id 与轮次 run_id 不同）────────────
#[test]
fn v16_5_run_id_matches_turn_level_run() {
    let server = Server::start();
    let session = uniq("n5");
    post(&server, json!({
        "session_id": session, "project": "job_copilot", "environment": "development", "release": "dev",
        "run_id": "run-A",
        "turns": [
            {"turn_id": "t1", "status": "succeeded", "run_id": "run-B"},
            {"turn_id": "t2", "status": "succeeded", "run_id": "run-C"}],
    }));
    for run_id in ["run-B", "run-C"] {
        let rows = rows_of(&list_resp(&server, &format!("run_id={run_id}")));
        assert!(rows.iter().any(|r| r["session_id"] == session.as_str()),
            "[v1.6-5] `?run_id={run_id}` 必须命中含该轮次 run 的 session（traces.run_id=run-A）：{rows:?}");
    }
}

// ── v1.6-6 summary 投影不含 note；X-Total-Count == 过滤总数 ──────────────────
#[test]
fn v16_6_summary_omits_note_and_counts_filtered() {
    let server = Server::start();
    let project = format!("ext2-proj-{}", std::process::id());
    let mut ids = Vec::new();
    for i in 0..2 {
        let session = uniq(&format!("n6-{i}"));
        let created = post(&server, json!({"session_id": session, "project": project,
            "environment": "development", "release": "dev", "turns": [{"turn_id": "t1", "status": "succeeded"}]}));
        ids.push(created.json["id"].as_str().unwrap_or_default().to_string());
    }
    let patched = server.patch_admin(&format!("{TRACES}/{}", ids[0]), &json!({"eval_status": "triage_good", "note": "人工备注"}));
    assert_eq!(patched.status, 200, "[v1.6-6] PATCH note 应 200：{}", patched.describe());

    let resp = list_resp(&server, &format!("fields=summary&project={project}&limit=1"));
    let rows = rows_of(&resp);
    assert_eq!(rows.len(), 1, "[v1.6-6] limit=1 必须只回 1 行：{}", resp.raw);
    assert!(rows[0].get("note").is_none(), "[v1.6-6] fields=summary 投影不得返回 note 键（实际键：{:?}）",
        rows[0].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
    assert_eq!(header(&resp, "x-total-count").as_deref(), Some("2"),
        "[v1.6-6] X-Total-Count 必须等于过滤总数 2（实际 {:?}）", header(&resp, "x-total-count"));
}

// ── v1.6-6 llm_calls 服务端归一：≤50 条 + error_type 合法化 ──────────────────
#[test]
fn v16_7_llm_calls_normalized() {
    let server = Server::start();
    let session = uniq("n7");
    let mut calls: Vec<Value> = (0..5000).map(|i| json!({
        "model": "m", "provider": "p", "status": "ok", "duration_ms": 1, "error_type": format!("E{i}")})).collect();
    calls.push(json!({"model": "m2", "provider": "p", "status": "error", "error_type": "X".repeat(500)}));
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "succeeded", "llm_calls": calls})]);
    let stored = turn_of(&record(&server, &session), "t1");
    let kept = stored["llm_calls"].as_array().cloned().unwrap_or_default();
    assert!(kept.len() <= 50, "[v1.6-6] llm_calls 必须截断到 <= 50 条（实际 {}）", kept.len());
    assert!(kept.iter().all(|c| c["error_type"].as_str().map(|s| s.chars().count() <= 64).unwrap_or(true)),
        "[v1.6-6] 超 64 字符的 error_type 条目必须丢弃：{}", &stored["llm_calls"]);
}

// ── v1.6-12 `payload_bytes_total` HELP 文案 = 上报体积 ───────────────────────
#[test]
fn v16_8_payload_bytes_help_says_upload_volume() {
    let server = Server::start();
    let metrics = server.get_anon("/metrics");
    assert_eq!(metrics.status, 200, "[v1.6-12] /metrics 应 200：{}", metrics.describe());
    let helper = metrics.raw.lines().find(|l| l.starts_with("# HELP pony_sentry_trace_payload_bytes_total"));
    let helper = helper.unwrap_or_else(|| panic!("[v1.6-12] /metrics 缺少 payload_bytes_total 的 HELP 行：\n{}", metrics.raw));
    assert!(helper.contains("上报体积"),
        "[v1.6-12] payload_bytes_total 的 HELP 必须标注「上报体积」而非存储体积：{helper}");
}
