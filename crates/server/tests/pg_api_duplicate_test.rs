//! PostgreSQL 端到端幂等回归（HTTP 层）：同一 `event_id` 重复 ingest 必须 201 → 200 duplicate，
//! 且进程不得 panic（缺陷现场：`crates/core/src/pg_repository.rs` 的 JSONB→TEXT 误解码导致空回复）。
//!
//! 需要 `PONY_TEST_PG_URL` 指向可写临时测试库；未设置时跳过。

use axum::http::StatusCode;
use axum_test::TestServer;
use pony_sentry_core::PgIssueRepository;
use pony_sentry_server::{create_app_with_state, AppState};
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const CLIENT_TOKEN: &str = "pg-test-client-token";
const ADMIN_TOKEN: &str = "pg-test-admin-token";

async fn server() -> Option<TestServer> {
    let url = match std::env::var("PONY_TEST_PG_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("SKIP: PONY_TEST_PG_URL 未设置，跳过 PG HTTP 回归用例");
            return None;
        }
    };
    let base: PgConnectOptions = url.parse().expect("PONY_TEST_PG_URL 不是合法 PG 连接串");
    let schema = "ponysentry_test_api_dup";

    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(base.clone())
        .await
        .expect("连接 PONY_TEST_PG_URL 失败");
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin_pool)
        .await
        .expect("DROP SCHEMA 失败");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin_pool)
        .await
        .expect("CREATE SCHEMA 失败");

    let scoped = base.options([("search_path", schema.to_string())]);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(scoped)
        .await
        .expect("连接隔离 schema 失败");
    let repo = std::sync::Arc::new(PgIssueRepository::new(pool));
    repo.migrate().await.expect("migrate 失败");

    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo),
        client_token: Some(CLIENT_TOKEN.into()),
        admin_token: Some(ADMIN_TOKEN.into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
    };
    Some(TestServer::new(create_app_with_state(state)).unwrap())
}

fn payload(event_id: &str) -> serde_json::Value {
    json!({
        "platform": "python",
        "release": "0.24.35",
        "environment": "production",
        "message": "pg 幂等回归",
        "level": "error",
        "event_id": event_id,
        "namespace": "production",
        "app": "api_v2",
        "component": "agent",
        "service_name": "langgraph",
        "error_type": "ValidationError",
        "source_kind": "log",
        "exception": {
            "error_type": "ValidationError",
            "value": "boom",
            "stacktrace": "File \"api_v2/src/x.py\", line 12, in run"
        },
        "tags": {"project": "job_copilot"}
    })
}

#[tokio::test]
async fn duplicate_event_id_is_200_on_pg_without_panic() {
    let Some(server) = server().await else {
        return;
    };

    let first = server
        .post("/api/v1/ingest")
        .add_header("x-client-token", CLIENT_TOKEN)
        .json(&payload("pg-evt-dup-1"))
        .await;
    first.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = first.json();
    assert_eq!(body["result"], "created");
    assert_eq!(body["duplicate"], false);
    let issue_id = body["issue_id"].as_str().unwrap().to_string();

    // 缺陷现场：此处曾 panic（ColumnDecode payload JSONB ≠ TEXT）→ 空回复 → worker 重试 → quarantine
    let second = server
        .post("/api/v1/ingest")
        .add_header("x-client-token", CLIENT_TOKEN)
        .json(&payload("pg-evt-dup-1"))
        .await;
    second.assert_status(StatusCode::OK);
    let body2: serde_json::Value = second.json();
    assert_eq!(body2["result"], "duplicate");
    assert_eq!(body2["duplicate"], true);
    assert_eq!(body2["issue_id"], issue_id);
    assert_eq!(body2["count"], 1, "duplicate 不得递增 count");

    // 进程仍存活且服务正常
    server.get("/healthz").await.assert_status(StatusCode::OK);

    // events 仅一行，且载荷可读（JSONB 解码路径）
    let events = server
        .get(&format!("/api/v1/issues/{issue_id}/events"))
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await;
    events.assert_status(StatusCode::OK);
    let rows: Vec<serde_json::Value> = events.json();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["payload"]["environment"], "production");
    assert_eq!(
        rows[0]["payload"]["extra"]["stacktrace_text"],
        "File \"api_v2/src/x.py\", line 12, in run"
    );
    assert_eq!(rows[0]["event_key"], "pg-evt-dup-1");
}
