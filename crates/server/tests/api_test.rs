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

    let state = AppState {
        repo: repo.clone(),
        trace_repo: None,
        client_token: None,
        webhook_url: None,
    };
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
    let list_res = server
        .get("/api/v1/issues?platform=rust&status=unresolved")
        .await;
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
    let state = AppState {
        repo: repo.clone(),
        trace_repo: None,
        client_token: Some("test-secret-token".into()),
        webhook_url: None,
    };
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

    let state = AppState {
        repo: repo.clone(),
        trace_repo: None,
        client_token: None,
        webhook_url: None,
    };
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
    assert_eq!(
        issues.len(),
        0,
        "SQL injection attempt must not return data"
    );

    // 负分页参数应被钳制而非报错
    let neg_res = server.get("/api/v1/issues?limit=-5&offset=-10").await;
    neg_res.assert_status(StatusCode::OK);
}

/// 项目维度端到端：extra.project_path → issue.project → 筛选与候选列表。
#[tokio::test]
async fn test_project_path_is_surfaced_and_filterable() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: None,
        client_token: None,
        webhook_url: None,
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    let server_ref = &server;
    let report = |error_type: &'static str, workspace: &'static str| async move {
        server_ref
            .post("/api/v1/ingest")
            .json(&json!({
                "platform": "vue",
                "release": "v1.0.0",
                "environment": "production",
                "message": error_type,
                "exception": {
                    "error_type": error_type,
                    "value": "boom",
                    "stacktrace": [{
                        "filename": "src/views/Dashboard.vue",
                        "function": "mounted",
                        "lineno": 45,
                        "in_app": true
                    }]
                },
                "extra": { "project_path": workspace }
            }))
            .await
    };

    report("ReferenceErrorA", "/home/dev/shop-web")
        .await
        .assert_status(StatusCode::OK);
    report("ReferenceErrorB", "/home/dev/blog-web")
        .await
        .assert_status(StatusCode::OK);

    // issue.project 暴露的是项目名（工作区末段），而非会随机器变化的绝对路径
    let issues: Vec<serde_json::Value> = server.get("/api/v1/issues").await.json();
    let mut projects: Vec<&str> = issues
        .iter()
        .map(|i| i["project"].as_str().expect("project must be present"))
        .collect();
    // 列表按 last_seen_at DESC 排序，同毫秒写入时次序不定，这里只校验集合
    projects.sort_unstable();
    assert_eq!(projects, vec!["blog-web", "shop-web"]);

    // 筛选只返回目标项目
    let filtered: Vec<serde_json::Value> =
        server.get("/api/v1/issues?project=shop-web").await.json();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["project"], "shop-web");

    // 下拉候选：去重排序
    let candidates: Vec<String> = server.get("/api/v1/projects").await.json();
    assert_eq!(
        candidates,
        vec!["blog-web".to_string(), "shop-web".to_string()]
    );

    // 无工作区注入的事件不进入候选列表
    server
        .post("/api/v1/ingest")
        .json(&json!({ "platform": "python", "message": "no workspace here" }))
        .await
        .assert_status(StatusCode::OK);
    let after: Vec<String> = server.get("/api/v1/projects").await.json();
    assert_eq!(after, vec!["blog-web".to_string(), "shop-web".to_string()]);
}

#[tokio::test]
async fn test_traces_ingest_supports_langgraph_thread_id_and_upsert() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 1. LangGraph / LangChain 客户端使用 thread_id 发送 Trace
    let langgraph_payload = json!({
        "thread_id": "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a",
        "project": "job_copilot",
        "environment": "production",
        "release": "1.0.0",
        "turns": [
            { "step": "agent_node", "input": "搜索最新招聘" }
        ],
        "total_input_tokens": 1500,
        "total_output_tokens": 300,
        "total_duration_ms": 2500
    });

    let resp = server
        .post("/api/v1/traces")
        .json(&langgraph_payload)
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: serde_json::Value = resp.json();
    assert_eq!(created["session_id"], "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a");

    // 2. 第二轮图执行完成后增量更新该 thread
    let langgraph_update = json!({
        "thread_id": "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a",
        "project": "job_copilot",
        "environment": "production",
        "release": "1.0.0",
        "turns": [
            { "step": "agent_node", "input": "搜索最新招聘" },
            { "step": "tool_node", "tool": "search_jobs" }
        ],
        "total_input_tokens": 3200,
        "total_output_tokens": 800,
        "total_duration_ms": 5000
    });

    server
        .post("/api/v1/traces")
        .json(&langgraph_update)
        .await
        .assert_status(StatusCode::CREATED);

    // 3. 验证幂等性：同一 thread_id 仅存在 1 条记录，且累积数据已刷新
    let list: Vec<serde_json::Value> = server.get("/api/v1/traces").await.json();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["session_id"], "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a");
    assert_eq!(list[0]["total_input_tokens"], 3200);
    assert_eq!(list[0]["total_duration_ms"], 5000);
}

#[tokio::test]
async fn test_webhook_anti_avalanche_on_regression() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use axum::routing::post;
    use axum::Router;

    static WEBHOOK_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    // 启动本地 mock webhook 接收服务器
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mock_app = Router::new().route("/webhook", post(|| async {
        WEBHOOK_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        StatusCode::OK
    }));

    tokio::spawn(async move {
        axum::serve(mock_listener, mock_app).await.unwrap();
    });

    let webhook_url = format!("http://127.0.0.1:{mock_port}/webhook");

    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: None,
        client_token: None,
        webhook_url: Some(webhook_url),
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    let payload = json!({
        "platform": "rust",
        "message": "test panic for webhook avalanche prevention",
        "exception": {
            "error_type": "AvalanchePanic",
            "value": "boom",
            "stacktrace": []
        }
    });

    // 1. 首次上报：触发 1 次 issue.created Webhook
    let res = server.post("/api/v1/ingest").json(&payload).await;
    res.assert_status(StatusCode::OK);
    let body: serde_json::Value = res.json();
    let issue_id = body["issue_id"].as_str().unwrap().to_string();
    assert_eq!(body["count"], 1);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(WEBHOOK_CALL_COUNT.load(Ordering::SeqCst), 1, "初次创建应触发 1 次 Webhook");

    // 2. 第二次上报：count=2，处于 unresolved，不触发 Webhook
    server.post("/api/v1/ingest").json(&payload).await.assert_status(StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(WEBHOOK_CALL_COUNT.load(Ordering::SeqCst), 1, "未解决的后续事件不应重复触发 Webhook");

    // 3. 将 issue 标记为 resolved
    pony_sentry_core::IssueRepository::update_issue_status(
        repo.as_ref(),
        &issue_id,
        pony_sentry_core::models::IssueStatus::Resolved,
        None,
    )
    .await
    .unwrap();

    // 4. 第三次上报：状态由 resolved 回归为 regression，必须触发第 2 次 Webhook
    server.post("/api/v1/ingest").json(&payload).await.assert_status(StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(WEBHOOK_CALL_COUNT.load(Ordering::SeqCst), 2, "从 resolved 回归时必须触发 1 次 issue.regression Webhook");

    // 5. 第四、五次上报：已处于 regression 状态，绝对不得再次触发 Webhook（防止雪崩）
    server.post("/api/v1/ingest").json(&payload).await.assert_status(StatusCode::OK);
    server.post("/api/v1/ingest").json(&payload).await.assert_status(StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(WEBHOOK_CALL_COUNT.load(Ordering::SeqCst), 2, "在已回归状态下的持续涌入事件严禁重复触发 Webhook 雪崩");
}

#[tokio::test]
async fn test_traces_api_project_filter_and_aggregation() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 1. 上报带有 project="job_copilot" 的 Trace
    let trace_copilot = json!({
        "session_id": "sess-copilot-api-01",
        "project": "job_copilot",
        "turns": [
            { "turn_id": "t1", "input": "find job" }
        ]
    });
    let res_copilot = server.post("/api/v1/traces").json(&trace_copilot).await;
    res_copilot.assert_status(StatusCode::CREATED);

    // 2. 上报带有 project="pony-agent" 的 Trace
    let trace_pony = json!({
        "session_id": "sess-pony-api-01",
        "project": "pony-agent",
        "turns": [
            { "turn_id": "t1", "input": "agent execute" }
        ]
    });
    let res_pony = server.post("/api/v1/traces").json(&trace_pony).await;
    res_pony.assert_status(StatusCode::CREATED);

    // 3. 上报一个 issue，带 extra.project_path 属于 "blog-web" 项目
    let issue_blog = json!({
        "platform": "rust",
        "message": "blog panic error",
        "exception": { "error_type": "BlogError", "value": "test", "stacktrace": [] },
        "extra": { "project_path": "/home/dev/blog-web" }
    });
    server.post("/api/v1/ingest").json(&issue_blog).await.assert_status(StatusCode::OK);

    // 4. 测试 GET /api/v1/traces?project=job_copilot 过滤
    let filtered: Vec<serde_json::Value> = server
        .get("/api/v1/traces?project=job_copilot")
        .await
        .json();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["session_id"], "sess-copilot-api-01");
    assert_eq!(filtered[0]["project"], "job_copilot");

    // 5. 测试 GET /api/v1/projects 聚合 issues 和 traces 的去重项目清单
    let projects: Vec<String> = server.get("/api/v1/projects").await.json();
    assert_eq!(
        projects,
        vec!["blog-web".to_string(), "job_copilot".to_string(), "pony-agent".to_string()]
    );
}

#[tokio::test]
async fn test_traces_server_side_sanitization() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 上报含未脱敏路径与 token 的 payload
    // 注意：SanitizationPipeline 中 SENSITIVE_KEYS 命中 key 时整字段替换为 [REDACTED_SECRET]；
    // 字符串内正则命中 Bearer 也会替换为 [REDACTED_SECRET]。
    let raw_payload = json!({
        "session_id": "sess-sanitize-server-01",
        "project": "security-test",
        "turns": [
            {
                "turn_id": "t1",
                "input_text": "read file /home/developer/secrets/app.key",
                "token": "sk-secret-token-abcdef123456",
                "note": "Authorization: Bearer secret-access-token"
            }
        ]
    });

    let resp = server.post("/api/v1/traces").json(&raw_payload).await;
    resp.assert_status(StatusCode::CREATED);

    // 查询落库后的 trace
    let list: Vec<serde_json::Value> = server
        .get("/api/v1/traces?session_id=sess-sanitize-server-01")
        .await
        .json();
    assert_eq!(list.len(), 1);
    let stored_payload = &list[0]["payload"];

    let turns = stored_payload["turns"].as_array().expect("turns array");
    let turn = &turns[0];

    // 断言敏感路径 /home/developer 经服务端 SanitizationPipeline 脱敏
    let input = turn["input_text"].as_str().unwrap_or_default();
    assert!(
        !input.contains("/home/developer"),
        "服务端脱敏后不应包含 /home/developer，实际值: {}",
        input
    );
    assert!(
        input.contains("[USER_HOME]"),
        "路径应包含 [USER_HOME]，实际值: {}",
        input
    );

    // 断言 token 字段被脱敏
    let token_val = turn["token"].as_str().unwrap_or_default();
    assert_eq!(
        token_val, "[REDACTED_SECRET]",
        "敏感 token 字段必须被脱敏为 [REDACTED_SECRET]"
    );

    // 断言 note 内的 Bearer token 经正则脱敏
    let note = turn["note"].as_str().unwrap_or_default();
    assert!(
        !note.contains("secret-access-token"),
        "Bearer token 必须被脱敏，实际值: {}",
        note
    );
    assert!(
        note.contains("[REDACTED_SECRET]"),
        "Bearer token 必须脱敏为 [REDACTED_SECRET]，实际值: {}",
        note
    );
}

#[tokio::test]
async fn test_traces_large_payload_limit_2mb() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 构造约 1.5MB 大小的合规 trace payload（> 512KB，< 2MB）
    let large_string = "X".repeat(1_500_000);
    let large_payload = json!({
        "session_id": "sess-large-2mb-01",
        "project": "large-test",
        "turns": [
            {
                "turn_id": "t-large",
                "output_text": large_string
            }
        ]
    });

    // 断言返回 HTTP 201 Created（未被老版本 512KB 限制截断/拦截）
    let resp = server.post("/api/v1/traces").json(&large_payload).await;
    resp.assert_status(StatusCode::CREATED);

    let created: serde_json::Value = resp.json();
    assert_eq!(created["session_id"], "sess-large-2mb-01");
}
