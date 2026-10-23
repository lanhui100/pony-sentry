//! PonySentry trace v2 验收测试（黑盒，红相冻结）。断言唯一依据：`.dev-team/trace-fix-contract.md` §C9。
//! C9.11（前端徽标/失败原因区）见 `web/tests/trace-ui.test.js`；C9.12（回归）由
//! `cargo test --workspace` + `web pnpm test` 覆盖。零 crate 内部类型依赖：只经进程级接缝
//! （`DATABASE_URL`/`PORT`/`CLIENT_TOKEN`/`ADMIN_TOKEN`）+ HTTP 驱动被测二进制，故红相失败必然
//! 是「能力缺失」而非编译/类型不匹配。每用例独立端口 + 独立临时库；`Proc` 额外暴露库文件路径。

#[path = "harness.rs"]
mod harness;

use harness::{Resp, Server, TEST_ADMIN_TOKEN, TEST_CLIENT_TOKEN};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const TRACES: &str = "/api/v1/traces";
// ── HTTP 夹具抽象：harness `Server`（不暴露库路径）/ 本文件 `Proc`（暴露库路径）─────────
trait Api {
    fn send_api(&self, method: reqwest::Method, path: &str, headers: &[(&str, &str)], body: Option<&Value>) -> Resp;
    fn get_api(&self, path: &str) -> Resp {
        self.send_api(reqwest::Method::GET, path, &[("x-admin-token", TEST_ADMIN_TOKEN)], None)
    }
    fn post_api(&self, body: &Value) -> Resp {
        let r = self.send_api(reqwest::Method::POST, TRACES, &[("x-client-token", TEST_CLIENT_TOKEN)], Some(body));
        assert!(r.status == 200 || r.status == 201, "[C5] POST {TRACES} 应 2xx：{}", r.describe());
        r
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

fn post_turns<A: Api>(api: &A, session: &str, turns: Vec<Value>) -> Resp {
    api.post_api(&json!({"session_id": session, "project": "job_copilot", "environment": "development", "release": "dev", "turns": turns}))
}

fn list<A: Api>(api: &A, query: &str) -> Vec<Value> {
    let r = api.get_api(&format!("{TRACES}?{query}"));
    assert_eq!(r.status, 200, "[C5] GET list 应 200：{}", r.describe());
    r.json.as_array().cloned().unwrap_or_else(|| panic!("[C5] GET list 必须返回数组：{}", r.raw))
}

/// 该 session 的聚合行（单行真相）。
fn row<A: Api>(api: &A, session: &str) -> Value {
    let rows = list(api, &format!("session_id={session}"));
    assert_eq!(rows.len(), 1, "[C5] session {session} 应仅一行聚合记录：{} 行", rows.len());
    rows[0].clone()
}
fn turn_of(rec: &Value, turn_id: &str) -> Value {
    rec["payload"]["turns"].as_array().cloned().unwrap_or_default().into_iter().find(|t| t["turn_id"] == turn_id)
        .unwrap_or_else(|| panic!("[C9] payload.turns 缺少 turn `{turn_id}`：{rec}"))
}

/// 上报单 turn 后读回该 turn。
fn ingest<A: Api>(api: &A, session: &str, payload: Value) -> Value {
    post_turns(api, session, vec![payload]);
    turn_of(&row(api, session), "t1")
}

fn uniq(prefix: &str) -> String {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    format!("{prefix}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst))
}

fn metric_sum(metrics: &str, name: &str) -> f64 {
    metrics.lines()
        .filter(|l| l.starts_with(name) && matches!(l.as_bytes().get(name.len()).copied(), Some(b'{') | Some(b' ')))
        .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok()).sum()
}

/// 截断响应体：失败信息可读，且避免 105 行列表刷屏污染红相收据。
fn brief(raw: &str) -> String {
    if raw.chars().count() > 240 { format!("{}…", raw.chars().take(240).collect::<String>()) } else { raw.to_string() }
}

// ── 可直连库文件的被测进程（C9.8 / C9.10）────────────────────────────────────
struct Proc {
    child: Child,
    base: String,
    dir: PathBuf,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("契约测试包必须位于仓库 tests/ 目录").to_path_buf()
}

fn binary() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let root = repo_root();
        let st = Command::new("cargo").args(["build", "--offline", "-p", "pony-sentry-server"]).current_dir(&root).status().expect("无法 cargo build");
        assert!(st.success(), "构建 pony-sentry-server 失败");
        root.join("target/debug/pony-sentry-server")
    }).clone()
}

fn unique_dir(prefix: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("pony-sentry-c9-{prefix}-{}-{}-{nanos}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).expect("无法创建临时目录");
    dir
}

impl Proc {
    /// 以指定库文件启动（库可已存在：启动即迁移）。
    fn start(dir: &Path, db: &Path, extra_env: &[(&str, &str)]) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("无法分配端口").port();
        let mut cmd = Command::new(binary());
        cmd.current_dir(dir).env("PORT", port.to_string())
            .env("DATABASE_URL", format!("sqlite:{}?mode=rwc", db.display()))
            .env("CLIENT_TOKEN", TEST_CLIENT_TOKEN).env("ADMIN_TOKEN", TEST_ADMIN_TOKEN)
            .env("RUST_LOG", "info").env_remove("RETENTION_DISABLED").env_remove("RETENTION_DRY_RUN")
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

/// python3 直连库文件执行片段（argv[1]=库路径，其余为显式参数）。
fn py(db: &Path, code: &str, args: &[&str]) -> String {
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(code).arg(db);
    for arg in args {
        cmd.arg(arg);
    }
    let out = cmd.output().unwrap_or_else(|err| panic!("无法执行 python3：{err}"));
    assert!(out.status.success(), "python3 片段失败（库 {}）：\n{}", db.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn py_scalar(db: &Path, sql: &str) -> String {
    py(db, "import sqlite3,sys\ncon=sqlite3.connect(sys.argv[1],timeout=20)\nprint(con.execute(sys.argv[2]).fetchone()[0])\n", &[sql])
}


// ── C9.1 合并语义 v2：终态覆盖、禁止回退、tool_calls 按 id 合并、新 turn 追加 ────────
#[test]
fn c9_1_terminal_status_overrides_and_never_regresses() {
    let server = Server::start();
    let session = uniq("c9-1a");
    post_turns(&server, &session, vec![turn("t1", "running")]);
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "failed", "attempt_index": 2, "error": {"type": "TimeoutError", "message": "boom"}})]);
    let rec = row(&server, &session);
    let merged = turn_of(&rec, "t1");
    assert_eq!(merged["status"], "failed", "[C9.1] running→failed 必须由终态覆盖：{rec}");
    assert_eq!(merged["error"]["message"], "boom", "[C9.1] 终态覆盖必须保留迟到补全的 error：{merged}");
    let session = uniq("c9-1b");
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "failed", "error": {"message": "keep-me"}})]);
    post_turns(&server, &session, vec![turn("t1", "running")]);
    let rec = row(&server, &session);
    let merged = turn_of(&rec, "t1");
    assert_eq!(merged["status"], "failed", "[C9.1] 旧终态 + 新非终态禁止回退：{rec}");
    assert_eq!(merged["error"]["message"], "keep-me", "[C9.1] 非终态上报不得清空既有 error：{merged}");
}

#[test]
fn c9_1_tool_calls_merged_by_id_and_new_turn_appended() {
    let server = Server::start();
    let session = uniq("c9-1c");
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "running", "tool_calls": [
        {"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "ok"},
        {"tool_call_id": "c2", "tool_name": "fetch", "status": "success", "output": "ok"}]})]);
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "failed", "tool_calls": [
        {"tool_call_id": "c1", "tool_name": "parse", "status": "error", "error": {"message": "解析失败"}},
        {"tool_call_id": "c3", "tool_name": "retry", "status": "success", "output": "ok"}]})]);
    let rec = row(&server, &session);
    let merged = turn_of(&rec, "t1");
    let calls = merged["tool_calls"].as_array().cloned().unwrap_or_default();
    assert_eq!(calls.len(), 3, "[C9.1] tool_calls 必须按 id 合并（旧保留 + 新 id 追加）：{merged}");
    assert!(calls.iter().any(|c| c["tool_call_id"] == "c2"), "[C9.1] 旧 tool_call c2 不得丢失：{merged}");
    let c1 = calls.iter().find(|c| c["tool_call_id"] == "c1").unwrap_or_else(|| panic!("[C9.1] 同 id c1 须合并为单条：{merged}"));
    assert_eq!(c1["status"], "error", "[C9.1] 同 id 的新状态必须覆盖旧条目：{c1}");
    post_turns(&server, &session, vec![turn("t2", "succeeded")]);
    let rec = row(&server, &session);
    let turns = rec["payload"]["turns"].as_array().cloned().unwrap_or_default();
    assert_eq!(turns.len(), 2, "[C9.1] 新 turn 必须追加：{rec}");
    assert_eq!(turns[0]["turn_id"], "t1", "[C9.1] turns 必须按 seq 升序：{rec}");
    assert_eq!(turns[1]["turn_id"], "t2", "[C9.1] turns 必须按 seq 升序：{rec}");
}

// ── C9.2 并发：同 session 20 并发 × 2 turn 全部入库 ──────────────────────────────
#[test]
fn c9_2_concurrent_ingest_loses_no_turn() {
    let server = Server::start();
    let session = uniq("c9-2");
    let base_url = server.base_url.clone();
    let mut handles = Vec::new();
    for worker in 0..20u32 {
        let (base_url, session) = (base_url.clone(), session.clone());
        handles.push(std::thread::spawn(move || {
            let client = harness::http_client();
            for slot in 0..2u32 {
                let body = json!({"session_id": session, "project": "job_copilot", "environment": "development", "release": "dev",
                    "turns": [{"turn_id": format!("r{worker}-t{slot}"), "status": "succeeded"}]});
                let resp = client.post(format!("{base_url}{TRACES}")).header("x-client-token", TEST_CLIENT_TOKEN).json(&body).send().expect("并发 POST 失败");
                let status = resp.status().as_u16();
                assert!(status == 200 || status == 201, "[C9.2] 并发上报 {worker}/{slot} 应 2xx，实际 {status}：{}", resp.text().unwrap_or_default());
            }
        }));
    }
    for handle in handles {
        handle.join().expect("并发线程 panic");
    }
    let rec = row(&server, &session);
    assert_eq!(rec["turns_count"], 40, "[C9.2] 20 并发 × 2 turn 必须全部入库（turns_count=40）：{rec}");
    assert_eq!(rec["payload"]["turns"].as_array().map(|a| a.len()), Some(40), "[C9.2] payload.turns 必须含 40 条：{rec}");
}

// ── C9.3 响应一致性：id 可 GET、total_* == DB == turns 求和 ─────────────────────
#[test]
fn c9_3_post_id_gettable_and_totals_match_turns_sum() {
    let server = Server::start();
    let session = uniq("c9-3");
    // 客户端故意直传错误汇总值：服务端必须以 turns 求和为准（单一真相）
    let created = server.post_api(&json!({"session_id": session, "project": "job_copilot", "environment": "development", "release": "dev",
        "total_input_tokens": 99999, "total_output_tokens": 99999, "total_duration_ms": 99999,
        "turns": [
            {"turn_id": "t1", "status": "succeeded", "input_tokens": 10, "output_tokens": 5, "duration_ms": 100},
            {"turn_id": "t2", "status": "failed", "input_tokens": 20, "output_tokens": 7, "duration_ms": 200}]}));
    let id = created.json["id"].as_str().unwrap_or_else(|| panic!("[C9.3] POST 响应必须含字符串 id：{}", brief(&created.raw))).to_string();
    let fetched = server.get_api(&format!("{TRACES}/{id}"));
    assert_eq!(fetched.status, 200, "[C9.3] POST 响应的 id 必须可直接 GET 200（库内持久化行）：{}", fetched.describe());
    for (field, expected) in [("total_input_tokens", 30), ("total_output_tokens", 12), ("total_duration_ms", 300)] {
        assert_eq!(created.json[field].as_i64(), Some(expected), "[C9.3] POST 响应 {field} 必须等于 turns 求和 {expected}（拒绝客户端直传值）");
        assert_eq!(fetched.json[field].as_i64(), Some(expected), "[C9.3] GET 响应 {field} 必须等于 turns 求和 {expected}：{}", brief(&fetched.raw));
        let stored = row(&server, &session);
        assert_eq!(stored[field].as_i64(), Some(expected), "[C9.3] 库内 {field} 必须等于 turns 求和 {expected}：{stored}");
    }
}

// ── C9.4 参数校验：400 不回显 DB 原文 + limit clamp ──────────────────────────────
#[test]
fn c9_4_invalid_query_params_rejected_without_db_leak() {
    let server = Server::start();
    for i in 0..105 {
        post_turns(&server, &uniq(&format!("c9-4-{i}")), vec![turn("t1", "succeeded")]);
    }
    for query in ["limit=-5", "limit=abc", "offset=-1"] {
        let resp = server.get_api(&format!("{TRACES}?{query}"));
        assert_eq!(resp.status, 400, "[C9.4] `{query}` 必须 400（不得静默接受/降级）：{}", brief(&resp.raw));
        for leak in ["Database error", "SQLITE", "sqlx", "syntax error", "no such"] {
            assert!(!resp.raw.contains(leak), "[C9.4] 4xx 响应体不得回显数据库错误原文（命中 `{leak}`）：{}", brief(&resp.raw));
        }
    }
    let resp = server.get_api(&format!("{TRACES}?limit=100000"));
    assert_eq!(resp.status, 200, "[C9.4] limit=100000 必须 200（clamp 而非拒绝）：{}", brief(&resp.raw));
    let rows = resp.json.as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(rows, 100, "[C9.4] limit=100000 必须 clamp 到 100（库内 105 行否则取满），实际 {rows} 行");
}

// ── C9.5 诊断标注三标志 ────────────────────────────────────────────────────────
#[test]
fn c9_5_diagnostics_three_flags() {
    let server = Server::start();
    let session = uniq("c9-5a");
    let d = ingest(&server, &session, json!({"turn_id": "t1", "status": "failed", "tool_calls": [
        {"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "ok"}]}));
    assert_eq!(d["diagnostics"]["error_contract"], "missing", "[C9.5] failed 且无错误明细须标注 error_contract=missing：{d}");
    let session = uniq("c9-5b");
    let d = ingest(&server, &session, json!({"turn_id": "t1", "status": "completed", "tool_calls": [
        {"tool_call_id": "c1", "tool_name": "format_jd_to_json", "status": "success", "output": "职位解析失败（原因: 模型返回空结果）"}]}));
    assert_eq!(d["diagnostics"]["status_conflict"], true, "[C9.5] status=success + 失败文案须标注 status_conflict=true：{d}");
    let session = uniq("c9-5c");
    let d = ingest(&server, &session, json!({"turn_id": "t1", "status": "completed", "tool_calls": [
        {"tool_call_id": "c1", "tool_name": "format_jd_to_json", "status": "error"}]}));
    assert_eq!(d["diagnostics"]["error_missing"], true, "[C9.5] tool_call status=error 且无 error 须标注 error_missing=true：{d}");
}

// ── C9.6 字段真源：extra 保留、payload.eval_status 移除、session_id_source ────────
#[test]
fn c9_6_extra_retained_eval_status_absent_session_id_source() {
    let server = Server::start();
    let session = uniq("c9-6");
    server.post_api(&json!({"session_id": session, "environment": "development", "release": "dev", "eval_status": "triage_good",
        "extra": {"project_path": "a", "keep": "x"}, "turns": [turn("t1", "succeeded")]}));
    server.post_api(&json!({"session_id": session.clone(), "environment": "development", "release": "dev",
        "extra": {"project_path": "b"}, "turns": [turn("t1", "succeeded")]}));
    let rec = row(&server, &session);
    assert_eq!(rec["payload"]["extra"]["project_path"], "b", "[C9.6] extra 非空新值必须覆盖旧值：{rec}");
    assert_eq!(rec["payload"]["extra"]["keep"], "x", "[C9.6] 二次上报缺省的 extra 字段不得置 null（保留旧值）：{rec}");
    assert!(rec["payload"].get("eval_status").is_none(), "[C9.6] payload.eval_status 必须移除（列是唯一真源）：{rec}");
    assert_eq!(rec["eval_status"], "unreviewed", "[C9.6-amend.v1.4] ingest 必须忽略客户端 eval_status（防写 token 自评），评审结论只能走 admin PATCH：{rec}");
    assert_eq!(rec["payload"]["session_id_source"], "session_id", "[C9.6] 主键来源须回写 payload.session_id_source：{rec}");
    let thread = uniq("c9-6-thread");
    server.post_api(&json!({"thread_id": thread.clone(), "environment": "development", "release": "dev", "turns": [turn("t1", "succeeded")]}));
    assert_eq!(row(&server, &thread)["payload"]["session_id_source"], "thread_id", "[C9.6] thread_id 回退须标注 session_id_source=thread_id");
}

// ── C9.7 note PATCH 落库回显；turn_status/turns_count 随写入更新 ─────────────────
#[test]
fn c9_7_note_patch_and_aggregates_advance() {
    let server = Server::start();
    let session = uniq("c9-7");
    let created = post_turns(&server, &session, vec![turn("t1", "succeeded")]);
    let id = created.json["id"].as_str().unwrap_or_else(|| panic!("[C9.7] POST 响应必须含字符串 id：{}", brief(&created.raw))).to_string();
    let first = row(&server, &session);
    assert_eq!(first["turns_count"], 1, "[C9.7] 首次写入后 turns_count 必须为 1：{first}");
    assert_eq!(first["turn_status"], "succeeded", "[C9.7] turn_status 必须为该 session 最后 turn 的 status：{first}");
    let patched = server.patch_admin(&format!("{TRACES}/{id}"), &json!({"eval_status": "triage_good", "note": "人工复核：失败原因已确认"}));
    assert_eq!(patched.status, 200, "[C9.7] PATCH note 应 200：{}", patched.describe());
    let detail = server.get_api(&format!("{TRACES}/{id}"));
    assert_eq!(detail.json["note"], "人工复核：失败原因已确认", "[C9.7] PATCH 后 note 须落库并在 GET 详情回显：{}", brief(&detail.raw));
    let listed = row(&server, &session);
    assert_eq!(listed["note"], "人工复核：失败原因已确认", "[C9.7] note 必须可从 GET list 读回：{listed}");
    post_turns(&server, &session, vec![turn("t2", "failed")]);
    let advanced = row(&server, &session);
    assert_eq!(advanced["turns_count"], 2, "[C9.7] 新 turn 写入后 turns_count 须推进到 2：{advanced}");
    assert_eq!(advanced["turn_status"], "failed", "[C9.7] turn_status 须随最后 turn 更新为 failed：{advanced}");
}

// ── C9.8 保留期按 COALESCE(updated_at, created_at) 判定 ─────────────────────────
// 触发：RETENTION_DAYS_TRACES=1，服务启动 30s 后首轮清理；python3 改写两行时间戳后轮询观测。
#[test]
fn c9_8_retention_judges_by_updated_at() {
    let dir = unique_dir("9-8");
    let db = dir.join("retention.db");
    let proc = Proc::start(&dir, &db, &[("RETENTION_DAYS_TRACES", "1")]);
    post_turns(&proc, "ret-stale", vec![turn("t1", "succeeded")]);
    post_turns(&proc, "ret-fresh", vec![turn("t1", "succeeded")]);
    let changed = py(&db, "import datetime, sqlite3, sys\n\
        now = datetime.datetime.now(datetime.timezone.utc)\n\
        def fmt(dt): return dt.strftime('%Y-%m-%dT%H:%M:%S.%f') + '000+00:00'\n\
        old, new = fmt(now - datetime.timedelta(days=10)), fmt(now)\n\
        con = sqlite3.connect(sys.argv[1], timeout=20)\n\
        con.execute(\"UPDATE traces SET created_at = ?, updated_at = ? WHERE session_id = 'ret-stale'\", (old, old))\n\
        con.execute(\"UPDATE traces SET created_at = ?, updated_at = ? WHERE session_id = 'ret-fresh'\", (old, new))\n\
        con.commit()\n\
        print(con.total_changes)\n", &[]);
    assert_eq!(changed, "2", "[C9.8] 前置条件失败：两行时间戳改写未生效（total_changes={changed}）");
    let deadline = Instant::now() + Duration::from_secs(75);
    while !list(&proc, "session_id=ret-stale").is_empty() {
        assert!(Instant::now() < deadline, "[C9.8] retention 未在 75s 内执行（契约首轮延迟 30s）");
        std::thread::sleep(Duration::from_secs(1));
    }
    let survivors = list(&proc, "session_id=ret-fresh");
    assert_eq!(survivors.len(), 1, "[C9.8] 须按 COALESCE(updated_at, created_at) 判定：updated_at 新的行被误删（created_at 旧）");
}

// ── C9.9 指标：/metrics 含 C7 四个指标名且计数随上报增长 ────────────────────────
#[test]
fn c9_9_metrics_expose_four_trace_counters() {
    let server = Server::start();
    let session = uniq("c9-9");
    post_turns(&server, &session, vec![json!({"turn_id": "t1", "status": "failed", "tool_calls": [
        {"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "解析失败"}]})]);
    let rejected = server.send(reqwest::Method::POST, TRACES, &[("x-client-token", TEST_CLIENT_TOKEN)], Some(&json!({"environment": "development", "release": "dev"})));
    assert_eq!(rejected.status, 400, "[C5] 缺失 session_id/thread_id 必须 400：{}", rejected.describe());
    let metrics = server.get_anon("/metrics");
    assert_eq!(metrics.status, 200, "[C9.9] /metrics 应 200：{}", metrics.describe());
    let names = [
        ("pony_sentry_traces_total", "traces_total 须随上报增长"),
        ("pony_sentry_trace_turns_total", "trace_turns_total 须随上报增长"),
        ("pony_sentry_trace_status_conflicts_total", "status_conflicts_total 须随矛盾上报增长"),
        ("pony_sentry_trace_ingest_rejected_total", "ingest_rejected_total 须随非法上报增长"),
    ];
    for (name, clause) in names {
        assert!(metrics.raw.contains(name), "[C9.9] /metrics 必须包含指标 `{name}`：\n{}", brief(&metrics.raw));
        assert!(metric_sum(&metrics.raw, name) >= 1.0, "[C9.9] {clause}（`{name}` 合计 {}）", metric_sum(&metrics.raw, name));
    }
}

const LEGACY_DB_PY: &str = r#"
import json, os, sqlite3, sys
db, root = sys.argv[1], sys.argv[2]
con = sqlite3.connect(db, timeout=20)
for name in ('0001_init.sql', '0002_add_project.sql', '0003_add_traces.sql', '0004_add_issue_dimensions.sql'):
    for chunk in open(os.path.join(root, 'migrations', name), encoding='utf-8').read().split(';'):
        stmt = '\n'.join(l for l in chunk.splitlines() if not l.strip().startswith('--')).strip()
        if not stmt:
            continue
        try:
            con.execute(stmt)
        except sqlite3.OperationalError as exc:
            if 'duplicate column' not in str(exc) and 'already exists' not in str(exc):
                raise
con.execute(
    "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project, eval_status, payload, "
    "total_input_tokens, total_output_tokens, total_duration_ms, reported_at, created_at, updated_at) "
    "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    ("legacy-row-1", "legacy-sess-1", "run-legacy", None, "development", "dev", "job_copilot", "unreviewed",
     json.dumps({"turns": [
         {"turn_id": "lt-1", "status": "succeeded", "input_tokens": 7, "output_tokens": 3, "duration_ms": 11},
         {"turn_id": "lt-2", "status": "failed", "input_tokens": 5, "output_tokens": 2, "duration_ms": 13}],
         "extra": {"legacy": True}, "session_id_source": "session_id"}), 12, 5, 24,
     "2026-10-01T00:00:00Z", "2026-10-01T00:00:00Z", "2026-10-01T00:00:00Z"),
)
con.commit()
print("ok")
"#;

#[test]
fn c9_10_migration_backfills_trace_turns_from_legacy_payload() {
    let dir = unique_dir("9-10");
    let db = dir.join("legacy.db");
    let built = py(&db, LEGACY_DB_PY, &[repo_root().to_str().expect("仓库根路径必须为 UTF-8")]);
    assert_eq!(built, "ok", "[C9.10] 旧形态库构造失败：{built}");
    let proc = Proc::start(&dir, &db, &[]); // 启动即迁移（0005 + 回填）
    let rec = row(&proc, "legacy-sess-1");
    assert_eq!(rec["turns_count"], 2, "[C9.10] 迁移回填后 turns_count 必须等于 payload.turns 条数（2）：{}", brief(&rec.to_string()));
    assert_eq!(rec["turn_status"], "failed", "[C9.10] 迁移回填 turn_status 必须取最后一个 turn 的 status：{rec}");
    let rows = py_scalar(&db, "SELECT COUNT(*) FROM trace_turns WHERE session_id='legacy-sess-1'");
    assert_eq!(rows, "2", "[C9.10] trace_turns 必须回填 2 行（实际 {rows}）");
    let seqs = py_scalar(&db, "SELECT GROUP_CONCAT(turn_id, ',') FROM (SELECT turn_id FROM trace_turns WHERE session_id='legacy-sess-1' ORDER BY seq)");
    assert_eq!(seqs, "lt-1,lt-2", "[C9.10] trace_turns 的 seq 必须按原 turns 顺序生成（实际 {seqs}）");
}
