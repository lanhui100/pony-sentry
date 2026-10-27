//! 读侧 `trace_id` 透出的 HTTP 契约（`GET /api/v1/issues` 与 `GET /api/v1/issues/:id`）。
//!
//! 断言三件事，均为「键存在性」级的硬要求：
//! 1. 历史形态（只放 `extra.trace_id`）的 issue → 列表/详情能读到该值；
//! 2. 新形态（一级 `trace_id`）→ 同值透出，且 payload 里落下了一级键（ingest 侧归一）；
//! 3. 从未带过 trace_id 的 issue → `trace_id` 键**仍然存在**且为 `null`（不得省略键）。

use axum::http::StatusCode;
use axum_test::TestServer;
use pony_sentry_core::{create_pool, SqliteIssueRepository};
use pony_sentry_server::{create_app_with_state, AppState};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const ADMIN_TOKEN: &str = "test-admin-token";
const LEGACY_TRACE: &str = "0123456789abcdef0123456789abcdef";
const MODERN_TRACE: &str = "fedcba9876543210fedcba9876543210";
const NEWEST_TRACE: &str = "aaaabbbbccccddddeeeeffff00001111";

async fn test_server() -> TestServer {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();
    let state = AppState {
        repo,
        trace_repo: None,
        client_token: None,
        admin_token: Some(ADMIN_TOKEN.into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        notifier: std::sync::Arc::default(),
        sessions: Default::default(),
    };
    TestServer::new(create_app_with_state(state)).unwrap()
}

async fn ingest(server: &TestServer, payload: Value) -> String {
    let resp = server.post("/api/v1/ingest").json(&payload).await;
    resp.assert_status(StatusCode::CREATED);
    let body: Value = resp.json();
    body["issue_id"]
        .as_str()
        .expect("ingest 响应必须回显 issue_id")
        .to_string()
}

async fn list_issues(server: &TestServer) -> Vec<Value> {
    server
        .get("/api/v1/issues")
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await
        .json()
}

fn by_id(items: &[Value]) -> HashMap<String, Value> {
    items
        .iter()
        .map(|item| {
            (
                item["id"].as_str().expect("issue 必须带 id").to_string(),
                item.clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn issue_list_and_detail_expose_trace_id() {
    let server = test_server().await;

    // 1) 历史形态：只放 extra.trace_id（发送侧既有契约）
    let legacy_payload = json!({
        "platform": "python",
        "environment": "production",
        "message": "legacy extra-only trace",
        "exception": {
            "error_type": "LegacyTraceError",
            "value": "boom",
            "stacktrace": []
        },
        "extra": { "trace_id": LEGACY_TRACE }
    });
    let legacy_id = ingest(&server, legacy_payload.clone()).await;

    // 2) 新形态：一级 trace_id（本次新增的 ingest 字段）
    let modern_payload = json!({
        "platform": "python",
        "environment": "production",
        "message": "modern top-level trace",
        "trace_id": MODERN_TRACE,
        "exception": {
            "error_type": "ModernTraceError",
            "value": "boom",
            "stacktrace": []
        }
    });
    let modern_id = ingest(&server, modern_payload).await;

    // 3) 完全没有 trace_id 的 issue：键必须存在且为 null
    let plain_id = ingest(
        &server,
        json!({
            "platform": "rust",
            "environment": "production",
            "message": "no trace at all",
            "exception": {
                "error_type": "NoTraceError",
                "value": "boom",
                "stacktrace": []
            }
        }),
    )
    .await;

    // —— 列表：每个对象都必须带 trace_id 键 ——
    let items = list_issues(&server).await;
    assert_eq!(items.len(), 3, "三个不同指纹必须产生三个 issue：{items:?}");
    for item in &items {
        assert!(
            item.get("trace_id").is_some(),
            "列表每个 issue 对象必须含 trace_id 键（无值为 null）：{item}"
        );
    }
    let listed = by_id(&items);
    assert_eq!(
        listed[&legacy_id]["trace_id"],
        json!(LEGACY_TRACE),
        "历史形态 extra.trace_id 必须透出到列表"
    );
    assert_eq!(
        listed[&modern_id]["trace_id"],
        json!(MODERN_TRACE),
        "新形态一级 trace_id 必须透出到列表"
    );
    assert_eq!(
        listed[&plain_id]["trace_id"],
        Value::Null,
        "无 trace_id 的 issue 必须是 null（不得省略键）"
    );

    // —— 详情：与列表同源，且既有派生字段 error_type 不受影响 ——
    for (issue_id, expected_trace, expected_error_type) in [
        (&legacy_id, json!(LEGACY_TRACE), "LegacyTraceError"),
        (&modern_id, json!(MODERN_TRACE), "ModernTraceError"),
        (&plain_id, Value::Null, "NoTraceError"),
    ] {
        let detail: Value = server
            .get(&format!("/api/v1/issues/{issue_id}"))
            .add_header("x-admin-token", ADMIN_TOKEN)
            .await
            .json();
        assert!(
            detail.get("trace_id").is_some(),
            "详情必须含 trace_id 键：{detail}"
        );
        assert_eq!(
            detail["trace_id"], expected_trace,
            "详情 trace_id 不符：{detail}"
        );
        assert_eq!(
            detail["error_type"],
            json!(expected_error_type),
            "既有派生字段 error_type 不得回归：{detail}"
        );
    }

    // —— ingest 侧归一：新形态的一级 trace_id 必须随 payload 落库 ——
    let events: Vec<Value> = server
        .get(&format!("/api/v1/issues/{modern_id}/events"))
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await
        .json();
    assert_eq!(
        events[0]["payload"]["trace_id"],
        json!(MODERN_TRACE),
        "一级 trace_id 必须落进 payload（不依赖 extra）：{}",
        events[0]
    );

    // —— 「最新事件」语义：同 issue 再来一条不同 trace_id 的事件 → 列表取最新 ——
    let mut newest_payload = legacy_payload;
    newest_payload["extra"] = json!({ "trace_id": NEWEST_TRACE });
    let same_issue_id = ingest(&server, newest_payload).await;
    assert_eq!(
        same_issue_id, legacy_id,
        "同指纹的再次上报必须归并到同一 issue（否则「最新事件」断言无意义）"
    );
    let listed_after = by_id(&list_issues(&server).await);
    assert_eq!(
        listed_after[&legacy_id]["trace_id"],
        json!(NEWEST_TRACE),
        "列表必须透出该 issue 最新事件的 trace_id"
    );
    assert_eq!(
        listed_after[&legacy_id]["count"], 2,
        "同 issue 事件计数应为 2：{}",
        listed_after[&legacy_id]
    );

    // —— envelope 形态同样必须带 trace_id（两种响应形状共用同一份字段）——
    let envelope: Value = server
        .get("/api/v1/issues?envelope=1")
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await
        .json();
    for item in envelope["items"]
        .as_array()
        .expect("envelope.items 必须是数组")
    {
        assert!(
            item.get("trace_id").is_some(),
            "envelope 模式的每个 issue 对象同样必须含 trace_id 键：{item}"
        );
    }
}
