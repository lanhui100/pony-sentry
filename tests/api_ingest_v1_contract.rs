//! PonySentry ingest v1 契约级黑盒验收测试（红相）。
//!
//! 断言唯一依据：`.dev-team/contracts-pony-sentry-migration.md` 冻结 v1.1
//! - R2 ingest v1 字段全集 / 截断 / 非法 level 降级 warning
//! - R3 stacktrace 双形态（Frame[] 与字符串 → extra.stacktrace_text）
//! - R4 响应契约与幂等语义（201 created / 200 duplicate，count 与 last_seen_at 不推进）
//! - R6 指纹 v3 含 environment，同签名跨环境形成两条独立 issue
//!
//! 红相语义：被测二进制能力缺失时必须表现为断言失败，而非编译/启动失败。

#[path = "harness.rs"]
mod harness;

use harness::{full_v1_ingest_payload, Resp, Server};

fn assert_status(resp: &Resp, expected: u16, contract_clause: &str) {
    assert_eq!(
        resp.status,
        expected,
        "[{contract_clause}] 期望 HTTP {expected}，实际 {}",
        resp.describe()
    );
}

/// 读取 issue 详情（R10：需 ADMIN_TOKEN）。
fn issue_detail(server: &Server, issue_id: &str) -> Resp {
    let resp = server.get_admin(&format!("/api/v1/issues/{issue_id}"));
    assert_status(&resp, 200, "R10 GET /api/v1/issues/:id 带 admin token");
    resp
}

/// R2 + R3：ingest v1 扩展字段全集必须被接受并可在 issue 读回；stacktrace 双形态均可用。
#[test]
fn r2_r3_ingest_accepts_all_v1_fields_and_both_stacktrace_shapes() {
    // Arrange
    let server = Server::start();

    // Act 1: Frame[] 形态
    let payload = full_v1_ingest_payload("evt-contract-r2-0001", "dev", "ValueError: 字段全集验收");
    let created = server.post_ingest_client(&payload);

    // Assert 1（R4：首次写入 201 created）
    assert_status(&created, 201, "R4 首次写入必须 201");
    assert_eq!(
        created.field("duplicate"),
        &serde_json::Value::Bool(false),
        "[R4] 首次写入 duplicate 必须为 false：{}",
        created.describe()
    );
    assert_eq!(
        created.str_field("result"),
        "created",
        "[R4] result 必须为 created：{}",
        created.describe()
    );
    assert_eq!(
        created.str_field("event_id"),
        "evt-contract-r2-0001",
        "[R2] 响应必须回显客户端 event_id：{}",
        created.describe()
    );
    let issue_id = created.str_field("issue_id");
    assert!(
        !issue_id.is_empty(),
        "[R4] issue_id 不得为空：{}",
        created.describe()
    );

    // Assert 2（R2）：全部扩展字段必须可在 issue 读回
    let detail = issue_detail(&server, &issue_id);
    assert_eq!(
        detail.str_field("environment"),
        "dev",
        "[R2/R5] issue.environment 必须等于上报 environment：{}",
        detail.describe()
    );
    assert_eq!(
        detail.str_field("level"),
        "error",
        "[R2/R5] issue.level 必须等于上报 level：{}",
        detail.describe()
    );
    assert_eq!(
        detail.str_field("app"),
        "api_v2",
        "[R2/R5] issue.app 必须等于上报 app：{}",
        detail.describe()
    );
    assert_eq!(
        detail.str_field("namespace"),
        "dev",
        "[R2/R5] issue.namespace 必须等于上报 namespace：{}",
        detail.describe()
    );
    assert_eq!(
        detail.str_field("component"),
        "issue_reporting",
        "[R2/R5] issue.component 必须等于上报 component：{}",
        detail.describe()
    );
    assert_eq!(
        detail.str_field("source_kind"),
        "agent",
        "[R2/R5] issue.source_kind 必须等于上报 source_kind：{}",
        detail.describe()
    );
    assert_eq!(
        detail.str_field("error_type"),
        "ValidationError",
        "[R2] issue 必须保留一级 error_type：{}",
        detail.describe()
    );

    // Act 2: 字符串形态 stacktrace（R3 / R3.1）
    let mut string_payload = full_v1_ingest_payload(
        "evt-contract-r3-0002",
        "dev",
        "RuntimeError: 字符串栈帧形态",
    );
    let stacktrace_text =
        "Traceback (most recent call last):\n  File \"api_v2/src/modules/issue_reporting/sink.py\", line 42, in write_issue_sink\nRuntimeError: 字符串栈帧形态";
    string_payload["exception"] = serde_json::json!({
        "error_type": "RuntimeError",
        "value": "字符串栈帧形态",
        "stacktrace": stacktrace_text
    });
    let string_created = server.post_ingest_client(&string_payload);

    // Assert 3（R3）：字符串形态不得 4xx/5xx/panic
    assert_status(&string_created, 201, "R3 字符串 stacktrace 首次写入必须 201");
    let string_issue_id = string_created.str_field("issue_id");

    // Assert 4（R3）：字符串 MUST 落入 extra.stacktrace_text（≤5000 字符）
    let events = server.get_admin(&format!("/api/v1/issues/{string_issue_id}/events"));
    assert_status(&events, 200, "R10 GET /api/v1/issues/:id/events 带 admin token");
    let rows = events
        .json
        .as_array()
        .unwrap_or_else(|| panic!("[R3] events 必须是数组：{}", events.describe()));
    assert_eq!(
        rows.len(),
        1,
        "[R3] 单次上报必须恰好产生 1 条 event：{}",
        events.describe()
    );
    let stored = &rows[0]["payload"]["extra"]["stacktrace_text"];
    assert_eq!(
        stored.as_str(),
        Some(stacktrace_text),
        "[R3] 字符串 stacktrace 必须原样落入 extra.stacktrace_text：{}",
        events.describe()
    );

    // Assert 5：两种形态都不得使服务端崩坏（R3.1）
    let health = server.healthz();
    assert_status(&health, 200, "R11.2 stacktrace 双形态后服务仍健康");
}

/// R2：非法 level MUST 降级为 "warning"（不得 4xx，不得原样写入）。
#[test]
fn r2_invalid_level_degrades_to_warning() {
    // Arrange
    let server = Server::start();
    let mut payload = full_v1_ingest_payload("evt-contract-level-0003", "preprod", "非法 level 降级");
    payload["level"] = serde_json::json!("banana");

    // Act
    let resp = server.post_ingest_client(&payload);

    // Assert（R2 表：非法值不 4xx）
    assert_status(&resp, 201, "R2 非法 level 不得导致 4xx");
    let issue_id = resp.str_field("issue_id");
    let detail = issue_detail(&server, &issue_id);
    assert_eq!(
        detail.str_field("level"),
        "warning",
        "[R2] 非法 level 必须降级为 warning：{}",
        detail.describe()
    );
}

/// R4：同 event_id 重复投递 → 201 created 后 200 duplicate，count / last_seen_at 不推进。
#[test]
fn r4_duplicate_delivery_is_idempotent() {
    // Arrange
    let server = Server::start();
    let payload = full_v1_ingest_payload("evt-contract-idem-0004", "production", "TypeError: 幂等性验收");

    // Act 1: 首次投递
    let first = server.post_ingest_client(&payload);

    // Assert 1（R4）
    assert_status(&first, 201, "R4 首次投递必须 201 created");
    assert_eq!(
        first.field("duplicate"),
        &serde_json::Value::Bool(false),
        "[R4] 首次投递 duplicate 必须为 false：{}",
        first.describe()
    );
    let issue_id = first.str_field("issue_id");
    let after_first = issue_detail(&server, &issue_id);
    let count_after_first = after_first.i64_field("count");
    let last_seen_after_first = after_first.str_field("last_seen_at");

    // Act 2: 同 event_id 重复投递（模拟 outbox 重试）
    let second = server.post_ingest_client(&payload);

    // Assert 2（R4）
    assert_status(&second, 200, "R4 重复投递必须 200 duplicate");
    assert_eq!(
        second.field("duplicate"),
        &serde_json::Value::Bool(true),
        "[R4] 重复投递 duplicate 必须为 true：{}",
        second.describe()
    );
    assert_eq!(
        second.str_field("result"),
        "duplicate",
        "[R4] result 必须为 duplicate：{}",
        second.describe()
    );
    assert_eq!(
        second.str_field("issue_id"),
        issue_id,
        "[R4] 重复投递必须归属同一 issue：{}",
        second.describe()
    );

    // Assert 3（R4.1：count / last_seen_at 不推进，且不新增 events 行）
    let after_second = issue_detail(&server, &issue_id);
    assert_eq!(
        after_second.i64_field("count"),
        count_after_first,
        "[R4.1] duplicate 不得递增 issues.count：首次={count_after_first} 重复后={}",
        after_second.describe()
    );
    assert_eq!(
        after_second.str_field("last_seen_at"),
        last_seen_after_first,
        "[R4.1] duplicate 不得推进 issues.last_seen_at：首次={last_seen_after_first} 重复后={}",
        after_second.describe()
    );
    let events = server.get_admin(&format!("/api/v1/issues/{issue_id}/events"));
    assert_status(&events, 200, "R10 GET events 带 admin token");
    let rows = events
        .json
        .as_array()
        .unwrap_or_else(|| panic!("[R4.1] events 必须是数组：{}", events.describe()));
    assert_eq!(
        rows.len(),
        1,
        "[R4.1] duplicate 不得新增 events 行：{}",
        events.describe()
    );
}

/// R6：同签名不同 environment MUST 形成两条独立 issue；按 environment 过滤只返回该环境。
#[test]
fn r6_environment_scopes_fingerprint_and_filter() {
    // Arrange
    let server = Server::start();
    let message = "OperationalError: 同签名跨环境验收";

    // Act: 同一签名在 dev 与 production 各报一次
    let dev = server.post_ingest_client(&full_v1_ingest_payload("evt-contract-env-dev", "dev", message));
    let prod = server.post_ingest_client(&full_v1_ingest_payload(
        "evt-contract-env-prod",
        "production",
        message,
    ));

    // Assert 1（R6.2）：两条独立 issue
    assert_status(&dev, 201, "R6 dev 首次投递 201");
    assert_status(&prod, 201, "R6 production 首次投递 201");
    let dev_issue_id = dev.str_field("issue_id");
    let prod_issue_id = prod.str_field("issue_id");
    assert_ne!(
        dev_issue_id, prod_issue_id,
        "[R6.2] 同签名不同 environment 必须形成两条独立 issue，实际均为 {dev_issue_id}"
    );

    // Assert 2（R6：指纹 v3 含 environment → 指纹必须不同）
    let dev_fp = dev.str_field("fingerprint");
    let prod_fp = prod.str_field("fingerprint");
    assert_ne!(
        dev_fp, prod_fp,
        "[R6] 指纹 v3 必须含 environment，dev 与 production 指纹不得相同：{dev_fp}"
    );

    // Assert 3（R6.2 / R14.1）：?environment=dev 只返回该环境
    let filtered = server.get_admin("/api/v1/issues?environment=dev");
    assert_status(&filtered, 200, "R10 GET /api/v1/issues?environment= 带 admin token");
    let issues = filtered
        .json
        .as_array()
        .unwrap_or_else(|| panic!("[R6.2] issues 必须是数组：{}", filtered.describe()));
    assert!(
        issues
            .iter()
            .any(|issue| issue["id"].as_str() == Some(dev_issue_id.as_str())),
        "[R6.2] environment=dev 结果必须包含 dev issue {dev_issue_id}：{}",
        filtered.describe()
    );
    assert!(
        !issues
            .iter()
            .any(|issue| issue["id"].as_str() == Some(prod_issue_id.as_str())),
        "[R6.2] environment=dev 结果不得包含 production issue {prod_issue_id}：{}",
        filtered.describe()
    );
    for issue in issues {
        assert_eq!(
            issue["environment"].as_str(),
            Some("dev"),
            "[R6.2] environment=dev 过滤结果必须全部为 dev：{issue}"
        );
    }
}
