use axum::http::StatusCode;
use axum_test::TestServer;
use pony_sentry_core::{create_pool, SqliteIssueRepository};
use pony_sentry_server::{create_app_with_state, AppState};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_ingest_and_agent_api_flow() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState { repo: repo.clone(), client_token: None, webhook_url: None };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 1. 模拟 Rust 端上报 panic 错误
    let rust_payload = json!({
        "platform": "rust",
        "release": "v0.1.0",
        "environment": "production",
        "message": "panic occurred at src/main.rs:12",
        "exception": {
            "error_type": "PanicPayload",
            "value": "explicit panic",
            "stacktrace": [
                {
                    "filename": "/home/user/app/src/main.rs",
                    "function": "main",
                    "lineno": 12,
                    "in_app": true
                }
            ]
        }
    });

    let res = server.post("/api/v1/ingest").json(&rust_payload).await;
    res.assert_status(StatusCode::OK);
    let json_body: serde_json::Value = res.json();
    let issue_id = json_body["issue_id"].as_str().unwrap().to_string();
    assert_eq!(json_body["count"], 1);
    assert_eq!(json_body["status"], "unresolved");

    // 2. 模拟 Vue SPA 端上报 Unhandled Promise Rejection
    let vue_payload = json!({
        "platform": "vue",
        "release": "v0.1.0",
        "environment": "production",
        "message": "TypeError: Cannot read properties of undefined (reading 'token')",
        "exception": {
            "error_type": "TypeError",
            "value": "Cannot read properties of undefined",
            "stacktrace": [
                {
                    "filename": "src/views/Dashboard.vue",
                    "function": "mounted",
                    "lineno": 45,
                    "in_app": true
                }
            ]
        }
    });
    let res_vue = server.post("/api/v1/ingest").json(&vue_payload).await;
    res_vue.assert_status(StatusCode::OK);

    // 3. Agent 调用 REST API 查询待处理的 Rust issues
    let list_res = server.get("/api/v1/issues?platform=rust&status=unresolved").await;
    list_res.assert_status(StatusCode::OK);
    let issues: Vec<serde_json::Value> = list_res.json();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["id"], issue_id);

    // 4. Agent 认领 issue (in_progress)
    let claim_res = server
        .patch(&format!("/api/v1/issues/{}", issue_id))
        .json(&json!({
            "status": "in_progress",
            "assigned_to": "agent-dev-lead"
        }))
        .await;
    claim_res.assert_status(StatusCode::OK);
    let claimed_issue: serde_json::Value = claim_res.json();
    assert_eq!(claimed_issue["status"], "in_progress");
    assert_eq!(claimed_issue["assigned_to"], "agent-dev-lead");

    // 5. Agent 完成修复，标记为 resolved
    let resolve_res = server
        .patch(&format!("/api/v1/issues/{}", issue_id))
        .json(&json!({
            "status": "resolved"
        }))
        .await;
    resolve_res.assert_status(StatusCode::OK);
    let resolved_issue: serde_json::Value = resolve_res.json();
    assert_eq!(resolved_issue["status"], "resolved");

    // 6. 新版本再度上报同一错误，自动触发 Regression
    let res_repeat = server.post("/api/v1/ingest").json(&rust_payload).await;
    res_repeat.assert_status(StatusCode::OK);
    let repeat_json: serde_json::Value = res_repeat.json();
    assert_eq!(repeat_json["status"], "regression");
    assert_eq!(repeat_json["count"], 2);
}

#[tokio::test]
async fn test_ingest_requires_client_token_when_configured() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    // 配置了 CLIENT_TOKEN 时必须校验
    let state = AppState { repo: repo.clone(), client_token: Some("test-secret-token".into()), webhook_url: None };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    let payload = json!({
        "platform": "rust",
        "message": "auth test"
    });

    // 无 Token → 401
    let res_no_token = server.post("/api/v1/ingest").json(&payload).await;
    res_no_token.assert_status(StatusCode::UNAUTHORIZED);

    // 错误 Token → 401
    let res_bad_token = server
        .post("/api/v1/ingest")
        .add_header("x-client-token", "wrong-token")
        .json(&payload)
        .await;
    res_bad_token.assert_status(StatusCode::UNAUTHORIZED);

    // 正确 Token → 200
    let res_ok = server
        .post("/api/v1/ingest")
        .add_header("x-client-token", "test-secret-token")
        .json(&payload)
        .await;
    res_ok.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn test_sql_injection_is_safely_parametrized() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState { repo: repo.clone(), client_token: None, webhook_url: None };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 插入一条测试数据
    server
        .post("/api/v1/ingest")
        .json(&json!({
            "platform": "rust",
            "message": "injection base",
            "exception": {
                "error_type": "TestError",
                "value": "base"
            }
        }))
        .await
        .assert_status(StatusCode::OK);

    // 尝试 SQL 注入 payload（URL 编码），参数化绑定后应安全返回 200 且为空结果
    let inject_res = server
        .get("/api/v1/issues?platform=%27%20OR%20%271%27%3D%271%27--")
        .await;
    inject_res.assert_status(StatusCode::OK);
    let issues: Vec<serde_json::Value> = inject_res.json();
    assert_eq!(issues.len(), 0, "SQL injection attempt must not return data");

    // 负分页参数应被钳制而非报错
    let neg_res = server.get("/api/v1/issues?limit=-5&offset=-10").await;
    neg_res.assert_status(StatusCode::OK);
}

