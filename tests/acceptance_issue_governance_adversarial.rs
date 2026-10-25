//! issue 治理契约**对抗破坏套件**（L2-AT 红军，wave-9 / feat:issue-governance）。
//!
//! 断言唯一依据：`.dev-team/contract-issue-governance.md`（A1-A10，Lead 冻结口径）。
//! 三族：① 畸形/边界 ② 确定性并发（Barrier + 固定迭代，零 sleep 猜时）③ 故障注入/降级。
//! 纪律：无 `#[ignore]`、无同义反复断言、不吞错。传输层失败记为 `status=0` 并由断言点名，
//! 绝不静默放过；每用例独立临时 SQLite 文件库 + 独立端口（harness 保证），零 crate 内部依赖。
//! 红相：被测能力当前为空桩（archive/stats 路由 panic），本套件预期全红（5xx / 缺字段 /
//! 传输失败）。待 Lead 在 `tests/Cargo.toml` 注册 `acceptance_issue_governance_adversarial`
//! target 后统一跑红相；本文件不含任何跳过或降级放行的开关。

#[path = "harness.rs"]
mod harness;

use harness::{full_v1_ingest_payload, Resp, Server, TEST_ADMIN_TOKEN, TEST_CLIENT_TOKEN};
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};

const ISSUES: &str = "/api/v1/issues";
const STATS: &str = "/api/v1/stats";
const INGEST_STATS: &str = "/api/v1/ingest/stats";

// ── 通用断言 / 请求 helper ──────────────────────────────────────────────────

fn header(resp: &Resp, name: &str) -> Option<String> {
    resp.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn preview(raw: &str) -> String {
    raw.chars().take(240).collect()
}

fn leaks_internals(raw: &str) -> bool {
    let low = raw.to_lowercase();
    ["sqlstate", "sqlite", "panicked", "backtrace"].iter().any(|n| low.contains(n))
}

/// 最强公共约束：无传输层失败、无 5xx、不泄露 SQL/panic 文本。
fn assert_clean(resp: &Resp, clause: &str) {
    assert_ne!(resp.status, 0, "[{clause}] 传输层失败（无 HTTP 状态码）：{}", preview(&resp.raw));
    assert!(resp.status < 500, "[{clause}] 不得出现 5xx：status={} body={}", resp.status, preview(&resp.raw));
    assert!(!leaks_internals(&resp.raw), "[{clause}] 响应泄露 SQL/panic 内部细节：{}", preview(&resp.raw));
}

fn archived_at(resp: &Resp) -> Option<String> {
    resp.json.get("archived_at").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// 原样发请求，把传输层错误折叠为 `status = 0` 的收据（禁止 panic 掩盖并发红相）。
fn call_abs(url: &str, method: Method, headers: &[(&str, &str)], body: Option<&Value>) -> Resp {
    let client = harness::http_client();
    let mut request = client.request(method, url);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    match request.send() {
        Ok(response) => {
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let raw = response.text().unwrap_or_default();
            let json = serde_json::from_str(&raw).unwrap_or(Value::Null);
            Resp { status, raw, json, headers }
        }
        Err(err) => Resp {
            status: 0,
            raw: format!("transport error: {err}"),
            json: Value::Null,
            headers: reqwest::header::HeaderMap::new(),
        },
    }
}

fn admin_get(server: &Server, path: &str) -> Resp {
    call_abs(&server.url(path), Method::GET, &[("x-admin-token", TEST_ADMIN_TOKEN)], None)
}

fn admin_post(server: &Server, path: &str, body: Option<&Value>) -> Resp {
    call_abs(&server.url(path), Method::POST, &[("x-admin-token", TEST_ADMIN_TOKEN)], body)
}

fn admin_patch(server: &Server, path: &str, body: &Value) -> Resp {
    call_abs(&server.url(path), Method::PATCH, &[("x-admin-token", TEST_ADMIN_TOKEN)], Some(body))
}

fn ingest(server: &Server, payload: &Value) -> Resp {
    call_abs(&server.url("/api/v1/ingest"), Method::POST, &[("x-client-token", TEST_CLIENT_TOKEN)], Some(payload))
}

fn post_ingest_at(base: &str, payload: &Value) -> Resp {
    call_abs(&format!("{base}/api/v1/ingest"), Method::POST, &[("x-client-token", TEST_CLIENT_TOKEN)], Some(payload))
}

// ── 造数与编码 helper ───────────────────────────────────────────────────────

fn seed_issue(server: &Server, project: &str, message: &str, event_id: &str) -> String {
    let mut payload = full_v1_ingest_payload(event_id, "production", message);
    payload["tags"] = json!({"project": project, "alertname": "l2at-adversarial"});
    payload["extra"] = json!({"project_path": format!("/workspace/{project}")});
    let resp = ingest(server, &payload);
    assert_eq!(resp.status, 201, "[Arrange] ingest 造数必须 201：{}", resp.describe());
    resp.json["issue_id"].as_str().unwrap_or_else(|| panic!("[Arrange] 缺 issue_id：{}", resp.describe())).to_string()
}

fn seed_ignored_issue(server: &Server, project: &str, message: &str, event_id: &str) -> String {
    let issue_id = seed_issue(server, project, message, event_id);
    let resp = admin_patch(server, &format!("{ISSUES}/{issue_id}"), &json!({"status": "ignored"}));
    assert_eq!(resp.status, 200, "[Arrange] PATCH ignored 必须 200：{}", resp.describe());
    issue_id
}

/// 形如 UUID 但必然不存在的 id（确定性，不含随机源）。
fn fake_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012x}")
}

/// RFC 4648 §5 base64url（无 padding），避免为测试包新增依赖。
fn b64url(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((chunk[0] as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        }
    }
    out
}

// ══ ① 畸形 / 边界 ══════════════════════════════════════════════════════════

/// 事件分页 `limit` 边界：`0`/负值 400；超大值 clamp 到 100（A6）。
#[test]
fn malformed_event_limit_matrix() {
    let server = Server::start();
    let issue_id = seed_issue(&server, "l2at-malformed-limit", "limit matrix", "evt-l2at-limit-1");

    for bad in ["0", "-1"] {
        let resp = admin_get(&server, &format!("{ISSUES}/{issue_id}/events?limit={bad}"));
        assert_eq!(resp.status, 400, "[events limit={bad}] 必须 400：{}", resp.describe());
        assert!(serde_json::from_str::<Value>(&resp.raw).is_ok(),
            "[events limit={bad}] 400 响应体必须是合法 JSON：{}", preview(&resp.raw));
        assert_clean(&resp, "events limit=bad");
    }

    let resp = admin_get(&server, &format!("{ISSUES}/{issue_id}/events?limit=999999"));
    assert_eq!(resp.status, 200, "[events limit=999999] 必须 200：{}", resp.describe());
    assert_eq!(header(&resp, "x-limit").as_deref(), Some("100"),
        "[events limit=999999] X-Limit 必须 clamp 为 100：{:?}", header(&resp, "x-limit"));
    assert!(serde_json::from_str::<Value>(&resp.raw).is_ok(),
        "[events limit=999999] 响应体必须是合法 JSON：{}", preview(&resp.raw));
    assert_clean(&resp, "events limit clamp");
}

/// 事件游标四类畸形：非 base64 / 可解码但非 JSON / JSON 缺 `id` / 超 512 字符 → 400 invalid cursor（A7）。
#[test]
fn malformed_event_cursor_matrix() {
    let server = Server::start();
    let issue_id = seed_issue(&server, "l2at-malformed-cursor", "cursor matrix", "evt-l2at-cursor-1");
    let missing_id = json!({"created_at": "2026-10-21T07:00:00Z"}).to_string();
    let cases: [(&str, String); 4] = [
        ("not-base64", "!!!not-base64!!!".to_string()),
        ("base64-not-json", b64url(b"hello")),
        ("json-missing-id", b64url(missing_id.as_bytes())),
        ("over-512", "a".repeat(513)),
    ];

    for (label, cursor) in cases {
        let resp = admin_get(&server, &format!("{ISSUES}/{issue_id}/events?cursor={cursor}"));
        assert_eq!(resp.status, 400, "[cursor {label}] 必须 400：{}", resp.describe());
        assert!(resp.raw.to_lowercase().contains("invalid cursor"),
            "[cursor {label}] 400 语义必须是 invalid cursor：{}", preview(&resp.raw));
        assert_clean(&resp, "cursor-malformed");
    }
}

/// 批量归档畸形矩阵：超量 ids / 非白名单 statuses / 负 older_than_days / 70KB body / 1000 个不存在 id（A5）。
#[test]
fn malformed_archive_batch_matrix() {
    let server = Server::start();
    let archive_path = format!("{ISSUES}/archive");

    let too_many: Vec<String> = (0..1001).map(fake_id).collect();
    let resp = admin_post(&server, &archive_path, Some(&json!({"ids": too_many})));
    assert_eq!(resp.status, 400, "[ids=1001] 必须 400：{}", resp.describe());
    assert!(resp.raw.contains("too many ids"), "[ids=1001] 必须报 too many ids：{}", preview(&resp.raw));
    assert_clean(&resp, "archive too many ids");

    let resp = admin_post(&server, &archive_path, Some(&json!({"statuses": ["unresolved"]})));
    assert_eq!(resp.status, 400, "[statuses=unresolved] 非白名单必须 400：{}", resp.describe());
    assert_clean(&resp, "archive non-whitelist status");

    let resp = admin_post(&server, &archive_path, Some(&json!({"older_than_days": -1})));
    assert_eq!(resp.status, 400, "[older_than_days=-1] 必须 400：{}", resp.describe());
    assert_clean(&resp, "archive negative older_than_days");

    let oversized = json!({"ids": [fake_id(0)], "pad": "x".repeat(70 * 1024)});
    let resp = admin_post(&server, &archive_path, Some(&oversized));
    assert_eq!(resp.status, 413, "[body=70KB] 必须 413：status={} body={}", resp.status, preview(&resp.raw));

    let missing: Vec<String> = (0..1000).map(fake_id).collect();
    let resp = admin_post(&server, &archive_path, Some(&json!({"ids": missing})));
    assert_eq!(resp.status, 200, "[ids=1000 missing] 必须 200（幂等跳过）：{}", resp.describe());
    assert_eq!(resp.json["archived"].as_i64(), Some(0), "[ids=1000 missing] archived 必须为 0：{}", resp.describe());
    assert_eq!(resp.json["skipped_missing"].as_i64(), Some(1000),
        "[ids=1000 missing] skipped_missing 必须为 1000：{}", resp.describe());
    assert_clean(&resp, "archive 1000 missing");
}

// ══ ② 确定性并发（Barrier + 固定迭代）══════════════════════════════════════

/// 8 线程 × 20 轮并发归档同一 ignored issue：全 200，`archived_at` 唯一且与最终详情一致（A4 幂等）。
#[test]
fn concurrent_single_archive_is_idempotent() {
    let server = Server::start();
    let issue_id = seed_ignored_issue(&server, "l2at-concurrent-archive", "archive race", "evt-l2at-race-archive");
    let url = server.url(&format!("{ISSUES}/{issue_id}/archive"));
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for thread in 0..8usize {
        let url = url.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut out = Vec::new();
            for iteration in 0..20usize {
                barrier.wait();
                let resp = call_abs(&url, Method::POST, &[("x-admin-token", TEST_ADMIN_TOKEN)], None);
                out.push((thread, iteration, resp.status, archived_at(&resp), preview(&resp.raw)));
            }
            out
        }));
    }

    let mut stamps = Vec::new();
    for handle in handles {
        for (thread, iteration, status, stamp, raw) in handle.join().expect("并发归档线程 panic") {
            assert_eq!(status, 200, "[thread {thread} iter {iteration}] 并发重复归档必须 200：{raw}");
            if let Some(value) = stamp {
                stamps.push(value);
            }
        }
    }
    assert!(!stamps.is_empty(), "所有 160 次归档响应均缺 archived_at 字段（幂等语义无法验证）");
    let first = stamps[0].clone();
    assert!(stamps.iter().all(|stamp| *stamp == first), "并发归档返回的 archived_at 不一致：{:?}", stamps);

    let detail = admin_get(&server, &format!("{ISSUES}/{issue_id}"));
    assert_eq!(detail.status, 200, "归档后详情必须 200：{}", detail.describe());
    assert_eq!(archived_at(&detail).as_deref(), Some(first.as_str()),
        "最终 archived_at 必须等于并发响应中的值：{}", detail.describe());
}

/// 8 线程 × 5 轮并发同一 `event_id` 的 ingest：恰好 1 次 201 created、其余 200 duplicate，
/// 且该 issue 事件总行数为 1（去重击穿为零）。
#[test]
fn concurrent_duplicate_ingest_is_exactly_once() {
    let server = Server::start();
    let base = server.base_url.clone();
    let payload = Arc::new(full_v1_ingest_payload("evt-l2at-dedupe", "production", "dedupe race"));
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for thread in 0..8usize {
        let base = base.clone();
        let payload = Arc::clone(&payload);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut out = Vec::new();
            for iteration in 0..5usize {
                barrier.wait();
                let resp = post_ingest_at(&base, &payload);
                let issue_id = resp.json["issue_id"].as_str().map(str::to_string);
                out.push((thread, iteration, resp.status, issue_id, preview(&resp.raw)));
            }
            out
        }));
    }

    let mut created = 0usize;
    let mut duplicate = 0usize;
    let mut issue_id: Option<String> = None;
    for handle in handles {
        for (thread, iteration, status, id, raw) in handle.join().expect("并发 ingest 线程 panic") {
            match status {
                201 => {
                    created += 1;
                    assert!(raw.contains("\"created\""), "[thread {thread} iter {iteration}] 201 语义必须是 created：{raw}");
                }
                200 => {
                    duplicate += 1;
                    assert!(raw.contains("\"duplicate\""), "[thread {thread} iter {iteration}] 200 语义必须是 duplicate：{raw}");
                }
                other => panic!("[thread {thread} iter {iteration}] 并发 ingest 只允许 201/200，实际 {other}：{raw}"),
            }
            if issue_id.is_none() {
                issue_id = id;
            }
        }
    }
    assert_eq!(created, 1, "同一 event_id 必须恰好 1 次 created，实际 {created}");
    assert_eq!(duplicate, 39, "其余 39 次必须全部 duplicate，实际 {duplicate}");

    let issue_id = issue_id.expect("并发 ingest 响应缺 issue_id");
    let events = admin_get(&server, &format!("{ISSUES}/{issue_id}/events?limit=1"));
    assert_eq!(events.status, 200, "事件列表必须 200：{}", events.describe());
    assert_eq!(header(&events, "x-total-count").as_deref(), Some("1"),
        "去重后事件总行数必须为 1：{:?}", header(&events, "x-total-count"));
}

/// 4 线程 × 10 轮交替 `GET /issues` + `GET /stats` + 归档/解档：只允许 200/404，
/// 且 `X-Total-Count >= items.len()`（分页头与载荷不得自相矛盾）。
#[test]
fn concurrent_read_and_toggle_never_5xx() {
    let server = Server::start();
    let mut ids = Vec::new();
    for index in 0..3usize {
        ids.push(seed_ignored_issue(&server, "l2at-concurrent-mixed", &format!("mixed race {index}"),
            &format!("evt-l2at-mixed-{index}")));
    }
    let base = server.base_url.clone();
    let ids = Arc::new(ids);
    let barrier = Arc::new(Barrier::new(4));

    let mut handles = Vec::new();
    for thread in 0..4usize {
        let base = base.clone();
        let ids = Arc::clone(&ids);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut violations = Vec::new();
            for iteration in 0..10usize {
                barrier.wait();
                let list = call_abs(&format!("{base}{ISSUES}?envelope=1&limit=5"), Method::GET,
                    &[("x-admin-token", TEST_ADMIN_TOKEN)], None);
                if list.status != 200 {
                    violations.push(format!("list status={} body={}", list.status, preview(&list.raw)));
                } else {
                    let items = list.json["items"].as_array().cloned().unwrap_or_default();
                    match header(&list, "x-total-count").and_then(|v| v.parse::<usize>().ok()) {
                        Some(total) if total >= items.len() => {}
                        other => violations.push(format!("X-Total-Count={other:?} < items.len()={}", items.len())),
                    }
                }

                let stats = call_abs(&format!("{base}{STATS}"), Method::GET,
                    &[("x-admin-token", TEST_ADMIN_TOKEN)], None);
                if stats.status != 200 && stats.status != 404 {
                    violations.push(format!("stats status={} body={}", stats.status, preview(&stats.raw)));
                }
                if leaks_internals(&stats.raw) || leaks_internals(&list.raw) {
                    violations.push("列表/统计响应泄露内部细节".to_string());
                }

                let target = &ids[(thread + iteration) % ids.len()];
                let action = if iteration % 2 == 0 { "archive" } else { "unarchive" };
                let toggle = call_abs(&format!("{base}{ISSUES}/{target}/{action}"), Method::POST,
                    &[("x-admin-token", TEST_ADMIN_TOKEN)], None);
                if toggle.status != 200 && toggle.status != 404 {
                    violations.push(format!("{action} status={} body={}", toggle.status, preview(&toggle.raw)));
                }
            }
            violations
        }));
    }

    let mut violations = Vec::new();
    for handle in handles {
        violations.extend(handle.join().expect("混跑线程 panic"));
    }
    assert!(violations.is_empty(), "并发读写/切换出现违规响应（{} 条）：{:#?}", violations.len(), violations);
}

// ══ ③ 故障注入 / 降级 ═════════════════════════════════════════════════════

/// 全新空 SQLite 库（服务自建 schema）上统计端点必须 200 且 `totals.received == 0`，
/// 不得因零数据而 5xx / panic（A8）。
#[test]
fn fresh_empty_database_stats_are_zero() {
    let server = Server::start();

    let ingest_stats = admin_get(&server, INGEST_STATS);
    assert_eq!(ingest_stats.status, 200, "[fresh /ingest/stats] 空库必须 200：{}", ingest_stats.describe());
    assert_eq!(ingest_stats.json["totals"]["received"].as_i64(), Some(0),
        "[fresh /ingest/stats] totals.received 必须为 0：{}", ingest_stats.describe());

    let stats = admin_get(&server, STATS);
    assert_eq!(stats.status, 200, "[fresh /stats] 空库必须 200：{}", stats.describe());
    assert_eq!(stats.json["totals"]["received"].as_i64(), Some(0),
        "[fresh /stats] totals.received 必须为 0：{}", stats.describe());
}

/// [FAULT-INJECTION: DEGRADED-EQUIVALENT]
/// 直接 `DROP TABLE ingest_metrics` 需要被测库文件路径；`harness::Server` 只暴露 `base_url`，
/// workdir 私有且无跨进程 SQL 接缝，故不改 harness 的前提下无法真删表。退化为契约可测的等价
/// 断言：治理指标旁路（读/写）失败只允许降级——200 次 ingest 全部 2xx、进程存活
/// （`/healthz` == 200），绝不因旁路异常把主链路拖成 5xx。
#[test]
fn sustained_ingest_burst_survives_metrics_path_degradation() {
    let server = Server::start();
    let base = server.base_url.clone();
    let mut created = 0usize;
    let mut duplicate = 0usize;

    for index in 0..200usize {
        // 偶数序号独占 event_id（created），奇数序号复用同一 event_id（duplicate）。
        let event_id = if index % 2 == 0 {
            format!("evt-l2at-burst-{index}")
        } else {
            "evt-l2at-burst-dup".to_string()
        };
        let payload = full_v1_ingest_payload(&event_id, "production", "burst probe");
        let resp = post_ingest_at(&base, &payload);
        assert_clean(&resp, "ingest burst");
        assert!(resp.status == 201 || resp.status == 200,
            "[burst iter {index}] ingest 只允许 201/200：{}", resp.describe());
        if resp.status == 201 { created += 1 } else { duplicate += 1 }
    }

    assert!(created > 0 && duplicate > 0,
        "混合负载未覆盖 created/duplicate：created={created} duplicate={duplicate}");
    let health = call_abs(&format!("{base}/healthz"), Method::GET, &[], None);
    assert_eq!(health.status, 200, "[burst 后] 进程必须存活且 /healthz == 200：{}", health.describe());
}

/// 3MB ingest payload → 413 且进程不被撑死（随后 `/healthz` == 200）；
/// 1MB 合法 payload → 201 或 400（契约内码），不得 5xx。
#[test]
fn oversized_payload_rejected_without_killing_process() {
    let server = Server::start();
    let base = server.base_url.clone();

    let mut huge = full_v1_ingest_payload("evt-l2at-huge", "production", "huge payload");
    huge["message"] = json!("x".repeat(3 * 1024 * 1024));
    let resp = post_ingest_at(&base, &huge);
    assert_eq!(resp.status, 413, "[3MB payload] 必须 413 拒绝：status={} body={}", resp.status, preview(&resp.raw));

    let health = call_abs(&format!("{base}/healthz"), Method::GET, &[], None);
    assert_eq!(health.status, 200, "[3MB 之后] 进程必须存活：{}", health.describe());

    let mut big = full_v1_ingest_payload("evt-l2at-1mb", "production", "one megabyte payload");
    big["message"] = json!("y".repeat(1024 * 1024));
    let resp = post_ingest_at(&base, &big);
    assert_clean(&resp, "1MB payload");
    assert!(resp.status == 201 || resp.status == 400,
        "[1MB payload] 只允许契约内 201/400：{}", resp.describe());
}
