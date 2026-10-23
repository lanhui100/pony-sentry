//! trace v2 扩展验收（amend v1.5：M-04/W-05/I-01/I-02/I-07/W-04/I-04/I-06/I-09/M-03）。红相冻结。
//!
//! 断言唯一依据：`.dev-team/trace-fix-contract.md` §C9 + amend v1.5。
//! 全黑盒：只经进程级接缝（`DATABASE_URL`/`PORT`/`CLIENT_TOKEN`/`ADMIN_TOKEN`/`TRACE_WEBHOOK_URL`）
//! + HTTP 驱动被测二进制；每用例独立端口 + 独立临时 SQLite 库。
//! M-03 用 python3 本地 stub（真实收包）验证 webhook 与 10 分钟去重，非 mock 库、非内存桩。

#[path = "harness.rs"]
mod harness;

use harness::{Resp, Server, TEST_ADMIN_TOKEN, TEST_CLIENT_TOKEN};
use serde_json::{json, Value};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const TRACES: &str = "/api/v1/traces";

// ── HTTP 夹具抽象 ───────────────────────────────────────────────────────────
trait Api {
    fn send_api(&self, method: reqwest::Method, path: &str, headers: &[(&str, &str)], body: Option<&Value>) -> Resp;
    fn get_api(&self, path: &str) -> Resp {
        self.send_api(reqwest::Method::GET, path, &[("x-admin-token", TEST_ADMIN_TOKEN)], None)
    }
    fn post_api(&self, body: &Value) -> Resp {
        self.send_api(reqwest::Method::POST, TRACES, &[("x-client-token", TEST_CLIENT_TOKEN)], Some(body))
    }
}

impl Api for Server {
    fn send_api(&self, method: reqwest::Method, path: &str, headers: &[(&str, &str)], body: Option<&Value>) -> Resp {
        Server::send(self, method, path, headers, body)
    }
}

fn turn(turn_id: &str, status: &str) -> Value {
    json!({"turn_id": turn_id, "status": status, "attempt_index": 1, "degraded": false})
}

/// 上报任意 trace 体（断言必须 2xx：本文件所有失败都是「能力缺失」而非请求非法）。
fn post_trace<A: Api>(api: &A, body: Value) -> Resp {
    let r = api.post_api(&body);
    assert!(r.status == 200 || r.status == 201, "[C5] POST {TRACES} 应 2xx：{}", r.describe());
    r
}

fn post_session<A: Api>(api: &A, session: &str, body: Value) -> Resp {
    let mut body = body;
    body["session_id"] = json!(session);
    post_trace(api, body)
}

fn list_resp<A: Api>(api: &A, query: &str) -> Resp {
    let r = api.get_api(&format!("{TRACES}?{query}"));
    assert_eq!(r.status, 200, "[C5] GET list 应 200：{}", r.describe());
    r
}

fn list<A: Api>(api: &A, query: &str) -> Vec<Value> {
    list_resp(api, query).json.as_array().cloned().unwrap_or_else(|| panic!("[C5] GET list 必须返回数组：{query}"))
}

/// 该 session 的聚合行（单行真相）。
fn record<A: Api>(api: &A, session: &str) -> Value {
    let rows = list(api, &format!("session_id={session}"));
    assert_eq!(rows.len(), 1, "[C5] session {session} 应仅一行聚合记录：{} 行", rows.len());
    rows[0].clone()
}

fn turn_by_id(rec: &Value, turn_id: &str) -> Value {
    rec["payload"]["turns"].as_array().cloned().unwrap_or_default().into_iter()
        .find(|t| t["turn_id"] == turn_id).unwrap_or_else(|| panic!("[C9] payload.turns 缺少 turn `{turn_id}`：{rec}"))
}

fn uniq(prefix: &str) -> String {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    format!("{prefix}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn brief(raw: &str) -> String {
    if raw.chars().count() > 200 { format!("{}…", raw.chars().take(200).collect::<String>()) } else { raw.to_string() }
}

// ── 可注入 env 的被测进程（M-03 需要 TRACE_WEBHOOK_URL）──────────────────────
struct Proc {
    child: Child,
    base: String,
    dir: PathBuf,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("契约测试包必须位于仓库 tests/ 目录").to_path_buf()
}

/// 被测二进制：`PONY_SENTRY_BIN` 显式指定时直接用（红相用快照，避免与并发实施者抢 cargo 锁）。
fn binary() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(explicit) = std::env::var("PONY_SENTRY_BIN") {
            let path = PathBuf::from(explicit);
            assert!(path.is_file(), "PONY_SENTRY_BIN 指向的二进制不存在：{}", path.display());
            return path;
        }
        let root = repo_root();
        let st = Command::new("cargo").args(["build", "--offline", "-p", "pony-sentry-server"]).current_dir(&root).status().expect("无法 cargo build");
        assert!(st.success(), "构建 pony-sentry-server 失败");
        root.join("target/debug/pony-sentry-server")
    }).clone()
}

fn unique_dir(prefix: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("pony-sentry-ext-{prefix}-{}-{}-{nanos}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).expect("无法创建临时目录");
    dir
}

impl Proc {
    fn start(dir: &Path, extra_env: &[(&str, String)]) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("无法分配端口").port();
        let mut cmd = Command::new(binary());
        cmd.current_dir(dir).env("PORT", port.to_string())
            .env("DATABASE_URL", format!("sqlite:{}?mode=rwc", dir.join("trace.db").display()))
            .env("CLIENT_TOKEN", TEST_CLIENT_TOKEN).env("ADMIN_TOKEN", TEST_ADMIN_TOKEN)
            .env("RUST_LOG", "info").env_remove("RETENTION_DISABLED").env_remove("RETENTION_DRY_RUN")
            .env_remove("TRACE_WEBHOOK_URL")
            .stdout(Stdio::null()).stderr(Stdio::piped());
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        let me = Self { child: cmd.spawn().expect("无法启动被测二进制"), base: format!("http://127.0.0.1:{port}"), dir: dir.to_path_buf() };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !harness::http_client().get(format!("{}/healthz", me.base)).send().is_ok_and(|r| r.status().is_success()) {
            assert!(Instant::now() < deadline, "[C9] 被测服务未在 30s 内就绪：{}/healthz", me.base);
            std::thread::sleep(Duration::from_millis(100));
        }
        me
    }

    fn send(&self, method: reqwest::Method, path: &str, headers: &[(&str, &str)], body: Option<&Value>) -> Resp {
        let mut req = harness::http_client().request(method, format!("{}{path}", self.base));
        for (key, value) in headers {
            req = req.header(*key, *value);
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().unwrap_or_else(|err| panic!("请求 {path} 失败：{err}"));
        let (status, headers, raw) = (resp.status().as_u16(), resp.headers().clone(), resp.text().unwrap_or_default());
        let json = serde_json::from_str(&raw).unwrap_or(Value::Null);
        Resp { status, raw, json, headers }
    }
}

impl Api for Proc {
    fn send_api(&self, method: reqwest::Method, path: &str, headers: &[(&str, &str)], body: Option<&Value>) -> Resp {
        self.send(method, path, headers, body)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ── M-04 凭证遮蔽（单引号 key / 裸 token=）──────────────────────────────────
#[test]
fn m_04_secrets_in_tool_output_are_redacted() {
    let server = Server::start();
    let session = uniq("m4");
    post_session(&server, &session, json!({"turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [{
        "tool_call_id": "c1", "tool_name": "load_cfg", "status": "error",
        "output": "cfg: 'password': 'hunter2' and token=abc123 end"}]}]}));
    let rec = record(&server, &session);
    let output = turn_by_id(&rec, "t1")["tool_calls"][0]["output"].as_str().unwrap_or_default().to_string();
    assert!(!output.contains("hunter2"), "[M-04] 单引号写法 `'password': 'x'` 的值必须遮蔽：{output}");
    assert!(!output.contains("abc123"), "[M-04] 裸 `token=` 的值必须遮蔽：{output}");
    assert!(output.contains("[REDACTED_SECRET]"), "[M-04] 遮蔽后必须留下 [REDACTED_SECRET] 标记：{output}");
}

// ── W-05 reported_at 偏差拒绝 + 默认按 reported_at 排序 ──────────────────────
#[test]
fn w_05_reported_at_range_and_default_ordering() {
    let server = Server::start();
    let session = uniq("w5-old");
    let stale = now_ms() - 3 * 24 * 3600 * 1000;
    // [amend] 越界用例必须用原生 POST（期望 400），不能用断言 2xx 的 post_session。
    let mut stale_body = json!({"session_id": session, "reported_at_ms": stale, "turns": [turn("t1", "succeeded")]});
    stale_body["session_id"] = json!(session);
    let resp = server.post_api(&stale_body);
    assert_eq!(resp.status, 400, "[W-05] reported_at_ms 偏差 > 24h 必须 400：{}", brief(&resp.raw));
    assert!(!resp.raw.contains("Database error"), "[W-05] 400 响应体不得回显 DB 原文：{}", brief(&resp.raw));

    // 默认排序按 reported_at DESC：先建的 newer、后建的 older
    let project = format!("c9ext-order-{}", std::process::id());
    let newer = uniq("w5-newer");
    let older = uniq("w5-older");
    post_session(&server, &newer, json!({"project": project, "reported_at_ms": now_ms(), "turns": [turn("t1", "succeeded")]}));
    post_session(&server, &older, json!({"project": project, "reported_at_ms": now_ms() - 2 * 3600 * 1000, "turns": [turn("t1", "succeeded")]}));
    let rows = list(&server, &format!("project={project}&limit=10"));
    assert_eq!(rows.len(), 2, "[W-05] 项目过滤应命中 2 行：{rows:?}");
    assert_eq!(rows[0]["session_id"], newer.as_str(), "[W-05] 默认列表必须按 reported_at DESC（新→旧）：{rows:?}");
    let by_created = list(&server, &format!("project={project}&order=created_at&limit=10"));
    assert_eq!(by_created[0]["session_id"], older.as_str(), "[W-05] `?order=created_at` 必须改按 created_at 排序（后建在前）：{by_created:?}");
}

// ── I-01 未知顶层字段保留到 payload._unmapped ───────────────────────────────
#[test]
fn i_01_unknown_top_level_fields_kept_in_unmapped() {
    let server = Server::start();
    let session = uniq("i1");
    post_session(&server, &session, json!({"foo_unknown": "x", "another_unknown": {"k": 1}, "turns": [turn("t1", "succeeded")]}));
    let rec = record(&server, &session);
    assert_eq!(rec["payload"]["_unmapped"]["foo_unknown"], "x", "[I-01] 未知顶层字段必须保留到 payload._unmapped（payload 现有键：{:?}）", rec["payload"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
    assert_eq!(rec["payload"]["_unmapped"]["another_unknown"]["k"], 1, "[I-01] 未知对象字段也必须保留");
}

// ── I-02 非法 turn 归一 + _warnings ─────────────────────────────────────────
#[test]
fn i_02_invalid_turns_normalized_with_warnings() {
    let server = Server::start();
    let session = uniq("i2");
    let resp = post_session(&server, &session, json!({"turns": [{"turn_id": "t1", "status": 123}, "not-an-object"]}));
    assert_eq!(resp.status, 201, "[I-02] 非法 turn 不得拒绝整条请求（应 201）：{}", resp.describe());
    let rec = record(&server, &session);
    let warnings = rec["payload"]["_warnings"].as_array().cloned().unwrap_or_default();
    assert!(!warnings.is_empty(), "[I-02] 非对象 turn 必须记入 payload._warnings（当前 payload 键：{:?}）", rec["payload"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
    assert!(warnings.iter().any(|w| w["reason"] == "invalid_turn"), "[I-02] warning.reason 必须为 invalid_turn：{warnings:?}");
    assert!(warnings.iter().all(|w| w.get("turn_index").is_some()), "[I-02] warning 必须带 turn_index：{warnings:?}");
    assert_eq!(turn_by_id(&rec, "t1")["status"], "unknown", "[I-02] status 非字符串必须归一为 unknown（实际 {:?}）", turn_by_id(&rec, "t1")["status"]);
}

// ── I-07 脱敏计数注入 payload._redactions ───────────────────────────────────
#[test]
fn i_07_redaction_counts_are_exposed() {
    let server = Server::start();
    let session = uniq("i7");
    post_session(&server, &session, json!({"turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [{
        "tool_call_id": "c1", "tool_name": "open_file", "status": "error",
        "output": "open /home/alice/x.py with api_key=abcd1234"}]}]}));
    let rec = record(&server, &session);
    let redactions = &rec["payload"]["_redactions"];
    let paths = redactions["paths"].as_i64().unwrap_or(0);
    let secrets = redactions["secrets"].as_i64().unwrap_or(0);
    assert!(paths >= 1, "[I-07] payload._redactions.paths 必须 >= 1（实际 {paths}，_redactions={}）", redactions);
    assert!(secrets >= 1, "[I-07] payload._redactions.secrets 必须 >= 1（实际 {secrets}）");
    let output = turn_by_id(&rec, "t1")["tool_calls"][0]["output"].as_str().unwrap_or_default().to_string();
    assert!(output.contains("[USER_HOME]") && output.contains("[REDACTED_SECRET]"), "[I-07] 路径/密钥仍须改写：{output}");
}

// ── W-04 ?run_id 同时匹配 traces.run_id 与任一轮次 run_id ───────────────────
#[test]
fn w_04_run_id_matches_any_turn() {
    let server = Server::start();
    let session = uniq("w4");
    post_session(&server, &session, json!({"run_id": "run-old", "turns": [json!({"turn_id": "t1", "status": "succeeded", "run_id": "run-old"})]}));
    post_session(&server, &session, json!({"run_id": "run-new", "turns": [json!({"turn_id": "t2", "status": "succeeded", "run_id": "run-new"})]}));
    for run_id in ["run-old", "run-new"] {
        let rows = list(&server, &format!("run_id={run_id}"));
        assert!(rows.iter().any(|r| r["session_id"] == session.as_str()),
            "[W-04] `?run_id={run_id}` 必须命中含该 run 的 session（旧 run 也不能丢）：{rows:?}");
    }
}

// ── I-04 ?fields=summary 投影 + X-Total-Count ───────────────────────────────
#[test]
fn i_04_summary_projection_and_total_count_header() {
    let server = Server::start();
    let project = format!("c9ext-i04-{}", std::process::id());
    for i in 0..3 {
        let session = uniq(&format!("i4-{i}"));
        post_session(&server, &session, json!({"project": project, "turns": [turn("t1", "succeeded")]}));
    }
    let resp = list_resp(&server, &format!("fields=summary&project={project}&limit=2"));
    let rows = resp.json.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 2, "[I-04] limit=2 必须只回 2 行：{}", brief(&resp.raw));
    for row in &rows {
        assert!(row.get("payload").is_none(), "[I-04] ?fields=summary 记录不得含 payload 字段（实际键：{:?}）", row.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
        assert!(row.get("id").is_some() && row.get("session_id").is_some(), "[I-04] summary 投影必须保留 id/session_id");
    }
    let total = resp.headers.get("x-total-count").map(|v| v.to_str().unwrap_or_default().to_string());
    assert_eq!(total.as_deref(), Some("3"), "[I-04] 响应头 X-Total-Count 必须等于过滤总数 3（实际 {total:?}）");
}

// ── I-06 单 tool_call 输出截断 + 三个新指标 ─────────────────────────────────
#[test]
fn i_06_large_tool_output_truncated_and_metrics() {
    let server = Server::start();
    let session = uniq("i6");
    let big = "y".repeat(22_000);
    post_session(&server, &session, json!({"turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [{
        "tool_call_id": "c1", "tool_name": "dump", "status": "error", "output": big}]}]}));
    let rec = record(&server, &session);
    let call = &turn_by_id(&rec, "t1")["tool_calls"][0];
    let len = call["output"].as_str().map(|s| s.chars().count()).unwrap_or(0);
    assert_eq!(call["truncated"], true, "[I-06] 超过 20000 字符的 tool_call.output 必须置 truncated:true（实际 {:?}，长度 {len}）", call["truncated"]);
    assert!(len <= 20_000, "[I-06] 截断后长度必须 <= 20000（实际 {len}）");

    let metrics = server.get_anon("/metrics");
    assert_eq!(metrics.status, 200, "[I-06] /metrics 应 200：{}", metrics.describe());
    for name in ["pony_sentry_trace_payload_bytes_total", "pony_sentry_trace_merge_lost_turns_total", "pony_sentry_trace_truncated_total"] {
        assert!(metrics.raw.contains(name), "[I-06] /metrics 必须包含指标 `{name}`：\n{}", brief(&metrics.raw));
    }
}

// ── I-09 可选生产者字段缺失不报错、不臆造、不误告警 ─────────────────────────
// 注：本条是「可选性守卫」（无 phase/sequence/node 的 turn 必须照常入库/返回）。
// 当前实现已满足 → 红相中预期为 GREEN-GUARD，不计入「能力缺失」失败集，收据中单列。
#[test]
fn i_09_optional_turn_fields_absent_is_fine() {
    let server = Server::start();
    let session = uniq("i9");
    let resp = post_session(&server, &session, json!({"turns": [{"turn_id": "t1", "status": "succeeded"}]}));
    assert!(resp.status == 200 || resp.status == 201, "[I-09] 缺 phase/sequence/node 的 turn 必须照常接收：{}", resp.describe());
    let rec = record(&server, &session);
    let stored = turn_by_id(&rec, "t1");
    for key in ["phase", "sequence", "node"] {
        assert!(stored.get(key).is_none(), "[I-09] 缺失的可选字段不得被臆造为 null/占位（{key}）：{stored}");
    }
    let warnings = rec["payload"]["_warnings"].as_array().cloned().unwrap_or_default();
    assert!(warnings.is_empty(), "[I-09] 可选字段缺失不得记为 _warnings：{warnings:?}");
}

// ── M-03 webhook：失败 trace 触发一次 POST，10 分钟内同键去重 ────────────────
const WEBHOOK_STUB_PY: &str = r#"
import http.server, sys

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('content-length') or 0)
        body = self.rfile.read(length).decode('utf-8', 'replace')
        with open(sys.argv[2], 'a', encoding='utf-8') as fh:
            fh.write(body.replace('\n', ' ') + '\n')
        self.send_response(200)
        self.send_header('content-length', '2')
        self.end_headers()
        self.wfile.write(b'ok')

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
"#;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("无法分配端口").port()
}

/// 本地收包 stub（RAII：断言 panic 时也回收子进程，不留孤儿）。
struct WebhookStub {
    child: Child,
    log: PathBuf,
    port: u16,
}

impl WebhookStub {
    fn start(dir: &Path) -> Self {
        let port = free_port();
        let log = dir.join("webhook.log");
        let child = Command::new("python3").arg("-c").arg(WEBHOOK_STUB_PY).arg(port.to_string()).arg(&log)
            .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().expect("无法启动 webhook stub");
        let me = Self { child, log, port };
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(("127.0.0.1", me.port)).is_err() {
            assert!(Instant::now() < deadline, "[M-03] webhook stub 未在 15s 内监听 {}", me.port);
            std::thread::sleep(Duration::from_millis(50));
        }
        me
    }
}

impl Drop for WebhookStub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn webhook_hits(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_string()).collect()
}

/// 轮询等待收包数达到 `want`（上限 `timeout`），返回最终收包数。
fn wait_hits(log: &Path, want: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let hits = webhook_hits(log).len();
        if hits >= want || Instant::now() >= deadline {
            return hits;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn m_03_webhook_fires_once_and_dedups() {
    let dir = unique_dir("m3");
    let stub = WebhookStub::start(&dir);
    let log = stub.log.clone();
    let proc = Proc::start(&dir, &[("TRACE_WEBHOOK_URL", format!("http://127.0.0.1:{}/hook", stub.port))]);
    let project = format!("c9ext-m3-{}", std::process::id());
    let body = |session: &str| json!({
        "session_id": session, "project": project,
        "turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [
            {"tool_call_id": "c1", "tool_name": "format_jd_to_json", "status": "error", "error": {"message": "boom"}}]}]});

    post_trace(&proc, body(&uniq("m3-a")));
    let first = wait_hits(&log, 1, Duration::from_secs(10));
    assert_eq!(first, 1, "[M-03] 失败 trace 必须触发 1 次 webhook POST（实际 {first} 次，日志 {:?}）", webhook_hits(&log));
    let hits = webhook_hits(&log);
    assert!(hits[0].contains(&project), "[M-03] webhook 报文必须含 project（去重键组成项）：{}", brief(&hits[0]));

    // 同 project+tool_name+last_turn_status 的第二次（不同 session）必须被 10 分钟去重窗口吞掉
    post_trace(&proc, body(&uniq("m3-b")));
    let second = wait_hits(&log, 2, Duration::from_secs(3));
    assert_eq!(second, 1, "[M-03] 10 分钟内同键第二次不得再发（实际 {second} 次）：{:?}", webhook_hits(&log));

}
