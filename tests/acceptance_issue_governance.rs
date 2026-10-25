//! issue 治理（归档 / 列表语义 / 事件分页 / 统计 / 治理指标）黑盒验收测试（红相冻结）。
//!
//! 断言唯一依据：`.dev-team/contract-issue-governance.md`（**FROZEN v1.2**，A1-A11 + C1/C2.5/C4.3 v1.1·v1.2 amend）。
//! 全黑盒：只经进程级接缝（`DATABASE_URL` / `PORT` / `CLIENT_TOKEN` / `ADMIN_TOKEN`
//! 以及本契约新增的 `PONY_ISSUE_ARCHIVE_*`）+ HTTP 驱动 `pony-sentry-server` 真实进程；
//! 每用例独立临时 SQLite 文件库 + 独立端口，零 crate 内部类型依赖。
//!
//! 事实（契约 v1.1 `B-12` / 侦察 `F23`）：fingerprint **v3 不含 project**，同 project 内
//! 不同 message 也会合并到同一 issue；需要多条独立 issue 的造数必须同时变更
//! `error_type` 与栈帧（filename/function），见 `seed_distinct`。
//!
//! 红相语义：被测能力缺失（新接口 404 / 响应缺字段）必须表现为**断言失败**，
//! 不得用 `#[ignore]`、不得吞错、不得写同义反复断言。

#[path = "harness.rs"]
mod harness;

use harness::{full_v1_ingest_payload, Resp, Server, TEST_ADMIN_TOKEN, TEST_CLIENT_TOKEN};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const ISSUES: &str = "/api/v1/issues";
const STATS: &str = "/api/v1/stats";
const INGEST_STATS: &str = "/api/v1/ingest/stats";

// ── 通用断言 / 读取 helper ───────────────────────────────────────────────────

fn header(resp: &Resp, name: &str) -> Option<String> {
    resp.headers
        .get(name)
        .map(|v| v.to_str().unwrap_or_default().to_string())
}

fn assert_status(resp: &Resp, expected: u16, clause: &str) {
    assert_eq!(
        resp.status,
        expected,
        "[{clause}] 期望 HTTP {expected}，实际 {}",
        resp.describe()
    );
    // 全局 5xx 卫生守卫（契约 v1.1 C2.3 / B-6）：任何 5xx 体都不得回吐 DB 原文。
    if resp.status >= 500 {
        for needle in ["SQLSTATE", "sqlite", "SQLite", "panicked", "SELECT ", "INSERT ", "UPDATE "] {
            assert!(
                !resp.raw.contains(needle),
                "[{clause}] 5xx 响应体泄漏数据库/内部实现细节（含 `{needle}`）：{}",
                resp.describe()
            );
        }
    }
}

/// 契约 v1.1 C2.3 / B-6：新接口 5xx 体必须**恰为** `Internal error`。
fn assert_internal_error_body(resp: &Resp, clause: &str) {
    if resp.status >= 500 {
        assert_eq!(
            resp.raw.trim(),
            "Internal error",
            "[{clause}] 5xx 响应体必须固定为 `Internal error`（B-6）：{}",
            resp.describe()
        );
    }
}

/// base64url（无 padding）编码／解码：仅用于断言 C4.2 v1.1 游标载荷格式。
const B64URL_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(input: &[u8]) -> String {
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64URL_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64URL_ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits: u32 = 0;
    let mut nbits: u32 = 0;
    let mut out = Vec::new();
    for ch in text.bytes() {
        let value = B64URL_ALPHABET.iter().position(|c| *c == ch)? as u32;
        bits = (bits << 6) | value;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

/// `archive_restored_at`（契约 v1.1 C2.1 / A-1）：null / 缺失 / 空串统一视为「从未手动恢复」。
fn restored_at_of(value: &Value) -> Option<String> {
    value
        .get("archive_restored_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 上报一次 ingest，返回 200/201 成功响应（失败即断言失败）。
fn ingest_event(server: &Server, payload: &Value) -> Resp {
    let resp = server.post_ingest_client(payload);
    assert!(
        resp.status == 201 || resp.status == 200,
        "[Arrange] ingest 必须成功（201 created / 200 duplicate），实际 {}",
        resp.describe()
    );
    resp
}

/// 解码 keyset 游标并断言其可解析（v1.1 C4.2 格式判据的前置）。
fn decode_cursor(cursor: &str, clause: &str) -> Value {
    let bytes = b64url_decode(cursor)
        .unwrap_or_else(|| panic!("[{clause}] cursor 不是合法 base64url：{cursor}"));
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|_| panic!("[{clause}] cursor 解码后不是 UTF-8：{cursor}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("[{clause}] cursor 解码后不是 JSON（{err}）：{text}"))
}

/// 列表响应取行：`envelope=1` 时为 `{items:[...]}`，缺省为裸数组。
fn rows(resp: &Resp, clause: &str) -> Vec<Value> {
    if let Some(items) = resp.json.get("items").and_then(Value::as_array) {
        return items.clone();
    }
    resp.json.as_array().cloned().unwrap_or_else(|| {
        panic!("[{clause}] 列表响应既不是裸数组也不含 items 信封：{}", resp.describe())
    })
}

fn ids_of(list: &[Value]) -> Vec<String> {
    list.iter()
        .map(|issue| {
            issue["id"]
                .as_str()
                .unwrap_or_else(|| panic!("列表行缺少字符串 id：{issue}"))
                .to_string()
        })
        .collect()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

fn string_field(resp: &Resp, key: &str, clause: &str) -> String {
    resp.json[key]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("[{clause}] 响应缺少字符串字段 `{key}`：{}", resp.describe()))
}

fn number_field(resp: &Resp, key: &str, clause: &str) -> i64 {
    resp.json[key]
        .as_i64()
        .unwrap_or_else(|| panic!("[{clause}] 响应缺少整数字段 `{key}`：{}", resp.describe()))
}

/// 归档状态读取口径：null / 缺失 / 空串统一视为「未归档」。
fn archived_at_of(value: &Value) -> Option<String> {
    value
        .get("archived_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ── 造数 helper（全部经真实 ingest 链路，不直接写库）──────────────────────────

/// 造一条 issue：`tags.project` 决定 project 维度，message 参与指纹。
fn seed_issue(server: &Server, project: &str, message: &str, event_id: &str) -> String {
    let mut payload = full_v1_ingest_payload(event_id, "production", message);
    payload["tags"] = json!({"project": project, "alertname": "issue-governance"});
    payload["extra"] = json!({"project_path": format!("/workspace/{project}")});
    let resp = server.post_ingest_client(&payload);
    assert_status(&resp, 201, "Arrange ingest 造数必须 201 created");
    resp.str_field("issue_id")
}

/// 造一条指定状态的 issue（状态经 PATCH 真实流转）。
fn seed_issue_with_status(
    server: &Server,
    project: &str,
    message: &str,
    event_id: &str,
    status: &str,
) -> String {
    let issue_id = seed_issue(server, project, message, event_id);
    // 新建 issue 本身即 unresolved，无需（也不宜）走 PATCH 无操作跃迁。
    if status != "unresolved" {
        set_issue_status(server, &issue_id, status);
    }
    issue_id
}

fn set_issue_status(server: &Server, issue_id: &str, status: &str) {
    let resp = server.patch_admin(
        &format!("{ISSUES}/{issue_id}"),
        &json!({"status": status}),
    );
    assert_status(&resp, 200, "Arrange PATCH issue status 必须 200");
    assert_eq!(
        resp.json["status"].as_str(),
        Some(status),
        "[Arrange] PATCH 后状态必须为 {status}：{}",
        resp.describe()
    );
}

fn issue_detail(server: &Server, issue_id: &str) -> Resp {
    let resp = server.get_admin(&format!("{ISSUES}/{issue_id}"));
    assert_status(&resp, 200, "GET /api/v1/issues/:id 带 admin token");
    resp
}

fn events_of(server: &Server, issue_id: &str, query: &str) -> Resp {
    let path = if query.is_empty() {
        format!("{ISSUES}/{issue_id}/events")
    } else {
        format!("{ISSUES}/{issue_id}/events?{query}")
    };
    server.get_admin(&path)
}

fn event_rows(server: &Server, issue_id: &str) -> Vec<Value> {
    let resp = events_of(server, issue_id, "");
    assert_status(&resp, 200, "GET /api/v1/issues/:id/events 带 admin token");
    rows(&resp, "GET /api/v1/issues/:id/events")
}

/// 单条归档（A4）。路径为契约 A4 口径。
fn archive_issue(server: &Server, issue_id: &str) -> Resp {
    let resp = expect_sut_response("POST /api/v1/issues/:id/archive", || {
        server.send(
            reqwest::Method::POST,
            &format!("{ISSUES}/{issue_id}/archive"),
            &[("x-admin-token", TEST_ADMIN_TOKEN)],
            None,
        )
    });
    assert_internal_error_body(&resp, "POST /api/v1/issues/:id/archive");
    resp
}

/// 单条恢复（A4）。
fn unarchive_issue(server: &Server, issue_id: &str) -> Resp {
    let resp = expect_sut_response("POST /api/v1/issues/:id/unarchive", || {
        server.send(
            reqwest::Method::POST,
            &format!("{ISSUES}/{issue_id}/unarchive"),
            &[("x-admin-token", TEST_ADMIN_TOKEN)],
            None,
        )
    });
    assert_internal_error_body(&resp, "POST /api/v1/issues/:id/unarchive");
    resp
}

/// 批量归档（A5）：`query` 传 dry_run 等查询串（含前导 `?` 或空串）。
fn archive_batch(server: &Server, query: &str, body: &Value) -> Resp {
    let resp = expect_sut_response("POST /api/v1/issues/archive", || {
        server.send(
            reqwest::Method::POST,
            &format!("{ISSUES}/archive{query}"),
            &[("x-admin-token", TEST_ADMIN_TOKEN)],
            Some(body),
        )
    });
    assert_internal_error_body(&resp, "POST /api/v1/issues/archive");
    resp
}

fn list_issues(server: &Server, query: &str) -> Resp {
    expect_sut_response("GET /api/v1/issues", || {
        server.get_admin(&format!("{ISSUES}?{query}"))
    })
}

fn stats(server: &Server, query: &str) -> Resp {
    let resp = expect_sut_response("GET /api/v1/stats", || {
        server.get_admin(&format!("{STATS}?{query}"))
    });
    assert_internal_error_body(&resp, "GET /api/v1/stats");
    resp
}

fn ingest_stats(server: &Server, query: &str) -> Resp {
    let resp = expect_sut_response("GET /api/v1/ingest/stats", || {
        server.get_admin(&format!("{INGEST_STATS}?{query}"))
    });
    assert_internal_error_body(&resp, "GET /api/v1/ingest/stats");
    resp
}

/// 新能力接缝的统一出口。
///
/// 被测能力缺失时 handler 内会 panic（空桩 `unimplemented!`），服务端直接断开连接，
/// 客户端拿不到任何 HTTP 响应。这里把**传输层中断显式转成断言失败**：
/// 失败即红相（绝不吞错、绝不视为通过），并在信息里写明成因在 SUT 侧。
fn expect_sut_response(clause: &str, call: impl FnOnce() -> Resp) -> Resp {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(resp) => resp,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<非字符串 panic payload>".to_string());
            panic!(
                "[{clause}] 断言失败：SUT 未实现该能力 —— handler 内 panic 使连接被断开，\
                 无 HTTP 响应可供断言（传输层错误：{detail}）"
            );
        }
    }
}

// ── A 段专用读取 helper（响应头 / 信封 / 木桶 / 轮询）──────────────────────────

/// 读取十进制整数响应头（小写名，见 C3.3）。
fn header_int(resp: &Resp, name: &str, clause: &str) -> i64 {
    let raw = header(resp, name)
        .unwrap_or_else(|| panic!("[{clause}] 缺少响应头 `{name}`：{}", resp.describe()));
    raw.trim()
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("[{clause}] 响应头 `{name}` 非十进制整数：{raw}"))
}

fn total_count(resp: &Resp, clause: &str) -> i64 {
    header_int(resp, "x-total-count", clause)
}

/// JSON 值必须为整数（stats / ingest-stats 的木桶与总量字段）。
fn vnum(value: &Value, clause: &str) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("[{clause}] 期望整数，实际 `{value}`"))
}

/// 木桶（`{string:int}`）求和。
fn bucket_sum(map: &Value, clause: &str) -> i64 {
    let obj = map
        .as_object()
        .unwrap_or_else(|| panic!("[{clause}] 期望 `{{string:int}}` 木桶对象，实际 `{map}`"));
    obj.values().map(|v| vnum(v, clause)).sum()
}

/// 事件行标识：优先 `id`，回落 `event_id`（C4.2 游标载荷字段名）。
fn row_key(row: &Value) -> String {
    ["id", "event_id"]
        .iter()
        .find_map(|key| row.get(*key).and_then(Value::as_str))
        .unwrap_or_else(|| panic!("事件行缺少 id/event_id：{row}"))
        .to_string()
}

fn keys_of(list: &[Value]) -> Vec<String> {
    list.iter().map(row_key).collect()
}

/// day 桶键必须是可解析的 `YYYY-MM-DD`（C5 series 判据）。
fn is_day_bucket(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// 显式条件轮询（禁止盲等）：条件在超时内成立返回 true。
fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn archived_now(server: &Server, issue_id: &str) -> Option<String> {
    let resp = server.get_admin(&format!("{ISSUES}/{issue_id}"));
    assert_status(&resp, 200, "轮询 GET /api/v1/issues/:id");
    archived_at_of(&resp.json)
}

/// 造一条**指纹唯一**的 issue：`error_type` 与栈帧（filename/function）都随 `tag` 变化，
/// 因此不会与同 project 的其他 seed 合并（指纹口径不由 message 决定，实测确认）。
fn seed_distinct(server: &Server, project: &str, tag: &str, event_id: &str) -> String {
    let mut payload = full_v1_ingest_payload(event_id, "production", &format!("governance {tag}"));
    let error_type = format!("Governance{tag}Error");
    payload["error_type"] = json!(error_type);
    payload["exception"] = json!({
        "error_type": error_type,
        "value": format!("governance {tag}"),
        "stacktrace": [{
            "filename": format!("fixtures/{tag}.py"),
            "function": format!("fn_{tag}"),
            "lineno": 1,
            "colno": 1,
            "in_app": true
        }]
    });
    payload["tags"] = json!({"project": project, "alertname": "issue-governance"});
    payload["extra"] = json!({"project_path": format!("/workspace/{project}")});
    let resp = server.post_ingest_client(&payload);
    assert_status(&resp, 201, "Arrange ingest 造数（指纹唯一）必须 201 created");
    resp.str_field("issue_id")
}

fn seed_distinct_with_status(
    server: &Server,
    project: &str,
    tag: &str,
    event_id: &str,
    status: &str,
) -> String {
    let issue_id = seed_distinct(server, project, tag, event_id);
    if status != "unresolved" {
        set_issue_status(server, &issue_id, status);
    }
    issue_id
}

/// 追加一条同指纹事件（不同 event_id）→ 同一 issue，count / 事件行 +1。
fn append_event(server: &Server, project: &str, message: &str, event_id: &str) -> String {
    let mut payload = full_v1_ingest_payload(event_id, "production", message);
    payload["tags"] = json!({"project": project, "alertname": "issue-governance"});
    payload["extra"] = json!({"project_path": format!("/workspace/{project}")});
    let resp = server.post_ingest_client(&payload);
    assert!(
        resp.status == 201 || resp.status == 200,
        "[Arrange] 追加事件必须成功（201 created / 200 duplicate），实际 {}",
        resp.describe()
    );
    resp.str_field("issue_id")
}

/// A9 治理指标快照。
struct Totals {
    received: i64,
    created: i64,
    duplicate: i64,
    accepted: i64,
    suppressed_ignored: i64,
}

fn ingest_totals(server: &Server) -> Totals {
    let resp = ingest_stats(server, "window=24h");
    assert_status(&resp, 200, "A9 GET /api/v1/ingest/stats?window=24h");
    let totals = &resp.json["totals"];
    Totals {
        received: vnum(&totals["received"], "A9 totals.received"),
        created: vnum(&totals["created"], "A9 totals.created"),
        duplicate: vnum(&totals["duplicate"], "A9 totals.duplicate"),
        accepted: vnum(&totals["accepted"], "A9 totals.accepted"),
        suppressed_ignored: vnum(&totals["suppressed_ignored"], "A9 totals.suppressed_ignored"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// A1 — 列表过滤：project + status=unresolved 只返回 unresolved，无 ignored
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a1_list_filter_project_and_status() {
    // Arrange：p1 两条 unresolved + 一条 ignored + 一条 resolved；p2 一条 ignored（不得串味）
    let server = Server::start();
    let u1 = seed_issue_with_status(&server, "p1", "A1 unresolved one", "evt-a1-u1", "unresolved");
    let u2 = seed_distinct(&server, "p1", "a1u2", "evt-a1-u2");
    let _ignored = seed_distinct_with_status(&server, "p1", "a1i1", "evt-a1-i1", "ignored");
    let _resolved = seed_distinct_with_status(&server, "p1", "a1r1", "evt-a1-r1", "resolved");
    let _other_project = seed_distinct_with_status(&server, "p2", "a1p2", "evt-a1-i2", "ignored");

    // Act
    let resp = list_issues(&server, "project=p1&status=unresolved");

    // Assert：只有 unresolved，且命中集合恰为 p1 的两条
    assert_status(&resp, 200, "A1 GET /api/v1/issues?project=p1&status=unresolved");
    let list = rows(&resp, "A1");
    for issue in &list {
        assert_eq!(
            issue["status"].as_str(),
            Some("unresolved"),
            "[A1] 过滤后不得出现非 unresolved 项：{issue}"
        );
    }
    assert!(
        list.iter().all(|issue| issue["status"].as_str() != Some("ignored")),
        "[A1] 结果中不得含 ignored 项：{}",
        resp.describe()
    );
    assert_eq!(
        sorted(ids_of(&list)),
        sorted(vec![u1.clone(), u2.clone()]),
        "[A1] 命中集合必须恰为 p1 的两条 unresolved：{}",
        resp.describe()
    );

    // 交叉验证：status=ignored 只返回 ignored
    let ignored_only = list_issues(&server, "project=p1&status=ignored");
    assert_status(&ignored_only, 200, "A1 status=ignored");
    let ignored_rows = rows(&ignored_only, "A1 status=ignored");
    assert_eq!(ignored_rows.len(), 1, "[A1] p1 恰有 1 条 ignored：{}", ignored_only.describe());
    assert!(
        ignored_rows.iter().all(|issue| issue["status"].as_str() == Some("ignored")),
        "[A1] status=ignored 结果必须全为 ignored：{}",
        ignored_only.describe()
    );

    // 负向边界：非法 status 必须 400（C3.2 修正 F5 静默忽略）
    assert_status(
        &list_issues(&server, "project=p1&status=bogus"),
        400,
        "C3.2 非法 status → 400 Bad request: invalid status",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A2 — total 语义：X-Total-Count 稳定 + envelope=1 结构 + X-Project-Filter
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a2_list_total_semantics_and_envelope() {
    // Arrange：p1 三条 + p2 一条（用于证明 total 是「同过滤条件」而非全库）
    let server = Server::start();
    let _p1a = seed_distinct(&server, "p1", "a2a", "evt-a2-1");
    let _p1b = seed_distinct(&server, "p1", "a2b", "evt-a2-2");
    let _p1c = seed_distinct(&server, "p1", "a2c", "evt-a2-3");
    let _p2 = seed_distinct(&server, "p2", "a2d", "evt-a2-4");

    // Act 1：limit=1
    let narrow = list_issues(&server, "project=p1&limit=1");
    assert_status(&narrow, 200, "A2 limit=1");
    assert_eq!(rows(&narrow, "A2 limit=1").len(), 1, "[A2] limit=1 必须恰返回 1 行");
    assert_eq!(
        total_count(&narrow, "A2"),
        3,
        "[A2] X-Total-Count 必须是同过滤条件总数（project=p1 → 3），不受 limit 影响：{}",
        narrow.describe()
    );
    assert_eq!(header_int(&narrow, "x-limit", "A2"), 1, "[A2/C3.3] X-Limit 必须回显生效 limit");
    assert_eq!(header_int(&narrow, "x-offset", "A2"), 0, "[A2/C3.3] X-Offset 必须回显生效 offset");
    assert_eq!(
        header(&narrow, "x-has-more").as_deref(),
        Some("true"),
        "[A2/C3.3] offset+len(1) < total(3) → X-Has-More: true：{}",
        narrow.describe()
    );
    assert_eq!(
        header(&narrow, "x-archived-mode").as_deref(),
        Some("exclude"),
        "[A2/C3.3] X-Archived-Mode 缺省必须回显 exclude"
    );
    assert_eq!(
        header(&narrow, "x-project-filter").as_deref(),
        Some("p1"),
        "[A2/C3.3] 给定 project 时 X-Project-Filter 必须回显原值"
    );

    // Act 2：limit=100（total 不随 limit 变化）
    let wide = list_issues(&server, "project=p1&limit=100");
    assert_status(&wide, 200, "A2 limit=100");
    assert_eq!(rows(&wide, "A2 limit=100").len(), 3, "[A2] project=p1 必须 3 行");
    assert_eq!(
        total_count(&wide, "A2"),
        total_count(&narrow, "A2"),
        "[A2] limit=1 与 limit=100 的 X-Total-Count 必须相等"
    );
    assert_eq!(
        header(&wide, "x-has-more").as_deref(),
        Some("false"),
        "[A2/C3.3] offset+len(3) == total(3) → X-Has-More: false"
    );

    // Act 3：缺省 project
    let all = list_issues(&server, "limit=50");
    assert_status(&all, 200, "A2 缺省 project");
    assert_eq!(
        header(&all, "x-project-filter").as_deref(),
        Some("all"),
        "[A2] 缺省 project 时 X-Project-Filter 必须为 all：{}",
        all.describe()
    );
    assert_eq!(total_count(&all, "A2"), 4, "[A2] 缺省 project 时 total 必须是全项目 4");

    // Act 4：envelope=1 结构（C3.4）
    let env = list_issues(&server, "project=p1&envelope=1&limit=2");
    assert_status(&env, 200, "A2 envelope=1");
    let items = rows(&env, "A2 envelope");
    assert_eq!(items.len(), 2, "[A2/C3.4] items 长度必须 == min(limit,total)=2：{}", env.describe());
    assert_eq!(number_field(&env, "total", "A2"), 3, "[A2/C3.4] envelope.total 必须 == X-Total-Count");
    assert_eq!(number_field(&env, "limit", "A2"), 2, "[A2/C3.4] envelope.limit 必须 == X-Limit");
    assert_eq!(number_field(&env, "offset", "A2"), 0, "[A2/C3.4] envelope.offset 必须 == X-Offset");
    assert_eq!(env.json["has_more"], json!(true), "[A2/C3.4] envelope.has_more 必须 == X-Has-More");
    assert_eq!(
        string_field(&env, "archived", "A2"),
        "exclude",
        "[A2/C3.4] envelope.archived 必须回显生效值"
    );
    assert_eq!(
        env.json["filters"]["project"],
        json!("p1"),
        "[A2/C3.4] filters.project 必须回显生效值：{}",
        env.describe()
    );
    assert_eq!(total_count(&env, "A2"), 3, "[A2/C3.4] envelope=1 时响应头同样下发且一致");

    // Act 5：offset 翻页（total 不随 offset 变化）
    let page2 = list_issues(&server, "project=p1&envelope=1&limit=2&offset=2");
    assert_status(&page2, 200, "A2 offset=2");
    assert_eq!(rows(&page2, "A2 offset=2").len(), 1, "[A2] 最后一页必须只剩 1 行");
    assert_eq!(number_field(&page2, "total", "A2"), 3, "[A2] total 不随 offset 变化");
    assert_eq!(page2.json["has_more"], json!(false), "[A2/C3.4] 末页 has_more 必须 false");
    assert_eq!(number_field(&page2, "offset", "A2"), 2, "[A2/C3.4] offset 回显");
}

// ═══════════════════════════════════════════════════════════════════════════
// A3 — archived 三态过滤
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a3_list_archived_filter_modes() {
    // Arrange
    let server = Server::start();
    let keep1 = seed_distinct(&server, "p1", "a3k1", "evt-a3-1");
    let keep2 = seed_distinct(&server, "p1", "a3k2", "evt-a3-2");
    let gone = seed_distinct_with_status(&server, "p1", "a3g", "evt-a3-3", "ignored");
    assert_status(&archive_issue(&server, &gone), 200, "A3 Arrange 单条归档");

    // Assert 1：默认列表（exclude）不含归档项
    let default_list = list_issues(&server, "project=p1");
    assert_status(&default_list, 200, "A3 默认列表");
    let default_ids = ids_of(&rows(&default_list, "A3 默认"));
    assert!(
        !default_ids.contains(&gone),
        "[A3] 归档后默认列表不得含归档项 {gone}：{}",
        default_list.describe()
    );
    assert_eq!(
        sorted(default_ids.clone()),
        sorted(vec![keep1.clone(), keep2.clone()]),
        "[A3] 默认列表必须恰为未归档的两条：{}",
        default_list.describe()
    );
    assert_eq!(
        header(&default_list, "x-archived-mode").as_deref(),
        Some("exclude"),
        "[A3/C3.3] 缺省 archived 必须回显 exclude"
    );

    // Assert 2：archived=only 只含归档项
    let only = list_issues(&server, "project=p1&archived=only");
    assert_status(&only, 200, "A3 archived=only");
    let only_rows = rows(&only, "A3 archived=only");
    assert_eq!(
        ids_of(&only_rows),
        vec![gone.clone()],
        "[A3] archived=only 必须恰含归档项：{}",
        only.describe()
    );
    for issue in &only_rows {
        assert!(
            archived_at_of(issue).is_some(),
            "[A3] archived=only 每项 archived_at 必须非空：{issue}"
        );
    }
    assert_eq!(
        header(&only, "x-archived-mode").as_deref(),
        Some("only"),
        "[A3/C3.3] X-Archived-Mode 必须回显 only"
    );

    // Assert 3：archived=include 两者都含，且 include == exclude + only
    let include = list_issues(&server, "project=p1&archived=include");
    assert_status(&include, 200, "A3 archived=include");
    let include_rows = rows(&include, "A3 archived=include");
    assert_eq!(include_rows.len(), 3, "[A3] archived=include 必须含全部 3 条：{}", include.describe());
    assert!(ids_of(&include_rows).contains(&gone), "[A3] include 必须含归档项 {gone}");
    assert_eq!(
        include_rows.len(),
        default_ids.len() + only_rows.len(),
        "[A3] include 数必须 == exclude 数 + only 数"
    );
    assert_eq!(
        header(&include, "x-archived-mode").as_deref(),
        Some("include"),
        "[A3/C3.3] X-Archived-Mode 必须回显 include"
    );

    // 负向边界：非法 archived → 400
    assert_status(
        &list_issues(&server, "project=p1&archived=bogus"),
        400,
        "C3.2 非法 archived → 400 Bad request: invalid archived",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A4 — 单条归档 / 恢复：幂等 + count/status/事件行零回归 + 404/401
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a4_single_archive_and_unarchive() {
    // Arrange：一条 issue，count=2、事件行=2
    let server = Server::start();
    let issue_id = seed_issue(&server, "p1", "A4 target", "evt-a4-1");
    assert_eq!(
        append_event(&server, "p1", "A4 target", "evt-a4-2"),
        issue_id,
        "[Arrange] 同指纹追加事件必须落到同一 issue"
    );
    let before = issue_detail(&server, &issue_id);
    let count_before = before.i64_field("count");
    let status_before = string_field(&before, "status", "A4 Arrange");
    let events_before = event_rows(&server, &issue_id).len();
    assert_eq!(count_before, 2, "[Arrange] count 必须为 2");
    assert_eq!(events_before, 2, "[Arrange] 事件行数必须为 2");
    assert_eq!(
        archived_at_of(&before.json),
        None,
        "[A4] 归档前 archived_at 必须为 null"
    );
    // 契约 v1.1 C4.3：events 响应头 X-Total-Count = 该 issue 事件总行数，与 limit/cursor 无关。
    let total_before = total_count(
        &events_of(&server, &issue_id, "limit=1"),
        "A4 归档前 events X-Total-Count（limit=1 也必须给总数）",
    );
    assert_eq!(total_before, 2, "[A4/C4.3] 事件总行数必须为 2");
    let env_before = events_of(&server, &issue_id, "limit=1&envelope=1");
    assert_status(&env_before, 200, "A4 归档前 events envelope=1");
    assert_eq!(
        number_field(&env_before, "total", "A4 events envelope"),
        total_before,
        "[A4/C4.3] 信封 total 必须等于 X-Total-Count"
    );

    // Act 1：归档
    let archived = archive_issue(&server, &issue_id);
    assert_status(&archived, 200, "A4 POST /api/v1/issues/:id/archive");
    let archived_at = archived_at_of(&archived.json).unwrap_or_else(|| {
        panic!("[A4] 归档响应必须含非空 archived_at：{}", archived.describe())
    });
    // 契约 v1.1 C2.1：手动归档**不写** archive_restored_at（只置 archived_at）。
    assert_eq!(
        restored_at_of(&archived.json),
        None,
        "[A4/C2.1 v1.1] 手动归档不得写 archive_restored_at：{}",
        archived.describe()
    );

    // Assert 1：归档不改 count / status / 事件行 / 事件总数（C4.3 双证）
    let after = issue_detail(&server, &issue_id);
    assert_eq!(
        archived_at_of(&after.json).as_deref(),
        Some(archived_at.as_str()),
        "[A4] 详情 archived_at 必须与归档响应一致：{}",
        after.describe()
    );
    assert_eq!(after.i64_field("count"), count_before, "[A4] 归档不得改变 count");
    assert_eq!(
        string_field(&after, "status", "A4"),
        status_before,
        "[A4] 归档不得改变 status"
    );
    assert_eq!(
        event_rows(&server, &issue_id).len(),
        events_before,
        "[A4] 归档不得改变事件行数"
    );
    let total_after = total_count(
        &events_of(&server, &issue_id, "limit=100"),
        "A4 归档后 events X-Total-Count",
    );
    assert_eq!(
        total_after, total_before,
        "[A4 补判据/C4.3] 归档前后 X-Total-Count 必须相等（{total_before} vs {total_after}）"
    );
    let env_after = events_of(&server, &issue_id, "limit=2&envelope=1");
    assert_status(&env_after, 200, "A4 归档后 events envelope=1");
    assert_eq!(
        number_field(&env_after, "total", "A4 events envelope"),
        total_before,
        "[A4 补判据/C4.3] 归档前后信封 total 必须相等"
    );

    // Act 2 / Assert 2：重复归档幂等（archived_at 不变、不写 restore 留痕）
    let again = archive_issue(&server, &issue_id);
    assert_status(&again, 200, "A4 重复归档必须 200（幂等）");
    assert_eq!(
        archived_at_of(&again.json).as_deref(),
        Some(archived_at.as_str()),
        "[A4] 重复归档 archived_at 必须不变：首次={archived_at} 实际={}",
        again.describe()
    );
    assert_eq!(
        restored_at_of(&again.json),
        None,
        "[A4/C2.3] 重复归档不得覆盖 archive_restored_at"
    );

    // Act 3 / Assert 3：恢复后 archived_at = null 且写 archive_restored_at 留痕（v1.1 A-1）
    let restored = unarchive_issue(&server, &issue_id);
    assert_status(&restored, 200, "A4 POST /api/v1/issues/:id/unarchive");
    assert_eq!(
        archived_at_of(&restored.json),
        None,
        "[A4] unarchive 后响应 archived_at 必须为 null：{}",
        restored.describe()
    );
    let restored_at = restored_at_of(&restored.json).unwrap_or_else(|| {
        panic!(
            "[A4/C2.3 v1.1] unarchive 必须写 archive_restored_at 留痕：{}",
            restored.describe()
        )
    });
    let detail_restored = issue_detail(&server, &issue_id);
    assert_eq!(
        archived_at_of(&detail_restored.json),
        None,
        "[A4] unarchive 后详情 archived_at 必须为 null"
    );
    assert_eq!(
        restored_at_of(&detail_restored.json).as_deref(),
        Some(restored_at.as_str()),
        "[A4/C2.3 v1.1] 详情 archive_restored_at 必须与恢复响应一致"
    );

    // 负向边界：不存在 id → 404（归档 / 恢复均 404）
    let missing = "00000000-0000-4000-8000-00000000dead";
    assert_status(&archive_issue(&server, missing), 404, "A4 归档不存在 id → 404");
    assert_status(&unarchive_issue(&server, missing), 404, "A4 恢复不存在 id → 404");

    // 负向边界：无 token → 401
    assert_status(
        &server.send(reqwest::Method::POST, &format!("{ISSUES}/{issue_id}/archive"), &[], None),
        401,
        "A4 无 token 归档 → 401",
    );
    assert_status(
        &server.send(reqwest::Method::POST, &format!("{ISSUES}/{issue_id}/unarchive"), &[], None),
        401,
        "A4 无 token 恢复 → 401",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A5 — 批量归档：dry_run 不改库 / 真实执行 / statuses 400 / ids 优先
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a5_batch_archive_dry_run_and_skips() {
    // Arrange：p1 三态各一条
    let server = Server::start();
    let ignored = seed_distinct_with_status(&server, "p1", "a5i", "evt-a5-1", "ignored");
    let resolved = seed_distinct_with_status(&server, "p1", "a5r", "evt-a5-2", "resolved");
    let unresolved = seed_distinct_with_status(&server, "p1", "a5u", "evt-a5-3", "unresolved");

    // Act 1：dry_run（v1.1 C2.3：只分桶，archived 恒 0，matched 与真实执行一致）
    let dry = archive_batch(&server, "?dry_run=1", &json!({"project": "p1"}));
    assert_status(&dry, 200, "A5 批量归档 dry_run=1");
    assert_eq!(dry.json["dry_run"], json!(true), "[A5/C2.4] dry_run 必须回显 true");
    assert_eq!(
        number_field(&dry, "matched", "A5"),
        2,
        "[A5/C2.3 v1.1] statuses 模式 matched ≡ 可归档命中数 = 2（unresolved 不进入命中集）：{}",
        dry.describe()
    );
    assert_eq!(
        number_field(&dry, "archived", "A5"),
        0,
        "[A5/C2.4] dry_run=true 时 archived 恒为 0：{}",
        dry.describe()
    );

    // Assert 1：dry_run 未改库
    for id in [&ignored, &resolved] {
        assert_eq!(
            archived_now(&server, id),
            None,
            "[A5] dry_run 不得改库，但 {id} 已被归档"
        );
    }
    assert_eq!(
        rows(&list_issues(&server, "project=p1&archived=only"), "A5 dry_run 后").len(),
        0,
        "[A5] dry_run 后 archived=only 必须为空"
    );

    // Act 2：真实执行（v1.1 C2.3：statuses 模式 matched ≡ archived ≡ RETURNING 行数，skipped_* 恒 0）
    let real = archive_batch(&server, "", &json!({"project": "p1"}));
    assert_status(&real, 200, "A5 真实批量归档");
    assert_eq!(real.json["dry_run"], json!(false), "[A5/C2.4] 真实执行 dry_run 必须回显 false");
    assert_eq!(
        number_field(&real, "archived", "A5"),
        2,
        "[A5/C2.4] 本次实际归档数必须为 2（ignored + resolved）：{}",
        real.describe()
    );
    assert_eq!(
        number_field(&real, "matched", "A5"),
        number_field(&real, "archived", "A5"),
        "[A5/C2.4 v1.1] statuses 模式 matched 必须恒等于 archived：{}",
        real.describe()
    );
    for field in [
        "skipped_already_archived",
        "skipped_not_archivable",
        "skipped_missing",
    ] {
        assert_eq!(
            number_field(&real, field, "A5"),
            0,
            "[A5/C2.4 v1.1] statuses 模式下 `{field}` 必须恒为 0：{}",
            real.describe()
        );
    }
    assert_eq!(
        number_field(&real, "matched", "A5"),
        number_field(&dry, "matched", "A5"),
        "[A5/C2.3] dry_run 与真实执行的 matched 必须一致"
    );
    assert!(
        archived_now(&server, &ignored).is_some(),
        "[A5] ignored issue 必须被归档"
    );
    assert!(
        archived_now(&server, &resolved).is_some(),
        "[A5] resolved issue 必须被归档"
    );
    assert_eq!(
        archived_now(&server, &unresolved),
        None,
        "[A5] unresolved issue 不得被归档"
    );
    assert_eq!(
        real.json["criteria"]["statuses"],
        json!(["ignored", "resolved"]),
        "[A5/C2.4] 缺省 statuses 必须回显生效白名单：{}",
        real.describe()
    );
    assert_eq!(
        real.json["criteria"]["ids_take_priority"],
        json!(false),
        "[A5/C2.4] 未给 ids 时 ids_take_priority 必须 false"
    );

    // Act 3：ids 模式（v1.1 C2.3：matched = dedup(ids).len()，四桶加和 == matched）
    let p2_unresolved = seed_distinct_with_status(&server, "p2", "a5p2u", "evt-a5-4", "unresolved");
    let p2_ignored = seed_distinct_with_status(&server, "p2", "a5p2i", "evt-a5-5", "ignored");
    let ghost = "11111111-1111-4111-8111-111111111111";
    let ids = vec![p2_ignored.clone(), p2_unresolved.clone(), ghost.to_string()];
    let ids_run = archive_batch(&server, "", &json!({"ids": ids}));
    assert_status(&ids_run, 200, "A5 ids 模式批量归档");
    assert_eq!(
        ids_run.json["criteria"]["ids_take_priority"],
        json!(true),
        "[A5/C2.4] ids 非空时 ids_take_priority 必须 true：{}",
        ids_run.describe()
    );
    assert_eq!(
        ids_run.json["criteria"]["statuses"],
        json!(["ignored", "resolved"]),
        "[A5/C2.4 v1.1] ids 模式未给 statuses → criteria.statuses 必须回显默认白名单：{}",
        ids_run.describe()
    );
    assert_eq!(
        ids_run.json["criteria"]["ids"],
        json!(ids),
        "[A5/C2.4] criteria.ids 必须回显请求值：{}",
        ids_run.describe()
    );
    assert_eq!(
        number_field(&ids_run, "matched", "A5"),
        3,
        "[A5/C2.4 v1.1] ids 模式 matched 必须 == dedup(ids).len() == 3：{}",
        ids_run.describe()
    );
    assert_eq!(
        number_field(&ids_run, "archived", "A5"),
        1,
        "[A5/C2.3] ids 模式只归档白名单内且未归档的 id"
    );
    assert_eq!(
        number_field(&ids_run, "skipped_not_archivable", "A5"),
        1,
        "[A5/C2.3] ids 含 unresolved → 计入 skipped_not_archivable：{}",
        ids_run.describe()
    );
    assert_eq!(
        number_field(&ids_run, "skipped_missing", "A5"),
        1,
        "[A5/C2.3 v1.1] ids 含库中不存在的 id → 计入 skipped_missing：{}",
        ids_run.describe()
    );
    assert_eq!(
        number_field(&ids_run, "skipped_already_archived", "A5"),
        0,
        "[A5/C2.3] 本批无已归档 id"
    );
    assert_eq!(
        number_field(&ids_run, "archived", "A5")
            + number_field(&ids_run, "skipped_not_archivable", "A5")
            + number_field(&ids_run, "skipped_already_archived", "A5")
            + number_field(&ids_run, "skipped_missing", "A5"),
        number_field(&ids_run, "matched", "A5"),
        "[A5/C2.4 v1.1] 单写者不变式 archived+skipped_not_archivable+skipped_already_archived+skipped_missing == matched"
    );
    assert!(
        archived_now(&server, &p2_ignored).is_some(),
        "[A5] ids 模式必须真实归档 p2 ignored issue"
    );
    assert_eq!(
        archived_now(&server, &p2_unresolved),
        None,
        "[A5] ids 模式的 unresolved 不得被归档"
    );

    // Act 4：ids 与 statuses 同给（v1.2 裁决写死：ids 定候选集、statuses 仍作归档白名单约束）
    // 期望口径：{"ids":[<ignored>,<unresolved>], "statuses":["ignored"]}
    //   → archived==1（只归档其中的 ignored）、skipped_not_archivable==1（其中的 unresolved）、
    //     criteria.statuses==["ignored"]（生效白名单原值）、criteria.ids_take_priority==true
    //     （= 候选集由 ids 决定，project/older_than_days 不参与筛选；不表示 statuses 被忽略）。
    let p3_ignored = seed_distinct_with_status(&server, "p3", "a5p3i", "evt-a5-6", "ignored");
    let p3_unresolved = seed_distinct_with_status(&server, "p3", "a5p3u", "evt-a5-7", "unresolved");
    let mixed = archive_batch(
        &server,
        "",
        &json!({"ids": [p3_ignored.clone(), p3_unresolved.clone()], "statuses": ["ignored"]}),
    );
    assert_status(&mixed, 200, "A5 ids+statuses 同给");
    assert_eq!(
        mixed.json["criteria"]["statuses"],
        json!(["ignored"]),
        "[A5/C2.4 v1.2] ids 模式给了 statuses → criteria.statuses 必须回显生效白名单原值：{}",
        mixed.describe()
    );
    assert_eq!(
        mixed.json["criteria"]["ids_take_priority"],
        json!(true),
        "[A5/C2.4 v1.2] ids 非空即 ids_take_priority=true（候选集由 ids 决定，不含 statuses 被忽略之意）"
    );
    assert_eq!(
        number_field(&mixed, "matched", "A5"),
        2,
        "[A5/C2.4 v1.2] matched == dedup(ids).len() == 2"
    );
    assert_eq!(
        number_field(&mixed, "archived", "A5"),
        1,
        "[A5/C2.3 v1.2] statuses 仍是归档白名单：ids 中只有 ignored 行被归档 → archived==1：{}",
        mixed.describe()
    );
    assert_eq!(
        number_field(&mixed, "skipped_not_archivable", "A5"),
        1,
        "[A5/C2.3 v1.2] ids 中的 unresolved 行计入 skipped_not_archivable==1：{}",
        mixed.describe()
    );
    assert_eq!(
        number_field(&mixed, "skipped_already_archived", "A5"),
        0,
        "[A5/C2.4 v1.2] 本批无已归档 id → skipped_already_archived==0"
    );
    assert_eq!(
        number_field(&mixed, "skipped_missing", "A5"),
        0,
        "[A5/C2.4 v1.2] ids 全部存在 → skipped_missing==0"
    );
    assert_eq!(
        number_field(&mixed, "archived", "A5")
            + number_field(&mixed, "skipped_not_archivable", "A5")
            + number_field(&mixed, "skipped_already_archived", "A5")
            + number_field(&mixed, "skipped_missing", "A5"),
        number_field(&mixed, "matched", "A5"),
        "[A5/C2.4 v1.2] 单写者等式：archived + skipped_not_archivable + skipped_already_archived + skipped_missing == matched"
    );
    assert!(
        archived_now(&server, &p3_ignored).is_some(),
        "[A5/C2.3 v1.2] ids 中的 ignored 行必须被真实归档"
    );
    assert_eq!(
        archived_now(&server, &p3_unresolved),
        None,
        "[A5/C2.3 v1.2] ids 中的 unresolved 行不得被归档（statuses 白名单约束）"
    );

    // 负向边界：越界 statuses → 400
    assert_status(
        &archive_batch(&server, "", &json!({"statuses": ["unresolved"]})),
        400,
        "C2.3 statuses 含 unresolved → 400 Bad request: invalid statuses",
    );
    assert_status(
        &archive_batch(&server, "", &json!({"statuses": ["regression"]})),
        400,
        "C2.3 statuses 含 regression → 400 Bad request: invalid statuses",
    );
    let dup_statuses = archive_batch(&server, "", &json!({"statuses": ["ignored", "ignored"]}));
    assert_status(&dup_statuses, 400, "C2.3 v1.1 statuses 含重复元素 → 400");
    assert!(
        dup_statuses.raw.contains("invalid statuses"),
        "[C2.3] 文案必须为 `Bad request: invalid statuses`：{}",
        dup_statuses.describe()
    );

    // 负向边界：v1.1 硬上限
    let too_many: Vec<String> = (0..1001)
        .map(|i| format!("00000000-0000-4000-8000-{i:012}"))
        .collect();
    let too_many_resp = archive_batch(&server, "", &json!({"ids": too_many}));
    assert_status(&too_many_resp, 400, "C2.3 v1.1 ids 去重后 > 1000 → 400");
    assert!(
        too_many_resp.raw.contains("too many ids"),
        "[C2.3 v1.1] 文案必须为 `Bad request: too many ids`：{}",
        too_many_resp.describe()
    );
    let long_id = "a".repeat(129);
    let long_id_resp = archive_batch(&server, "", &json!({"ids": [long_id]}));
    assert_status(&long_id_resp, 400, "C2.3 v1.1 单 id 长度 > 128 → 400");
    let huge_body = archive_batch(&server, "", &json!({"project": "x".repeat(70 * 1024)}));
    assert_status(&huge_body, 413, "C2.3 v1.1 body > 64KB → 413（DefaultBodyLimit）");

    // 负向边界：无 token → 401
    assert_status(
        &server.send(
            reqwest::Method::POST,
            &format!("{ISSUES}/archive"),
            &[],
            Some(&json!({})),
        ),
        401,
        "A5 无 token 批量归档 → 401",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A6 — TTL 自动归档作业（ENABLED / TTL_SECS / INTERVAL_SECS）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a6_ttl_auto_archive_job() {
    // Arrange：作业开启，TTL=1s，周期=1s
    let server = Server::start_with(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("PONY_ISSUE_ARCHIVE_ENABLED", "1"),
        ("PONY_ISSUE_ARCHIVE_TTL_SECS", "1"),
        ("PONY_ISSUE_ARCHIVE_INTERVAL_SECS", "1"),
    ]);
    let ignored = seed_distinct_with_status(&server, "p1", "a6i", "evt-a6-1", "ignored");
    let unresolved = seed_distinct_with_status(&server, "p1", "a6u", "evt-a6-2", "unresolved");

    // Act / Assert 1：ignored 在 ≤10s 内被自动归档
    let auto_archived = wait_until(|| archived_now(&server, &ignored).is_some(), Duration::from_secs(10));
    assert!(
        auto_archived,
        "[A6] TTL_SECS=1 + INTERVAL_SECS=1 下，ignored issue 必须在 ≤10s 内被自动归档；\
         实际 archived_at={:?}",
        archived_now(&server, &ignored)
    );

    // Assert 2：unresolved 永不被自动归档（作业至少已跑过多轮）
    assert_eq!(
        archived_now(&server, &unresolved),
        None,
        "[A6] unresolved issue 永不得被自动归档"
    );

    // Arrange 2 / Assert 3：ENABLED=0 时作业不跑
    let disabled = Server::start_with(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("PONY_ISSUE_ARCHIVE_ENABLED", "0"),
        ("PONY_ISSUE_ARCHIVE_TTL_SECS", "1"),
        ("PONY_ISSUE_ARCHIVE_INTERVAL_SECS", "1"),
    ]);
    let disabled_ignored = seed_distinct_with_status(&disabled, "p1", "a6d", "evt-a6-3", "ignored");
    let appeared = wait_until(
        || archived_now(&disabled, &disabled_ignored).is_some(),
        Duration::from_secs(4),
    );
    assert!(
        !appeared,
        "[A6] PONY_ISSUE_ARCHIVE_ENABLED=0 时作业必须不跑（4s 观察窗内不得自动归档）"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A7 — 事件分页（limit / cursor keyset / 篡改 400 / order=asc）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a7_event_pagination_cursor() {
    // Arrange：同一 issue 造 12 条事件
    let server = Server::start();
    let issue_id = seed_issue(&server, "p1", "A7 events", "evt-a7-00");
    for i in 1..12 {
        let event_id = format!("evt-a7-{i:02}");
        assert_eq!(
            append_event(&server, "p1", "A7 events", &event_id),
            issue_id,
            "[Arrange] 同指纹追加事件必须落到同一 issue（{event_id}）"
        );
    }
    assert_eq!(
        issue_detail(&server, &issue_id).i64_field("count"),
        12,
        "[Arrange] 12 次上报后 count 必须为 12"
    );

    // Act 1：limit=5
    let page1 = events_of(&server, &issue_id, "limit=5");
    assert_status(&page1, 200, "A7 GET /issues/:id/events?limit=5");
    let page1_rows = rows(&page1, "A7 limit=5");
    assert_eq!(page1_rows.len(), 5, "[A7] limit=5 必须恰好 5 条：{}", page1.describe());
    assert_eq!(header_int(&page1, "x-limit", "A7"), 5, "[A7/C4.3] X-Limit 必须回显生效 limit");
    assert_eq!(
        header(&page1, "x-has-more").as_deref(),
        Some("true"),
        "[A7/C4.3] 存在下一页 → X-Has-More: true"
    );
    assert_eq!(
        total_count(&page1, "A7/C4.3 events X-Total-Count"),
        12,
        "[A7/C4.3 v1.1] X-Total-Count 必须是事件总行数（与 limit/cursor 无关）：{}",
        page1.describe()
    );
    let cursor = header(&page1, "x-next-cursor").unwrap_or_else(|| {
        panic!("[A7/C4.3] 有下一页时必须下发 X-Next-Cursor：{}", page1.describe())
    });

    // Assert 1：游标载荷格式（v1.1 C4.2：{"created_at": 原始 RFC3339 串, "id": id}，无 padding base64url）
    let decoded = decode_cursor(&cursor, "A7/C4.2 v1.1");
    let keys: Vec<&String> = decoded
        .as_object()
        .unwrap_or_else(|| panic!("[A7/C4.2] 游标载荷必须是 JSON 对象：{decoded}"))
        .keys()
        .collect();
    let mut keys_sorted: Vec<String> = keys.iter().map(|k| (*k).clone()).collect();
    keys_sorted.sort();
    assert_eq!(
        keys_sorted,
        vec!["created_at".to_string(), "id".to_string()],
        "[A7/C4.2 v1.1] 游标载荷必须恰为 {{created_at,id}} 两个字段：{decoded}"
    );
    let last = page1_rows
        .last()
        .expect("limit=5 页非空")
        .clone();
    assert_eq!(
        decoded["created_at"].as_str().unwrap_or_default(),
        last["created_at"].as_str().unwrap_or_default(),
        "[A7/C4.2 v1.1] 游标 created_at 必须是该行原始 RFC3339 串（不得截断精度）：{decoded} vs {last}"
    );
    assert_eq!(
        decoded["id"].as_str().unwrap_or_default(),
        row_key(&last),
        "[A7/C4.2 v1.1] 游标 id 必须是本页末行 id：{decoded} vs {last}"
    );

    // Act 2：envelope=1 得 next_cursor 与 total
    let env1 = events_of(&server, &issue_id, "limit=5&envelope=1");
    assert_status(&env1, 200, "A7 limit=5&envelope=1");
    assert_eq!(rows(&env1, "A7 envelope").len(), 5, "[A7/C4.3] envelope items 长度 5");
    assert_eq!(number_field(&env1, "limit", "A7"), 5, "[A7/C4.3] envelope.limit 必须回显");
    assert_eq!(env1.json["has_more"], json!(true), "[A7/C4.3] envelope.has_more 必须 true");
    assert_eq!(
        number_field(&env1, "total", "A7"),
        12,
        "[A7/C4.3 v1.1] envelope.total 必须等于 X-Total-Count：{}",
        env1.describe()
    );
    let env_cursor = env1.json["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("[A7/C4.3] envelope=1 必须含非空 next_cursor：{}", env1.describe()))
        .to_string();

    // 对照：单页取全量
    let full = events_of(&server, &issue_id, "limit=100");
    assert_status(&full, 200, "A7 limit=100");
    let full_rows = rows(&full, "A7 limit=100");
    assert_eq!(full_rows.len(), 12, "[A7] 事件总行数必须 12：{}", full.describe());
    assert_eq!(
        total_count(&full, "A7 limit=100 X-Total-Count"),
        12,
        "[A7/C4.3 v1.1] 全量页 X-Total-Count 必须 12"
    );
    assert!(
        header(&full, "x-next-cursor").is_none(),
        "[A7/C4.3] 无下一页时不得出现 X-Next-Cursor：{:?}",
        header(&full, "x-next-cursor")
    );
    assert_eq!(
        header(&full, "x-has-more").as_deref(),
        Some("false"),
        "[A7/C4.3] 全量覆盖时 X-Has-More 必须 false"
    );
    // C4.1：limit > 100 → clamp 到 100
    let clamped = events_of(&server, &issue_id, "limit=500");
    assert_status(&clamped, 200, "A7 limit=500");
    assert_eq!(
        header_int(&clamped, "x-limit", "A7"),
        100,
        "[A7/C4.1] limit>100 必须 clamp 到 100"
    );
    let full_keys = sorted(keys_of(&full_rows));

    // Act 3：cursor 翻页无重无漏（每页 X-Total-Count 恒为 12）
    let mut seen: Vec<String> = keys_of(&page1_rows);
    let mut cursor = env_cursor;
    let mut pages = 1;
    loop {
        let resp = server.get_admin(&format!(
            "{ISSUES}/{issue_id}/events?limit=5&envelope=1&cursor={cursor}"
        ));
        assert_status(&resp, 200, "A7 cursor 翻页");
        assert_eq!(
            number_field(&resp, "total", "A7 翻页"),
            12,
            "[A7/C4.3 v1.1] 任意一页的 total 必须恒为 12（与 cursor/limit 无关）：{}",
            resp.describe()
        );
        pages += 1;
        assert!(pages <= 10, "[A7] 翻页次数异常，疑似游标不前进：cursor={cursor}");
        let page_rows = rows(&resp, "A7 cursor 页");
        assert!(
            !page_rows.is_empty(),
            "[A7] next_cursor 非空却返回空页（游标不前进）：{}",
            resp.describe()
        );
        seen.extend(keys_of(&page_rows));
        match resp.json["next_cursor"].as_str() {
            Some(next) => cursor = next.to_string(),
            None => break,
        }
    }
    assert_eq!(
        sorted(seen.clone()).len(),
        seen.len(),
        "[A7] 翻页出现重复项（有重）：{seen:?}"
    );
    assert_eq!(
        sorted(seen.clone()),
        full_keys,
        "[A7] 翻页并集必须等于全量 12 条（无漏）：seen={seen:?}"
    );
    assert_eq!(pages, 3, "[A7/C4.2] 12 条 / limit=5 必须恰好翻 3 页，实际 {pages}");

    // C4.4 边界：issue 不存在 → 200 + [] + X-Total-Count: 0 + 无 X-Next-Cursor
    let ghost = "22222222-2222-4222-8222-222222222222";
    let ghost_resp = events_of(&server, ghost, "limit=5");
    assert_status(&ghost_resp, 200, "C4.4 issue 不存在必须 200 + []");
    assert_eq!(
        rows(&ghost_resp, "C4.4 不存在 issue").len(),
        0,
        "[C4.4] 不存在 issue 必须返回空数组"
    );
    assert_eq!(
        total_count(&ghost_resp, "C4.4 不存在 issue"),
        0,
        "[C4.4 v1.1] 不存在 issue 的 X-Total-Count 必须 0"
    );
    assert!(
        header(&ghost_resp, "x-next-cursor").is_none(),
        "[C4.4] 空页不得下发 X-Next-Cursor"
    );

    // 负向边界：limit=0 → 400
    assert_status(
        &events_of(&server, &issue_id, "limit=0"),
        400,
        "C4.1 limit=0 → 400 Bad request: invalid limit",
    );
    // 负向边界：篡改 cursor → 400
    assert_status(
        &events_of(&server, &issue_id, "cursor=@@not-base64@@"),
        400,
        "C4.1 非 base64url cursor → 400 Bad request: invalid cursor",
    );
    assert_status(
        &events_of(&server, &issue_id, "cursor=e30"),
        400,
        "C4.1 缺字段 cursor（base64url `{}`）→ 400",
    );
    // v1.1 格式变更：旧的 created_at_micros 载荷必须被拒（未知字段）
    let legacy = b64url_encode(br#"{"created_at_micros":1,"id":"x"}"#);
    assert_status(
        &events_of(&server, &issue_id, &format!("cursor={legacy}")),
        400,
        "C4.1 v1.1 旧格式游标（created_at_micros）必须 400（多出未知字段）",
    );
    let long_cursor = "A".repeat(513);
    assert_status(
        &events_of(&server, &issue_id, &format!("cursor={long_cursor}")),
        400,
        "C4.3 cursor 长度 > 512 → 400",
    );
    assert_status(
        &events_of(&server, &issue_id, "order=bogus"),
        400,
        "C4.1 非法 order → 400",
    );

    // Assert 4：order=asc 必须是 desc 的严格反序（created_at,id 全序）
    let desc_resp = events_of(&server, &issue_id, "limit=100&order=desc");
    assert_status(&desc_resp, 200, "A7 order=desc");
    let desc_all = rows(&desc_resp, "A7 order=desc");
    let asc_all = rows(&events_of(&server, &issue_id, "limit=100&order=asc"), "A7 order=asc");
    assert_eq!(desc_all.len(), 12, "[A7] order=desc 必须 12 条");
    assert_eq!(asc_all.len(), 12, "[A7] order=asc 必须 12 条");
    let mut expected_asc = keys_of(&desc_all);
    expected_asc.reverse();
    assert_eq!(
        keys_of(&asc_all),
        expected_asc,
        "[A7/C4.2] order=asc 必须是 order=desc 的严格反序"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A8 — 统计：issues/events 双口径 + 木桶不变式 + series + 400 边界
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a8_stats_bucket_invariants() {
    // Arrange：p1 三态各一条（其中 unresolved 追加一次 → count=2），p2 一条不得串味
    let server = Server::start();
    let unresolved = seed_issue_with_status(&server, "p1", "A8 unresolved", "evt-a8-1", "unresolved");
    let resolved = seed_distinct_with_status(&server, "p1", "a8r", "evt-a8-2", "resolved");
    let ignored = seed_distinct_with_status(&server, "p1", "a8i", "evt-a8-3", "ignored");
    let _other = seed_distinct(&server, "p2", "a8p2", "evt-a8-4");
    assert_eq!(
        append_event(&server, "p1", "A8 unresolved", "evt-a8-5"),
        unresolved,
        "[Arrange] 追加事件必须落到 p1 的 unresolved issue"
    );

    // Act
    let resp = stats(&server, "project=p1");

    // Assert 1：信封回显
    assert_status(&resp, 200, "A8 GET /api/v1/stats?project=p1");
    assert_eq!(resp.json["project"], json!("p1"), "[A8/C5] project 必须回显生效值");
    assert_eq!(
        resp.json["include_archived"],
        json!(false),
        "[A8/C5] include_archived 缺省必须回显 false"
    );
    assert_eq!(
        resp.json["range"]["bucket"],
        json!("day"),
        "[A8/C5] range.bucket 缺省必须为 day"
    );

    // Assert 2：issues 口径与木桶不变式
    let issues = &resp.json["issues"];
    let issues_total = vnum(&issues["total"], "A8 issues.total");
    assert_eq!(issues_total, 3, "[A8/C5] project=p1 的 issues.total 必须为 3：{}", resp.describe());
    assert_eq!(
        bucket_sum(&issues["by_status"], "A8 issues.by_status"),
        issues_total,
        "[A8/C5] sum(by_status) 必须 == issues.total"
    );
    assert_eq!(
        bucket_sum(&issues["by_level"], "A8 issues.by_level"),
        issues_total,
        "[A8/C5] sum(by_level) 必须 == issues.total"
    );
    assert_eq!(
        bucket_sum(&issues["by_environment"], "A8 issues.by_environment"),
        issues_total,
        "[A8/C5] sum(by_environment) 必须 == issues.total"
    );
    assert_eq!(
        issues["by_status"]["unresolved"],
        json!(1),
        "[A8] by_status.unresolved 必须为 1：{}",
        resp.describe()
    );
    assert_eq!(issues["by_status"]["resolved"], json!(1), "[A8] by_status.resolved 必须为 1");
    assert_eq!(issues["by_status"]["ignored"], json!(1), "[A8] by_status.ignored 必须为 1");

    // Assert 3：events 聚合口径 == SUM(issues.count) = 2+1+1 = 4
    let events = &resp.json["events"];
    assert_eq!(
        events["basis"],
        json!("sum_issues_count"),
        "[A8/C5] events.basis 恒为 sum_issues_count：{}",
        resp.describe()
    );
    assert_eq!(
        vnum(&events["total"], "A8 events.total"),
        4,
        "[A8/C5] events.total 必须 == SUM(issues.count)（2+1+1）"
    );
    // Assert 4：events 行口径 == 实际 events 行数（行口径独立可验证）
    let mut stored = 0usize;
    for id in [&unresolved, &resolved, &ignored] {
        stored += event_rows(&server, id).len();
    }
    assert_eq!(
        vnum(&events["stored_rows"], "A8 events.stored_rows"),
        stored as i64,
        "[A8/C5] events.stored_rows 必须是同过滤条件的 events 行数（实际 {stored}）"
    );

    // Assert 5：series 桶可解析
    let series = resp.json["series"]
        .as_array()
        .unwrap_or_else(|| panic!("[A8/C5] series 必须是数组：{}", resp.describe()));
    assert!(!series.is_empty(), "[A8/C5] series.len() 必须 >= 1");
    for point in series {
        let bucket = point["bucket"]
            .as_str()
            .unwrap_or_else(|| panic!("[A8/C5] series 桶缺字符串 bucket：{point}"));
        assert!(
            is_day_bucket(bucket),
            "[A8/C5] bucket=day 的桶键必须可解析为 YYYY-MM-DD，实际 `{bucket}`"
        );
        vnum(&point["issues"], "A8 series.issues");
        vnum(&point["events"], "A8 series.events");
    }

    // 负向边界：hour 桶超范围（缺省 30d > 7d）→ 400
    assert_status(
        &stats(&server, "project=p1&bucket=hour"),
        400,
        "C5 bucket=hour 且 to-from>7d → 400 Bad request: range too large for hour bucket",
    );
    // 负向边界：from > to → 400
    assert_status(
        &stats(&server, "project=p1&from=2026-10-25T00:00:00Z&to=2026-10-01T00:00:00Z"),
        400,
        "C5 from>to → 400 Bad request: invalid range",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A9 — 治理指标：received/created/duplicate/suppressed_ignored 计数语义
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a9_ingest_governance_metrics() {
    let server = Server::start();
    let payload = |event_id: &str| {
        let mut body = full_v1_ingest_payload(event_id, "production", "A9 metrics target");
        body["tags"] = json!({"project": "p1", "alertname": "issue-governance"});
        body["extra"] = json!({"project_path": "/workspace/p1"});
        body
    };

    // Arrange：空库基线
    let t0 = ingest_totals(&server);
    assert_eq!(t0.received, 0, "[A9/C6.2] 空库 received 必须为 0");
    assert_eq!(t0.created, 0, "[A9/C6.2] 空库 created 必须为 0");

    // Act 1：新 event_id 上报
    let first = server.post_ingest_client(&payload("evt-a9-1"));
    assert_status(&first, 201, "A9 Arrange 首次上报");

    // Assert 1：received +1、created +1
    let t1 = ingest_totals(&server);
    assert_eq!(t1.received, t0.received + 1, "[A9/C6.2] 新 event_id → received +1");
    assert_eq!(t1.created, t0.created + 1, "[A9/C6.2] 新 event_id → created +1");
    assert_eq!(t1.duplicate, t0.duplicate, "[A9/C6.2] 新 event_id 不得计 duplicate");

    // Act 2：同 event_id 重放
    let replay = server.post_ingest_client(&payload("evt-a9-1"));
    assert_status(&replay, 200, "A9 Arrange 同 event_id 重放");

    // Assert 2：received +1、duplicate +1、created 不变
    let t2 = ingest_totals(&server);
    assert_eq!(t2.received, t1.received + 1, "[A9/C6.2] 重放 → received +1");
    assert_eq!(t2.duplicate, t1.duplicate + 1, "[A9/C6.2] 重放 → duplicate +1");
    assert_eq!(t2.created, t1.created, "[A9/C6.2] 重放 → created 不变");
    assert_eq!(
        t2.accepted,
        t2.created + t2.duplicate,
        "[A9/C6.2] totals.accepted 必须恒等于 created + duplicate"
    );

    // Act 3：对 ignored issue 上报
    let issue_id = first.str_field("issue_id");
    set_issue_status(&server, &issue_id, "ignored");
    let t3 = ingest_totals(&server);
    let suppressed = server.post_ingest_client(&payload("evt-a9-2"));
    assert!(
        suppressed.status == 201 || suppressed.status == 200,
        "[A9/C6.2] 对 ignored issue 上报必须仍写入成功（不拦截），实际 {}",
        suppressed.describe()
    );
    assert_eq!(
        suppressed.str_field("issue_id"),
        issue_id,
        "[A9] 同指纹必须落到同一 issue"
    );

    // Assert 3：suppressed_ignored +1（且 received 同步 +1）
    let t4 = ingest_totals(&server);
    assert_eq!(
        t4.suppressed_ignored,
        t3.suppressed_ignored + 1,
        "[A9/C6.2] 对 ignored issue 上报 → suppressed_ignored +1：{}",
        t4.suppressed_ignored
    );
    assert_eq!(t4.received, t3.received + 1, "[A9/C6.2] 对 ignored issue 上报 → received +1");

    // Assert 4：401 不使 received 增长
    let t5 = ingest_totals(&server);
    assert_status(&server.post_ingest_anon(&payload("evt-a9-3")), 401, "A9 无 token 上报 → 401");
    let t6 = ingest_totals(&server);
    assert_eq!(
        t6.received, t5.received,
        "[A9/C6.2] 鉴权失败 401 不得使 received 增长"
    );

    // Assert 5：结构完整（by_project / series）与 400 边界
    let probe = ingest_stats(&server, "window=24h&project=p1");
    assert_status(&probe, 200, "A9 ingest/stats 带 project");
    assert!(
        probe.json["by_project"].is_array(),
        "[A9/C6.1] by_project 必须是数组：{}",
        probe.describe()
    );
    assert!(
        probe.json["series"].is_array(),
        "[A9/C6.1] series 必须是数组：{}",
        probe.describe()
    );
    assert_eq!(probe.json["window"], json!("24h"), "[A9/C6.1] window 必须回显");
    assert_status(
        &ingest_stats(&server, "window=bogus"),
        400,
        "C6.1 非法 window → 400 Bad request: invalid window",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A10 — ingest 幂等零回归：201 created → 200 duplicate，count / issue 数不变
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a10_ingest_idempotency_zero_regression() {
    // Arrange
    let server = Server::start();
    let payload = |event_id: &str| {
        let mut body = full_v1_ingest_payload(event_id, "production", "A10 idempotency");
        body["tags"] = json!({"project": "p1", "alertname": "issue-governance"});
        body["extra"] = json!({"project_path": "/workspace/p1"});
        body
    };

    // Act 1：首次投递
    let first = server.post_ingest_client(&payload("evt-a10-1"));

    // Assert 1：201 created，响应字段零变更
    assert_status(&first, 201, "A10 首次投递必须 201 created");
    assert_eq!(first.json["duplicate"], json!(false), "[A10] 首次投递 duplicate 必须 false");
    assert_eq!(first.str_field("result"), "created", "[A10] 首次投递 result 必须 created");
    assert_eq!(
        first.str_field("event_id"),
        "evt-a10-1",
        "[A10] 响应必须回显 event_id（字段零变更）：{}",
        first.describe()
    );
    let issue_id = first.str_field("issue_id");
    assert!(!issue_id.is_empty(), "[A10] issue_id 不得为空");
    let count_after_first = issue_detail(&server, &issue_id).i64_field("count");

    // Act 2：同 event_id 重放
    let second = server.post_ingest_client(&payload("evt-a10-1"));

    // Assert 2：200 duplicate，count 不增
    assert_status(&second, 200, "A10 同 event_id 二次投递必须 200 duplicate");
    assert_eq!(second.json["duplicate"], json!(true), "[A10] 重放 duplicate 必须 true");
    assert_eq!(second.str_field("result"), "duplicate", "[A10] 重放 result 必须 duplicate");
    assert_eq!(second.str_field("issue_id"), issue_id, "[A10] 重放必须归属同一 issue");
    assert_eq!(
        issue_detail(&server, &issue_id).i64_field("count"),
        count_after_first,
        "[A10] duplicate 不得递增 issues.count"
    );

    // Act 3：同 fingerprint、不同 event_id
    let third = server.post_ingest_client(&payload("evt-a10-2"));

    // Assert 3：count +1，issue 数不变，事件行 +1
    assert_status(&third, 201, "A10 同指纹新 event_id 必须 201 created");
    assert_eq!(
        third.str_field("issue_id"),
        issue_id,
        "[A10] 同指纹不同 event_id 必须落到同一 issue"
    );
    assert_eq!(
        issue_detail(&server, &issue_id).i64_field("count"),
        count_after_first + 1,
        "[A10] 同指纹新 event_id → count +1"
    );
    let list = server.get_admin(&format!("{ISSUES}?limit=100"));
    assert_status(&list, 200, "A10 列表");
    assert_eq!(
        rows(&list, "A10").len(),
        1,
        "[A10] issue 数必须仍为 1：{}",
        list.describe()
    );
    assert_eq!(
        event_rows(&server, &issue_id).len(),
        2,
        "[A10] 事件行数必须为 2（duplicate 不写行，新 event_id 写 1 行）"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A6b — 手动恢复抑制 TTL 再归档（契约 v1.1 C2.1 / A-1 的 `archive_restored_at` 抑制项）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a6b_restore_suppresses_ttl_rearchive() {
    // Arrange：TTL 作业开启（TTL=1s / INTERVAL=1s）
    let server = Server::start_with(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("PONY_ISSUE_ARCHIVE_ENABLED", "1"),
        ("PONY_ISSUE_ARCHIVE_TTL_SECS", "1"),
        ("PONY_ISSUE_ARCHIVE_INTERVAL_SECS", "1"),
    ]);
    let issue_id = seed_distinct_with_status(&server, "p1", "a6b", "evt-a6b-1", "ignored");
    assert!(
        wait_until(|| archived_now(&server, &issue_id).is_some(), Duration::from_secs(10)),
        "[A6b Arrange] ignored issue 必须先被 TTL 作业自动归档（证明作业在跑）"
    );

    // Act：手动恢复
    let restored = unarchive_issue(&server, &issue_id);
    assert_status(&restored, 200, "A6b 手动恢复已归档 issue");
    assert_eq!(
        archived_at_of(&restored.json),
        None,
        "[A6b Arrange] 恢复后 archived_at 必须为 null：{}",
        restored.describe()
    );
    assert!(
        restored_at_of(&restored.json).is_some(),
        "[A6b/C2.3 v1.1] 手动恢复必须留下 archive_restored_at 留痕：{}",
        restored.describe()
    );

    // Assert：4s 观察窗（≫ TTL 1s + INTERVAL 1s，期间无新事件）内不得被自动重新归档
    let re_archived = wait_until(
        || archived_now(&server, &issue_id).is_some(),
        Duration::from_secs(4),
    );
    assert!(
        !re_archived,
        "[A6b/C2.1 v1.1] 手动恢复后（期间无新事件）TTL 作业不得再次自动归档：archive_restored_at 抑制项缺失"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A11 — 活跃即解档（契约 v1.1 C2.5 / 裁决 A-2，修「活跃僵尸」）
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a11_ingest_unarchives_active_issue() {
    let server = Server::start();

    // ── 场景 1：已归档的 unresolved（活跃态）issue 收到新事件 → 必须解档 ──────────
    let active_payload = |event_id: &str| {
        let mut body = full_v1_ingest_payload(event_id, "production", "A11 active");
        body["tags"] = json!({"project": "p1", "alertname": "issue-governance"});
        body["extra"] = json!({"project_path": "/workspace/p1"});
        body
    };
    let first = ingest_event(&server, &active_payload("evt-a11-1"));
    assert_status(&first, 201, "A11 Arrange 活跃 issue 首次上报");
    let active_id = first.str_field("issue_id");

    let archived = archive_issue(&server, &active_id);
    assert_status(&archived, 200, "A11 Arrange 归档活跃态 issue");
    assert!(
        archived_at_of(&archived.json).is_some(),
        "[A11 Arrange] 归档后 archived_at 必须非空：{}",
        archived.describe()
    );
    let hidden = ids_of(&rows(&list_issues(&server, "project=p1"), "A11 归档后默认列表"));
    assert!(
        !hidden.contains(&active_id),
        "[A11 Arrange] 归档后默认列表不得含该 issue"
    );

    // Act：上报新事件（同指纹、不同 event_id）
    let second = ingest_event(&server, &active_payload("evt-a11-2"));

    // Assert 1：ingest 响应零变更（C2.5 硬约束）
    assert_eq!(
        second.str_field("issue_id"),
        active_id,
        "[A11/C2.5] 同指纹新事件必须归属同一 issue"
    );
    assert_eq!(
        second.json["duplicate"],
        json!(false),
        "[A11/C2.5] 新 event_id 不得计 duplicate：{}",
        second.describe()
    );
    assert_eq!(
        second.str_field("result"),
        "created",
        "[A11/C2.5] 解档不得改变 ingest 响应字段：{}",
        second.describe()
    );
    assert_eq!(
        second.str_field("event_id"),
        "evt-a11-2",
        "[A11/C2.5] 响应必须回显 event_id"
    );

    // Assert 2：归档状态被清除（archived_at=null、archive_restored_at=null），默认列表重新可见
    let reopened = issue_detail(&server, &active_id);
    assert_eq!(
        reopened.str_field("status"),
        "unresolved",
        "[A11/C2.5] 解档不得改写 status：{}",
        reopened.describe()
    );
    assert_eq!(
        archived_at_of(&reopened.json),
        None,
        "[A11/C2.5] 活跃态 issue 收到新事件必须解档（archived_at=null）：{}",
        reopened.describe()
    );
    assert_eq!(
        restored_at_of(&reopened.json),
        None,
        "[A11/C2.5] 解档必须同时清空 archive_restored_at：{}",
        reopened.describe()
    );
    assert_eq!(
        reopened.i64_field("count"),
        2,
        "[A11/C2.5] 解档路径不得改变 count 递增语义（1→2）"
    );
    let visible = ids_of(&rows(&list_issues(&server, "project=p1"), "A11 解档后默认列表"));
    assert!(
        visible.contains(&active_id),
        "[A11/C2.5] 解档后必须重新在默认列表可见：{visible:?}"
    );

    // ── 场景 2：已归档的 ignored（非活跃态）issue 收到新事件 → 保持归档、count 仍 +1 ──
    let ignored_payload = |event_id: &str| {
        let mut body = full_v1_ingest_payload(event_id, "production", "A11 ignored");
        body["tags"] = json!({"project": "p1", "alertname": "issue-governance"});
        body["extra"] = json!({"project_path": "/workspace/p1"});
        body["error_type"] = json!("A11IgnoredError");
        body["exception"] = json!({
            "error_type": "A11IgnoredError",
            "value": "A11 ignored",
            "stacktrace": [{
                "filename": "fixtures/a11_ignored.py",
                "function": "fn_a11_ignored",
                "lineno": 1,
                "colno": 1,
                "in_app": true
            }]
        });
        body
    };
    let created = ingest_event(&server, &ignored_payload("evt-a11-3"));
    assert_status(&created, 201, "A11 Arrange ignored issue 首次上报");
    let ignored_id = created.str_field("issue_id");
    assert_ne!(
        ignored_id, active_id,
        "[Arrange] 不同指纹必须形成独立 issue（B-12：指纹不含 project，靠栈帧区分）"
    );
    set_issue_status(&server, &ignored_id, "ignored");
    let ignored_archived = archive_issue(&server, &ignored_id);
    assert_status(&ignored_archived, 200, "A11 Arrange 归档 ignored issue");
    assert!(
        archived_at_of(&ignored_archived.json).is_some(),
        "[A11 Arrange] ignored issue 归档后 archived_at 必须非空"
    );
    let count_before = issue_detail(&server, &ignored_id).i64_field("count");

    let again = ingest_event(&server, &ignored_payload("evt-a11-4"));
    assert_eq!(
        again.str_field("issue_id"),
        ignored_id,
        "[A11/C2.5] 同指纹新事件必须归属同一 issue"
    );

    let still_archived = issue_detail(&server, &ignored_id);
    assert_eq!(
        still_archived.str_field("status"),
        "ignored",
        "[A11/C2.5] ingest 不得改写 ignored 状态：{}",
        still_archived.describe()
    );
    assert!(
        archived_at_of(&still_archived.json).is_some(),
        "[A11/C2.5] ignored（非活跃态）已归档 issue 收到新事件必须保持归档：{}",
        still_archived.describe()
    );
    assert_eq!(
        still_archived.i64_field("count"),
        count_before + 1,
        "[A11/C2.5] 不拦截写入：count 仍必须 +1"
    );
}
