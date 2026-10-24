//! `PONY_TRACE_INGEST_ENABLED` 停收开关的进程内集成测试。
//!
//! 断言口径（B-OBS-6）：
//! 1. 关闭时 4 个 trace 路由一律 `410 Gone` + 固定响应体，且**处理器未执行**
//!    （用仓储直读证明没有写库、用 `/metrics` 证明没有递增 trace 指标）；
//! 2. 关闭时 `/healthz`、`POST /api/v1/ingest`、`GET /api/v1/issues`、`/metrics` 行为不变；
//! 3. 默认（未设置环境变量）时 4 个 trace 路由行为不变（201 / 200 / 200 / 200）。
//!
//! 开关经 `create_app_with_options` 显式注入，不依赖改进程环境变量——避免与其它测试并发互扰。

use axum::http::StatusCode;
use axum_test::TestServer;
use pony_sentry_core::{create_pool, SqliteIssueRepository, TraceFilter, TraceRepository};
use pony_sentry_server::{
    create_app_with_options, trace_ingest::TraceIngestSwitch, trust::TrustConfig, AppState,
};
use serde_json::{json, Value};
use std::sync::Arc;

const CLIENT_TOKEN: &str = "test-client-token";
const ADMIN_TOKEN: &str = "test-admin-token";
const TRACES: &str = "/api/v1/traces";

/// 一条最小合法 trace 上报体（`turns` 求和是 `total_*` 的单一真相）。
fn trace_payload() -> Value {
    json!({
        "thread_id": "jc_switch_test_thread",
        "project": "job_copilot",
        "environment": "production",
        "release": "1.0.0",
        "turns": [
            { "step": "agent_node", "input": "hello", "input_tokens": 10, "output_tokens": 5, "duration_ms": 100 }
        ],
        "total_input_tokens": 10,
        "total_output_tokens": 5,
        "total_duration_ms": 100
    })
}

/// 构造一套「既有 trace 仓储 + 两个静态令牌」的 AppState（与既有 api_test 同构）。
async fn build_state() -> (AppState, Arc<SqliteIssueRepository>) {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();
    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        client_token: Some(CLIENT_TOKEN.into()),
        admin_token: Some(ADMIN_TOKEN.into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        notifier: std::sync::Arc::default(),
        sessions: Default::default(),
    };
    (state, repo)
}

async fn app_with(switch: TraceIngestSwitch) -> (TestServer, Arc<SqliteIssueRepository>) {
    let (state, repo) = build_state().await;
    let app = create_app_with_options(state, TrustConfig::from_env(), switch);
    (TestServer::new(app).unwrap(), repo)
}

#[tokio::test]
async fn disabled_switch_makes_four_trace_routes_410_and_skips_writes() {
    let (server, repo) = app_with(TraceIngestSwitch::from_value(Some("0"))).await;

    // 1) 带合法凭证的 4 个路由：一律 410 + 固定响应体
    let post = server
        .post(TRACES)
        .add_header("x-client-token", CLIENT_TOKEN)
        .json(&trace_payload())
        .await;
    post.assert_status(StatusCode::GONE);
    let body: Value = post.json();
    assert_eq!(body["error"], "trace_ingest_disabled");
    assert_eq!(body["message"], "tracing moved to tempo+langfuse");

    let list = server
        .get(TRACES)
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await;
    list.assert_status(StatusCode::GONE);

    let get_one = server
        .get("/api/v1/traces/jc_switch_test_thread")
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await;
    get_one.assert_status(StatusCode::GONE);

    let patch = server
        .patch("/api/v1/traces/jc_switch_test_thread")
        .add_header("x-admin-token", ADMIN_TOKEN)
        .json(&json!({ "eval_status": "triage_good", "note": "disabled" }))
        .await;
    patch.assert_status(StatusCode::GONE);

    // 2) 无凭证也是 410：停收门在 admin 鉴权之外，避免「补个 token 就能继续上报」的误导
    server
        .post(TRACES)
        .json(&trace_payload())
        .await
        .assert_status(StatusCode::GONE);
    server.get(TRACES).await.assert_status(StatusCode::GONE);

    // 3) 处理器未执行 ⇒ 没有写库（仓储直读，绕过 HTTP）
    let written = repo
        .list_traces(TraceFilter::default())
        .await
        .expect("list_traces 应可直读");
    assert!(
        written.is_empty(),
        "停收期间不得写库，实际写入 {} 条",
        written.len()
    );

    // 4) trace 指标未递增（`/metrics` 仍可读，且不存在非零 trace 计数）
    let metrics = server.get("/metrics").await;
    metrics.assert_status(StatusCode::OK);
    let text = metrics.text();
    for line in text.lines() {
        if line.starts_with("pony_sentry_trace_") && !line.starts_with('#') {
            let value = line.rsplit(' ').next().unwrap_or("0");
            assert_eq!(value, "0", "停收期间 trace 指标必须为 0，实得: {line}");
        }
    }
}

#[tokio::test]
async fn disabled_switch_leaves_issue_ingest_healthz_and_metrics_untouched() {
    let (server, _repo) = app_with(TraceIngestSwitch::from_value(Some("off"))).await;

    // /healthz：语义不变
    let health = server.get("/healthz").await;
    health.assert_status(StatusCode::OK);
    let health_body: Value = health.json();
    assert_eq!(health_body["db"], "up");
    assert_eq!(health_body["status"], "ok");

    // POST /api/v1/ingest：Issue 采集照旧
    let ingest = server
        .post("/api/v1/ingest")
        .add_header("x-client-token", CLIENT_TOKEN)
        .json(&json!({
            "platform": "rust",
            "release": "v0.1.0",
            "environment": "production",
            "message": "issue ingest must keep working while tracing is off"
        }))
        .await;
    ingest.assert_status(StatusCode::CREATED);
    let ingest_body: Value = ingest.json();
    assert_eq!(ingest_body["count"], 1);

    // GET/PATCH /api/v1/issues*：读与状态流转照旧
    let issues = server
        .get("/api/v1/issues")
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await;
    issues.assert_status(StatusCode::OK);
    let issues_body: Value = issues.json();
    let first = &issues_body[0];
    let issue_id = first["id"].as_str().expect("issue 应带 id").to_string();

    let patched = server
        .patch(&format!("/api/v1/issues/{issue_id}"))
        .add_header("x-admin-token", ADMIN_TOKEN)
        .json(&json!({ "status": "resolved" }))
        .await;
    patched.assert_status(StatusCode::OK);

    // /metrics：仍免鉴权可读
    server.get("/metrics").await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn default_switch_keeps_trace_routes_working() {
    // 未设置环境变量 = 默认开启（不传 Some，走 from_value(None)）
    let (server, _repo) = app_with(TraceIngestSwitch::from_value(None)).await;

    let created = server
        .post(TRACES)
        .add_header("x-client-token", CLIENT_TOKEN)
        .json(&trace_payload())
        .await;
    created.assert_status(StatusCode::CREATED);
    let created_body: Value = created.json();
    let trace_id = created_body["id"]
        .as_str()
        .expect("trace 应带 id")
        .to_string();

    let list = server
        .get(TRACES)
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await;
    list.assert_status(StatusCode::OK);
    let list_body: Value = list.json();
    assert_eq!(list_body.as_array().map(|a| a.len()), Some(1));

    let one = server
        .get(&format!("/api/v1/traces/{trace_id}"))
        .add_header("x-admin-token", ADMIN_TOKEN)
        .await;
    one.assert_status(StatusCode::OK);

    let patched = server
        .patch(&format!("/api/v1/traces/{trace_id}"))
        .add_header("x-admin-token", ADMIN_TOKEN)
        .json(&json!({ "eval_status": "triage_good", "note": "keep working" }))
        .await;
    patched.assert_status(StatusCode::OK);
}
