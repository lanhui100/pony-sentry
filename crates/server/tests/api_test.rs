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
        admin_token: Some("test-admin-token".into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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
    res.assert_status(StatusCode::CREATED);
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
    res_vue.assert_status(StatusCode::CREATED);

    // 3. Agent 调用 REST API 查询待处理的 Rust issues
    let list_res = server
        .get("/api/v1/issues?platform=rust&status=unresolved")
        .add_header("x-admin-token", "test-admin-token")
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
        .add_header("x-admin-token", "test-admin-token")
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
        .add_header("x-admin-token", "test-admin-token")
        .await;
    resolve_res.assert_status(StatusCode::OK);
    let resolved_issue: serde_json::Value = resolve_res.json();
    assert_eq!(resolved_issue["status"], "resolved");

    // 6. 新版本再度上报同一错误，自动触发 Regression
    let res_repeat = server.post("/api/v1/ingest").json(&rust_payload).await;
    res_repeat.assert_status(StatusCode::CREATED);
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
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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
    res_ok.assert_status(StatusCode::CREATED);
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
        admin_token: Some("test-admin-token".into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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
        .assert_status(StatusCode::CREATED);

    // 尝试 SQL 注入 payload（URL 编码），参数化绑定后应安全返回 200 且为空结果
    let inject_res = server
        .get("/api/v1/issues?platform=%27%20OR%20%271%27%3D%271%27--")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    inject_res.assert_status(StatusCode::OK);
    let issues: Vec<serde_json::Value> = inject_res.json();
    assert_eq!(
        issues.len(),
        0,
        "SQL injection attempt must not return data"
    );

    // 负分页参数应被钳制而非报错
    let neg_res = server
        .get("/api/v1/issues?limit=-5&offset=-10")
        .add_header("x-admin-token", "test-admin-token")
        .await;
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
        admin_token: Some("test-admin-token".into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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
        .assert_status(StatusCode::CREATED);
    report("ReferenceErrorB", "/home/dev/blog-web")
        .await
        .assert_status(StatusCode::CREATED);

    // issue.project 暴露的是项目名（工作区末段），而非会随机器变化的绝对路径
    let issues: Vec<serde_json::Value> = server
        .get("/api/v1/issues")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    let mut projects: Vec<&str> = issues
        .iter()
        .map(|i| i["project"].as_str().expect("project must be present"))
        .collect();
    // 列表按 last_seen_at DESC 排序，同毫秒写入时次序不定，这里只校验集合
    projects.sort_unstable();
    assert_eq!(projects, vec!["blog-web", "shop-web"]);

    // 筛选只返回目标项目
    let filtered: Vec<serde_json::Value> = server
        .get("/api/v1/issues?project=shop-web")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["project"], "shop-web");

    // 下拉候选：去重排序
    let candidates: Vec<String> = server
        .get("/api/v1/projects")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(
        candidates,
        vec!["blog-web".to_string(), "shop-web".to_string()]
    );

    // 无工作区注入的事件不进入候选列表
    server
        .post("/api/v1/ingest")
        .json(&json!({ "platform": "python", "message": "no workspace here" }))
        .await
        .assert_status(StatusCode::CREATED);
    let after: Vec<String> = server
        .get("/api/v1/projects")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
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
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 1. LangGraph / LangChain 客户端使用 thread_id 发送 Trace
    // `C2`（v2 语义）：`total_*` 是 turns 求和（单一真相），故 per-turn 需携带计量字段；
    // 顶层直传值只作向后兼容占位、不再落库。
    let langgraph_payload = json!({
        "thread_id": "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a",
        "project": "job_copilot",
        "environment": "production",
        "release": "1.0.0",
        "turns": [
            { "step": "agent_node", "input": "搜索最新招聘", "input_tokens": 1000, "output_tokens": 200, "duration_ms": 1500 }
        ],
        "total_input_tokens": 1500,
        "total_output_tokens": 300,
        "total_duration_ms": 2500
    });

    let resp = server.post("/api/v1/traces").json(&langgraph_payload).await;
    resp.assert_status(StatusCode::CREATED);
    let created: serde_json::Value = resp.json();
    assert_eq!(
        created["session_id"],
        "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a"
    );

    // 2. 第二轮图执行完成后增量更新该 thread
    let langgraph_update = json!({
        "thread_id": "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a",
        "project": "job_copilot",
        "environment": "production",
        "release": "1.0.0",
        "turns": [
            { "step": "agent_node", "input": "搜索最新招聘", "input_tokens": 1000, "output_tokens": 200, "duration_ms": 1500 },
            { "step": "tool_node", "tool": "search_jobs", "input_tokens": 2200, "output_tokens": 600, "duration_ms": 3500 }
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

    // 3. 验证幂等性：同一 thread_id 仅存在 1 条记录，且累积数据已刷新（= turns 求和）
    let list: Vec<serde_json::Value> = server
        .get("/api/v1/traces")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0]["session_id"],
        "jc_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a"
    );
    assert_eq!(list[0]["turns_count"], 2, "两轮 turn 必须去重合并为 2 条");
    assert_eq!(list[0]["total_input_tokens"], 3200);
    assert_eq!(list[0]["total_duration_ms"], 5000);
}

#[tokio::test]
async fn test_webhook_anti_avalanche_on_regression() {
    use axum::routing::post;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static WEBHOOK_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    // 启动本地 mock webhook 接收服务器
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mock_app = Router::new().route(
        "/webhook",
        post(|| async {
            WEBHOOK_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }),
    );

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
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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
    res.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = res.json();
    let issue_id = body["issue_id"].as_str().unwrap().to_string();
    assert_eq!(body["count"], 1);

    // 等待异步 webhook tokio::spawn 完成（重试轮询，避免单机高负载时 sleep(50ms) 竞态）
    for _ in 0..20 {
        if WEBHOOK_CALL_COUNT.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        WEBHOOK_CALL_COUNT.load(Ordering::SeqCst),
        1,
        "初次创建应触发 1 次 Webhook"
    );

    // 2. 第二次上报：count=2，处于 unresolved，不触发 Webhook
    server
        .post("/api/v1/ingest")
        .json(&payload)
        .await
        .assert_status(StatusCode::CREATED);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        WEBHOOK_CALL_COUNT.load(Ordering::SeqCst),
        1,
        "未解决的后续事件不应重复触发 Webhook"
    );

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
    server
        .post("/api/v1/ingest")
        .json(&payload)
        .await
        .assert_status(StatusCode::CREATED);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        WEBHOOK_CALL_COUNT.load(Ordering::SeqCst),
        2,
        "从 resolved 回归时必须触发 1 次 issue.regression Webhook"
    );

    // 5. 第四、五次上报：已处于 regression 状态，绝对不得再次触发 Webhook（防止雪崩）
    server
        .post("/api/v1/ingest")
        .json(&payload)
        .await
        .assert_status(StatusCode::CREATED);
    server
        .post("/api/v1/ingest")
        .json(&payload)
        .await
        .assert_status(StatusCode::CREATED);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        WEBHOOK_CALL_COUNT.load(Ordering::SeqCst),
        2,
        "在已回归状态下的持续涌入事件严禁重复触发 Webhook 雪崩"
    );
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
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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
    server
        .post("/api/v1/ingest")
        .json(&issue_blog)
        .await
        .assert_status(StatusCode::CREATED);

    // 4. 测试 GET /api/v1/traces?project=job_copilot 过滤
    let filtered: Vec<serde_json::Value> = server
        .get("/api/v1/traces?project=job_copilot")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["session_id"], "sess-copilot-api-01");
    assert_eq!(filtered[0]["project"], "job_copilot");

    // 5. 测试 GET /api/v1/projects 聚合 issues 和 traces 的去重项目清单
    let projects: Vec<String> = server
        .get("/api/v1/projects")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(
        projects,
        vec![
            "blog-web".to_string(),
            "job_copilot".to_string(),
            "pony-agent".to_string()
        ]
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
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 上报含未脱敏路径与 token 的 payload
    // 注意：SanitizationPipeline 中 SENSITIVE_KEYS 命中 key 时整字段替换为 [REDACTED_SECRET]；
    // 字符串内正则命中 Bearer 也会替换为 [REDACTED_SECRET]。
    let raw_payload = json!({
        "session_id": "sess-sanitize-server-01",
        "project": "security-test",
        "total_input_tokens": 1200,
        "turns": [
            {
                "turn_id": "t1",
                "input_text": "read file /home/developer/secrets/app.key",
                "input_tokens": 800,
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
        .add_header("x-admin-token", "test-admin-token")
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

    // 断言 token 字段被脱敏，而 tokens 计量指标不受影响
    let token_val = turn["token"].as_str().unwrap_or_default();
    assert_eq!(
        token_val, "[REDACTED_SECRET]",
        "敏感 token 字段必须被脱敏为 [REDACTED_SECRET]"
    );
    // `C2`（v2 语义）：顶层 `total_*` 为 turns 求和（单一真相），客户端直传 1200 不再落库。
    assert_eq!(
        stored_payload["total_input_tokens"], 800,
        "total_input_tokens 计量指标严禁被误杀（须等于 turns 求和 800）"
    );
    assert_eq!(
        list[0]["total_input_tokens"], 800,
        "traces.total_input_tokens 列须与 turns 求和一致"
    );
    assert_eq!(
        turn["input_tokens"], 800,
        "turn.input_tokens 计量指标严禁被误杀"
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
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
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

#[tokio::test]
async fn test_ingest_contract_diagnostics_and_synonym_resolution() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 1. 上报 tags.service: "campus_collector" 异常消息
    let probe_payload = json!({
        "platform": "python",
        "environment": "dev",
        "message": "企业【测试企业】岗位【校园技术专家】门禁校验异常",
        "tags": {
            "company": "测试企业",
            "service": "campus_collector"
        }
    });
    let probe_resp = server.post("/api/v1/ingest").json(&probe_payload).await;
    probe_resp.assert_status(StatusCode::CREATED);
    let probe_json: serde_json::Value = probe_resp.json();
    let probe_issue_id = probe_json["issue_id"].as_str().unwrap();

    let issue_detail = server
        .get(&format!("/api/v1/issues/{}", probe_issue_id))
        .add_header("x-admin-token", "test-admin-token")
        .await;
    issue_detail.assert_status(StatusCode::OK);
    let issue_json: serde_json::Value = issue_detail.json();
    assert_eq!(issue_json["project"], "campus_collector");

    // 2. 上报 IPC 错误：遵循契约带 tags.project 与 command/source
    let ipc_payload = json!({
        "platform": "tauri",
        "environment": "dev",
        "release": "0.1.136",
        "message": "IPC command \"ask_cancel\" failed: control request cannot be consumed",
        "exception": {
            "error_type": "IpcCommandError",
            "value": "IPC command \"ask_cancel\" failed: control request cannot be consumed",
            "stacktrace": null
        },
        "tags": {
            "project": "my_desktop_app",
            "source": "tauri_safe_invoke",
            "command": "ask_cancel"
        }
    });
    let ipc_resp = server.post("/api/v1/ingest").json(&ipc_payload).await;
    ipc_resp.assert_status(StatusCode::CREATED);
    let ipc_json: serde_json::Value = ipc_resp.json();
    let ipc_issue_id = ipc_json["issue_id"].as_str().unwrap();
    assert!(
        ipc_json.get("warnings").is_none() || ipc_json["warnings"].as_array().unwrap().is_empty()
    );

    let ipc_issue_detail = server
        .get(&format!("/api/v1/issues/{}", ipc_issue_id))
        .add_header("x-admin-token", "test-admin-token")
        .await;
    ipc_issue_detail.assert_status(StatusCode::OK);
    let ipc_detail_json: serde_json::Value = ipc_issue_detail.json();
    assert_eq!(ipc_detail_json["project"], "my_desktop_app");
    assert_eq!(
        ipc_detail_json["culprit"],
        "tauri_safe_invoke in ask_cancel"
    );

    // 2.1 若上报仅有 IPC 错误但未带 project，服务端严禁盲猜，必须为 null 并返回 warning
    let unprojected_ipc_payload = json!({
        "platform": "tauri",
        "environment": "dev",
        "message": "IPC command \"ask_open\" failed",
        "exception": {
            "error_type": "IpcCommandError",
            "value": "IPC command \"ask_open\" failed",
            "stacktrace": null
        },
        "tags": {
            "source": "tauri_safe_invoke",
            "command": "ask_open"
        }
    });
    let unproj_resp = server
        .post("/api/v1/ingest")
        .json(&unprojected_ipc_payload)
        .await;
    unproj_resp.assert_status(StatusCode::CREATED);
    let unproj_json: serde_json::Value = unproj_resp.json();
    let unproj_warnings = unproj_json["warnings"]
        .as_array()
        .expect("warnings expected");
    assert!(unproj_warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("missing_project")));
    assert!(!unproj_warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("missing_culprit")));

    let unproj_detail: serde_json::Value = server
        .get(&format!(
            "/api/v1/issues/{}",
            unproj_json["issue_id"].as_str().unwrap()
        ))
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(unproj_detail["project"], serde_json::Value::Null);

    // 3. 上报无项目与无栈的事件，验证 warnings 反馈
    let bare_payload = json!({
        "platform": "other",
        "message": "Something generic broke"
    });
    let bare_resp = server.post("/api/v1/ingest").json(&bare_payload).await;
    bare_resp.assert_status(StatusCode::CREATED);
    let bare_json: serde_json::Value = bare_resp.json();
    let warnings = bare_json["warnings"]
        .as_array()
        .expect("warnings should be present");
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("missing_project")));
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("missing_culprit")));
}

#[tokio::test]
async fn test_console_session_login_cookie_csrf_and_logout() {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();

    let state = AppState {
        repo: repo.clone(),
        trace_repo: None,
        client_token: None,
        admin_token: Some("test-admin-token".into()),
        webhook_url: None,
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
    };
    let app = create_app_with_state(state);
    let server = TestServer::new(app).unwrap();

    // 先造一个可 PATCH 的 issue。
    let payload = json!({
        "platform": "rust",
        "release": "v0.1.0",
        "environment": "production",
        "message": "session csrf test",
        "exception": {"error_type": "SessionTestError"}
    });
    let created = server.post("/api/v1/ingest").json(&payload).await;
    created.assert_status(StatusCode::CREATED);
    let issue_id = created.json::<serde_json::Value>()["issue_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 错误 token 登录失败。
    let bad_login = server
        .post("/api/v1/auth/session")
        .json(&json!({"token": "wrong-token"}))
        .await;
    bad_login.assert_status(StatusCode::UNAUTHORIZED);

    // 正确 token 换取 HttpOnly + SameSite=Strict Cookie 和 CSRF token。
    let login = server
        .post("/api/v1/auth/session")
        .json(&json!({"token": "test-admin-token"}))
        .await;
    login.assert_status(StatusCode::OK);
    let set_cookie = login.header("set-cookie").to_str().unwrap().to_string();
    assert!(
        set_cookie.contains("HttpOnly"),
        "session cookie: {set_cookie}"
    );
    assert!(
        set_cookie.contains("SameSite=Strict"),
        "session cookie: {set_cookie}"
    );
    let cookie_pair = set_cookie.split(';').next().unwrap().to_string();
    let login_body = login.json::<serde_json::Value>();
    let csrf_token = login_body["csrf_token"].as_str().unwrap().to_string();
    assert!(login_body["expires_in"].as_u64().unwrap() >= 60);

    // Cookie 会话可读，且会话查询返回同一 CSRF token。
    let session = server
        .get("/api/v1/auth/session")
        .add_header("cookie", &cookie_pair)
        .await;
    session.assert_status(StatusCode::OK);
    assert_eq!(
        session.json::<serde_json::Value>()["csrf_token"]
            .as_str()
            .unwrap(),
        csrf_token
    );
    let list = server
        .get("/api/v1/issues")
        .add_header("cookie", &cookie_pair)
        .await;
    list.assert_status(StatusCode::OK);

    // Cookie 会话的 PATCH 必须带 CSRF；缺失 403、匹配则成功。
    let no_csrf = server
        .patch(&format!("/api/v1/issues/{issue_id}"))
        .json(&json!({"status": "resolved"}))
        .add_header("cookie", &cookie_pair)
        .await;
    no_csrf.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        no_csrf.json::<serde_json::Value>()["error"]
            .as_str()
            .unwrap(),
        "csrf_invalid"
    );
    let with_csrf = server
        .patch(&format!("/api/v1/issues/{issue_id}"))
        .json(&json!({"status": "resolved"}))
        .add_header("cookie", &cookie_pair)
        .add_header("x-csrf-token", &csrf_token)
        .await;
    with_csrf.assert_status(StatusCode::OK);
    assert_eq!(with_csrf.json::<serde_json::Value>()["status"], "resolved");

    // 注销后旧 Cookie 立即失效。
    let logout = server
        .delete("/api/v1/auth/session")
        .add_header("cookie", &cookie_pair)
        .await;
    logout.assert_status(StatusCode::NO_CONTENT);
    let after_logout = server
        .get("/api/v1/issues")
        .add_header("cookie", &cookie_pair)
        .await;
    after_logout.assert_status(StatusCode::UNAUTHORIZED);
}

// ─────────────────────────────────────────────────────────────────────────────
// `C5` / `C7` / `C9.4` / `C9.7` / `C9.9`：T4b HTTP 面与 trace 指标（服务端集成）
// ─────────────────────────────────────────────────────────────────────────────

/// 构造带 trace 仓库的应用（`C5`/`C7` 用例共用）。
async fn trace_test_server() -> TestServer {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();
    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
    };
    TestServer::new(create_app_with_state(state)).unwrap()
}

#[tokio::test]
async fn test_trace_query_params_reject_invalid_without_db_leak() {
    let server = trace_test_server().await;
    // 1) 非法/负值参数必须 400，且响应体不得回显数据库原文
    for query in [
        "limit=-5",
        "limit=abc",
        "offset=-1",
        "offset=xyz",
        "from=not-a-time",
        "to=2026-13-45",
    ] {
        let resp = server
            .get(&format!("/api/v1/traces?{query}"))
            .add_header("x-admin-token", "test-admin-token")
            .await;
        resp.assert_status(StatusCode::BAD_REQUEST);
        let body = resp.text();
        for leak in [
            "Database error",
            "SQLITE",
            "sqlx",
            "syntax error",
            "no such",
        ] {
            assert!(
                !body.contains(leak),
                "`{query}` 的 4xx 响应体不得回显数据库错误原文（命中 `{leak}`）：{body}"
            );
        }
    }

    // 2) limit 超上限必须 clamp 到 100（而非拒绝）：造 101 行后取回 100 行
    for index in 0..101 {
        server
            .post("/api/v1/traces")
            .json(&json!({
                "session_id": format!("limit-clamp-{index}"),
                "project": "clamp",
                "turns": [{ "turn_id": "t1", "status": "succeeded" }]
            }))
            .await
            .assert_status(StatusCode::CREATED);
    }
    let resp = server
        .get("/api/v1/traces?limit=100000")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    resp.assert_status(StatusCode::OK);
    let rows: Vec<serde_json::Value> = resp.json();
    assert_eq!(
        rows.len(),
        100,
        "limit=100000 必须 clamp 到 100，实际 {} 行",
        rows.len()
    );

    // 3) 合法 RFC3339 时间窗 → 200（端点语义由 core 用例覆盖）
    let ok = server
        .get("/api/v1/traces?from=2026-01-01T00:00:00Z&to=2099-01-01T00:00:00Z")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    ok.assert_status(StatusCode::OK);

    // 4) 未知参数必须被忽略（保持向后兼容，不得 400）
    let unknown = server
        .get("/api/v1/traces?unknown_param=1&limit=5")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    unknown.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn test_trace_metrics_expose_four_counters_and_grow() {
    let server = trace_test_server().await;
    let before = server.get("/metrics").await.text();
    let sum_of = |body: &str, name: &str| -> f64 {
        body.lines()
            .filter(|line| {
                line.starts_with(name)
                    && matches!(line.as_bytes().get(name.len()), Some(b'{') | Some(b' '))
            })
            .filter_map(|line| line.rsplit(' ').next().and_then(|v| v.parse::<f64>().ok()))
            .sum()
    };
    let before_traces = sum_of(&before, "pony_sentry_traces_total");

    // 1) 一次「成功状态 + 失败文案」上报：traces_total / turns_total / status_conflicts 均应增长
    server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "metrics-conflict-1",
            "project": "job_copilot",
            "turns": [{
                "turn_id": "t1",
                "status": "failed",
                "tool_calls": [{
                    "tool_call_id": "c1",
                    "tool_name": "format_jd_to_json",
                    "status": "success",
                    "output": "职位解析失败（原因: 模型返回空结果）"
                }]
            }]
        }))
        .await
        .assert_status(StatusCode::CREATED);

    // 2) 缺失 session_id/thread_id → 400 且 ingest_rejected 增长
    server
        .post("/api/v1/traces")
        .json(&json!({"environment": "development", "release": "dev"}))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let after = server.get("/metrics").await.text();
    for name in [
        "pony_sentry_traces_total",
        "pony_sentry_trace_turns_total",
        "pony_sentry_trace_status_conflicts_total",
        "pony_sentry_trace_ingest_rejected_total",
    ] {
        assert!(
            after.contains(name),
            "`/metrics` 必须包含 `{name}`：\n{after}"
        );
        assert!(
            sum_of(&after, name) >= 1.0,
            "`{name}` 必须随上报增长（当前合计 {}）",
            sum_of(&after, name)
        );
    }
    assert!(
        sum_of(&after, "pony_sentry_traces_total") > before_traces,
        "traces_total 必须随成功上报递增"
    );
    assert!(
        after.contains("pony_sentry_traces_total{project=\"job_copilot\""),
        "traces_total 必须带 project/turn_status 维度：\n{after}"
    );
    assert!(
        after.contains("pony_sentry_trace_turns_total{status=\"failed\"}"),
        "trace_turns_total 必须带 status 维度：\n{after}"
    );
    assert!(
        after.contains("pony_sentry_trace_ingest_rejected_total{reason=\"session_missing\"}"),
        "ingest_rejected_total 必须带 reason 维度：\n{after}"
    );
}

#[tokio::test]
async fn test_trace_note_patch_persists_and_survives_reingest() {
    let server = trace_test_server().await;
    let created = server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "note-patch-1",
            "project": "job_copilot",
            "turns": [{ "turn_id": "t1", "status": "succeeded" }]
        }))
        .await;
    created.assert_status(StatusCode::CREATED);
    let id = created.json::<serde_json::Value>()["id"]
        .as_str()
        .expect("POST 响应必须含字符串 id")
        .to_string();

    let patched = server
        .patch(&format!("/api/v1/traces/{id}"))
        .json(&json!({"eval_status": "triage_good", "note": "人工复核：失败原因已确认"}))
        .add_header("x-admin-token", "test-admin-token")
        .await;
    patched.assert_status(StatusCode::OK);
    assert_eq!(
        patched.json::<serde_json::Value>()["note"],
        "人工复核：失败原因已确认",
        "PATCH 响应必须回显落库后的 note"
    );

    let detail: serde_json::Value = server
        .get(&format!("/api/v1/traces/{id}"))
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(
        detail["note"], "人工复核：失败原因已确认",
        "note 必须从详情读回"
    );
    assert_eq!(detail["eval_status"], "triage_good", "eval_status 必须落库");

    // 再次 ingest（同 session）不得覆盖已有 note
    server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "note-patch-1",
            "project": "job_copilot",
            "turns": [{ "turn_id": "t2", "status": "failed" }]
        }))
        .await
        .assert_status(StatusCode::CREATED);
    let listed: Vec<serde_json::Value> = server
        .get("/api/v1/traces?session_id=note-patch-1")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0]["note"], "人工复核：失败原因已确认",
        "重复上报不得清空 note"
    );
    assert_eq!(listed[0]["turns_count"], 2, "turns_count 必须随写入推进");
    assert_eq!(
        listed[0]["turn_status"], "failed",
        "turn_status 必须为最后一个 turn 的 status"
    );
}

#[tokio::test]
async fn test_trace_ingest_response_matches_persisted_row() {
    let server = trace_test_server().await;
    let created = server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "ingest-consistency-1",
            "project": "job_copilot",
            "environment": "development",
            "release": "dev",
            // 客户端直传的汇总值必须被忽略（`C2` 单一真相 = turns 求和）
            "total_input_tokens": 99999,
            "total_output_tokens": 99999,
            "total_duration_ms": 99999,
            "turns": [
                { "turn_id": "t1", "status": "succeeded", "input_tokens": 10, "output_tokens": 5, "duration_ms": 100 },
                { "turn_id": "t2", "status": "failed", "input_tokens": 20, "output_tokens": 7, "duration_ms": 200 }
            ]
        }))
        .await;
    created.assert_status(StatusCode::CREATED);
    let created_json: serde_json::Value = created.json();
    let id = created_json["id"]
        .as_str()
        .expect("POST 响应必须含 id")
        .to_string();

    let fetched: serde_json::Value = server
        .get(&format!("/api/v1/traces/{id}"))
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(fetched["id"], id, "POST 返回的 id 必须可直接 GET");

    for (field, expected) in [
        ("total_input_tokens", 30_i64),
        ("total_output_tokens", 12),
        ("total_duration_ms", 300),
    ] {
        assert_eq!(
            created_json[field].as_i64(),
            Some(expected),
            "POST 响应 {field} 必须等于 turns 求和（拒绝客户端直传值）"
        );
        assert_eq!(
            fetched[field].as_i64(),
            Some(expected),
            "GET 响应 {field} 必须等于 turns 求和"
        );
    }
    assert_eq!(
        created_json["turns_count"], 2,
        "POST 响应必须回传播久化 turns_count"
    );
    assert_eq!(
        created_json["turn_status"], "failed",
        "POST 响应必须回传播久化 turn_status"
    );
    assert!(
        fetched["payload"].get("eval_status").is_none(),
        "payload.eval_status 必须移除（列是唯一真源）"
    );
}

#[tokio::test]
async fn test_trace_ingest_malformed_body_is_400_without_db_leak() {
    let server = trace_test_server().await;
    let before = server.get("/metrics").await.text();
    let rejected_before = before
        .lines()
        .find(|line| {
            line.starts_with("pony_sentry_trace_ingest_rejected_total{reason=\"invalid_params\"}")
        })
        .and_then(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .unwrap_or(0.0);

    // 非法 JSON 体：统一 400 `Bad request`，不得回显 serde/DB 原文
    let resp = server
        .post("/api/v1/traces")
        .content_type("application/json")
        .text("{not-json")
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    let body = resp.text();
    assert!(
        body.contains("Bad request"),
        "非法请求体响应须为 `Bad request`：{body}"
    );
    for leak in ["Database error", "SQLITE", "sqlx", "no such"] {
        assert!(
            !body.contains(leak),
            "响应体不得回显数据库原文（命中 `{leak}`）：{body}"
        );
    }

    let after = server.get("/metrics").await.text();
    let rejected_after = after
        .lines()
        .find(|line| {
            line.starts_with("pony_sentry_trace_ingest_rejected_total{reason=\"invalid_params\"}")
        })
        .and_then(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    assert!(
        rejected_after > rejected_before,
        "非法请求体必须计入 ingest_rejected_total{{reason=\"invalid_params\"}}：\n{after}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T6（L3-B 修复）：F1 错误原文泄漏 / F2 指标基数 / F6 PATCH rejection / F10 eval_status
// ─────────────────────────────────────────────────────────────────────────────

/// 文件型 SQLite 夹具：返回 server 与同一 pool（测试内可制造真实 DB 故障），Drop 时删库文件。
struct FileDbServer {
    server: TestServer,
    pool: pony_sentry_core::DatabasePool,
    path: std::path::PathBuf,
}

async fn file_db_server(tag: &str) -> FileDbServer {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "pony-sentry-{tag}-{}-{nanos}.db",
        std::process::id()
    ));
    let pool = create_pool(&format!("sqlite:{}?mode=rwc", path.display()))
        .await
        .unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool.clone()));
    repo.migrate().await.unwrap();
    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: None,
        sessions: Default::default(),
    };
    let server = TestServer::new(create_app_with_state(state)).unwrap();
    FileDbServer { server, pool, path }
}

impl Drop for FileDbServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("db-wal"));
        let _ = std::fs::remove_file(self.path.with_extension("db-shm"));
    }
}

/// `F1`：断言错误响应体只含通用文案，绝不回显 DB/serde 原文。
fn assert_no_internal_leak(raw: &str, context: &str) {
    for leak in [
        "Database error",
        "SQLITE",
        "sqlx",
        "no such table",
        "syntax error",
        "UNIQUE constraint",
        "expected value",
        "unknown variant",
    ] {
        assert!(
            !raw.contains(leak),
            "`{context}` 的响应体不得回显内部原文（命中 `{leak}`）：{raw}"
        );
    }
}

#[tokio::test]
async fn test_issue_endpoints_hide_db_error_text() {
    let fixture = file_db_server("f1-issue").await;
    // 制造真实 DB 故障（比 mock 更接近 L3-B 的「DB 锁/故障」场景）：直接删表。
    // `events` 与 `issues` 各自独立被查询，故两张表都要删，才能覆盖三条读路径。
    sqlx::query("DROP TABLE issues")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE events")
        .execute(&fixture.pool)
        .await
        .unwrap();

    let list = fixture
        .server
        .get("/api/v1/issues")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    list.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        list.text(),
        "Internal error",
        "F1：issue 列表 DB 故障必须只回 Internal error"
    );
    assert_no_internal_leak(&list.text(), "GET /api/v1/issues");

    let detail = fixture
        .server
        .get("/api/v1/issues/whatever")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    detail.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_leak(&detail.text(), "GET /api/v1/issues/:id");

    let events = fixture
        .server
        .get("/api/v1/issues/whatever/events")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    events.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_leak(&events.text(), "GET /api/v1/issues/:id/events");

    let projects = fixture
        .server
        .get("/api/v1/projects")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    projects.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_leak(&projects.text(), "GET /api/v1/projects");
}

#[tokio::test]
async fn test_trace_endpoints_hide_db_error_text() {
    let fixture = file_db_server("f1-trace").await;
    sqlx::query("DROP TABLE trace_turns")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE traces")
        .execute(&fixture.pool)
        .await
        .unwrap();

    let ingest = fixture
        .server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "f1-trace-1",
            "turns": [{ "turn_id": "t1", "status": "succeeded" }]
        }))
        .await;
    ingest.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        ingest.text(),
        "Internal error",
        "F1：trace ingest DB 故障必须只回 Internal error"
    );
    assert_no_internal_leak(&ingest.text(), "POST /api/v1/traces");

    let list = fixture
        .server
        .get("/api/v1/traces")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    list.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_leak(&list.text(), "GET /api/v1/traces");

    let detail = fixture
        .server
        .get("/api/v1/traces/whatever")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    detail.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_leak(&detail.text(), "GET /api/v1/traces/:id");
}

#[tokio::test]
async fn test_trace_metrics_labels_are_normalized_and_bounded() {
    let server = trace_test_server().await;
    // 1) 超长且含非法字符的 project：label 截断到 64 字符 + 字符集替换，/metrics 不被撑大
    let huge_project = "A".repeat(100_000);
    server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "f2-huge-1",
            "project": huge_project,
            "turns": [{ "turn_id": "t1", "status": "succeeded" }]
        }))
        .await
        .assert_status(StatusCode::CREATED);
    let metrics = server.get("/metrics").await.text();
    assert!(
        metrics.len() < 8 * 1024,
        "F2：100KB project 不得撑大 /metrics（实际 {} 字节）",
        metrics.len()
    );
    assert!(
        metrics.contains(&format!("project=\"{}\"", "A".repeat(64))),
        "F2：project label 必须截断到 64 字符：\n{metrics}"
    );
    assert!(
        !metrics.contains(&"A".repeat(65)),
        "F2：不得输出超过 64 字符的 project label"
    );

    // 2) 非法字符集与未受控 turn_status 归一
    server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "f2-weird-1",
            "project": "proj/<weird>#",
            "turns": [{ "turn_id": "t1", "status": "Weird Status" }]
        }))
        .await
        .assert_status(StatusCode::CREATED);
    let metrics = server.get("/metrics").await.text();
    assert!(
        metrics.contains("project=\"proj__weird__\""),
        "F2：非法字符必须替换为 `_`：\n{metrics}"
    );
    assert!(
        metrics
            .contains("pony_sentry_traces_total{project=\"proj__weird__\",turn_status=\"other\"}"),
        "F2：未受控 turn_status 必须归 `other`：\n{metrics}"
    );
    assert!(
        metrics.contains("pony_sentry_trace_turns_total{status=\"other\"}"),
        "F2：turn 维度未受控 status 必须归 `other`：\n{metrics}"
    );
}

#[tokio::test]
async fn test_trace_metrics_series_are_capped_to_512_with_overflow() {
    let server = trace_test_server().await;
    // 512 series 上限：造 520 个不同 project，超出部分折叠进 overflow
    for index in 0..520 {
        server
            .post("/api/v1/traces")
            .json(&json!({
                "session_id": format!("f2-cap-{index}"),
                "project": format!("cap-{index}"),
                "turns": [{ "turn_id": "t1", "status": "succeeded" }]
            }))
            .await
            .assert_status(StatusCode::CREATED);
    }
    let metrics = server.get("/metrics").await.text();
    let series = metrics
        .lines()
        .filter(|line| line.starts_with("pony_sentry_traces_total{"))
        .count();
    assert!(
        series <= 513,
        "F2：series 上限 512（含 overflow 行至多 513），实际 {series} 行"
    );
    assert!(
        metrics.contains("pony_sentry_traces_total{project=\"overflow\",turn_status=\"overflow\"}"),
        "F2：超出上限必须折叠进 overflow：\n{metrics}"
    );
}

#[tokio::test]
async fn test_patch_rejections_are_bad_request_without_enum_echo() {
    let server = trace_test_server().await;
    let created = server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "f6-patch-1",
            "turns": [{ "turn_id": "t1", "status": "succeeded" }]
        }))
        .await
        .json::<serde_json::Value>();
    let id = created["id"]
        .as_str()
        .expect("POST 必须返回 id")
        .to_string();

    // 1) 非法枚举值：400 Bad request，且不得回显合法枚举全集
    let invalid_enum = server
        .patch(&format!("/api/v1/traces/{id}"))
        .json(&json!({"eval_status": "bogus_status"}))
        .add_header("x-admin-token", "test-admin-token")
        .await;
    invalid_enum.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_enum.text(),
        "Bad request",
        "F6：PATCH 非法枚举必须只回 Bad request"
    );
    for leaked in [
        "triage_good",
        "eval_dataset",
        "unreviewed",
        "unknown variant",
        "bogus_status",
    ] {
        assert!(
            !invalid_enum.text().contains(leaked),
            "F6：PATCH 响应不得回显枚举值/内部结构（命中 `{leaked}`）：{}",
            invalid_enum.text()
        );
    }

    // 2) 非法 JSON 体：400 Bad request，不回显 serde 原文
    let malformed = server
        .patch(&format!("/api/v1/traces/{id}"))
        .content_type("application/json")
        .text("{not-json")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    malformed.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(malformed.text(), "Bad request");
    assert_no_internal_leak(&malformed.text(), "PATCH /api/v1/traces/:id");

    // 3) issue PATCH 同样收口
    let issue_malformed = server
        .patch("/api/v1/issues/whatever")
        .content_type("application/json")
        .text("{not-json")
        .add_header("x-admin-token", "test-admin-token")
        .await;
    issue_malformed.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(issue_malformed.text(), "Bad request");
    assert_no_internal_leak(&issue_malformed.text(), "PATCH /api/v1/issues/:id");

    // 4) 合法 PATCH 依然生效（note 落库）
    let ok = server
        .patch(&format!("/api/v1/traces/{id}"))
        .json(&json!({"eval_status": "triage_good", "note": "ok"}))
        .add_header("x-admin-token", "test-admin-token")
        .await;
    ok.assert_status(StatusCode::OK);
    assert_eq!(ok.json::<serde_json::Value>()["eval_status"], "triage_good");
}

#[tokio::test]
async fn test_ingest_ignores_client_eval_status() {
    let server = trace_test_server().await;
    // F10（契约 v1.4-5）：客户端上报的 eval_status 一律忽略，落库为 unreviewed
    let created = server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "f10-eval-1",
            "eval_status": "triage_good",
            "turns": [{ "turn_id": "t1", "status": "succeeded" }]
        }))
        .await;
    created.assert_status(StatusCode::CREATED);
    assert_eq!(
        created.json::<serde_json::Value>()["eval_status"],
        "unreviewed",
        "F10：ingest 必须忽略客户端 eval_status（一律 unreviewed）"
    );

    let id = created.json::<serde_json::Value>()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let detail: serde_json::Value = server
        .get(&format!("/api/v1/traces/{id}"))
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(
        detail["eval_status"], "unreviewed",
        "F10：库内 eval_status 必须为 unreviewed"
    );

    // 二次上报同样不得改写
    server
        .post("/api/v1/traces")
        .json(&json!({
            "session_id": "f10-eval-1",
            "eval_status": "eval_dataset",
            "turns": [{ "turn_id": "t2", "status": "failed" }]
        }))
        .await
        .assert_status(StatusCode::CREATED);
    let listed: Vec<serde_json::Value> = server
        .get("/api/v1/traces?session_id=f10-eval-1")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .json();
    assert_eq!(
        listed[0]["eval_status"], "unreviewed",
        "F10：二次上报不得改写 eval_status"
    );

    // 评审只能走 admin PATCH
    let patched = server
        .patch(&format!("/api/v1/traces/{id}"))
        .json(&json!({"eval_status": "triage_bad"}))
        .add_header("x-admin-token", "test-admin-token")
        .await;
    patched.assert_status(StatusCode::OK);
    assert_eq!(
        patched.json::<serde_json::Value>()["eval_status"],
        "triage_bad"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// L3B-F11：`/metrics` 可选鉴权（`METRICS_TOKEN`）
// ─────────────────────────────────────────────────────────────────────────────

/// 带可选 `/metrics` 令牌的应用（`None` = 未配置，保持免鉴权）。
async fn metrics_server(metrics_token: Option<&str>) -> TestServer {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    let repo = Arc::new(SqliteIssueRepository::new(pool));
    repo.migrate().await.unwrap();
    let state = AppState {
        repo: repo.clone(),
        trace_repo: Some(repo.clone()),
        webhook_url: None,
        client_token: None,
        admin_token: Some("test-admin-token".into()),
        metrics: std::sync::Arc::default(),
        metrics_token: metrics_token.map(|token| token.to_string()),
        sessions: Default::default(),
    };
    TestServer::new(create_app_with_state(state)).unwrap()
}

#[tokio::test]
async fn test_metrics_stays_anonymous_when_token_not_configured() {
    // 默认模式（README / 探针现状）：未配置 METRICS_TOKEN → 免鉴权 200
    let server = metrics_server(None).await;
    let resp = server.get("/metrics").await;
    resp.assert_status(StatusCode::OK);
    assert!(
        resp.text().contains("pony_sentry_events_total"),
        "未配置令牌时 /metrics 必须保持既有输出"
    );
    // 空串等价未配置
    let server_empty = metrics_server(Some("")).await;
    server_empty
        .get("/metrics")
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn test_metrics_requires_bearer_or_metrics_token_when_configured() {
    let server = metrics_server(Some("metrics-secret")).await;

    // 1) 无凭证 → 401 纯文本 Unauthorized
    let anonymous = server.get("/metrics").await;
    anonymous.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous.text(),
        "Unauthorized",
        "F11：必须纯文本 `Unauthorized`"
    );

    // 2) 错误凭证 → 401（两种通道都覆盖）
    for (name, value) in [
        ("x-metrics-token", "wrong"),
        ("authorization", "Bearer wrong"),
        ("authorization", "metrics-secret"),
    ] {
        let denied = server.get("/metrics").add_header(name, value).await;
        denied.assert_status(StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.text(),
            "Unauthorized",
            "错误凭证 `{name}: {value}` 必须 401"
        );
    }

    // 3) 两种合法通道 → 200
    let via_header = server
        .get("/metrics")
        .add_header("x-metrics-token", "metrics-secret")
        .await;
    via_header.assert_status(StatusCode::OK);
    assert!(via_header.text().contains("pony_sentry_events_total"));

    let via_bearer = server
        .get("/metrics")
        .add_header("authorization", "Bearer metrics-secret")
        .await;
    via_bearer.assert_status(StatusCode::OK);
    assert!(via_bearer.text().contains("pony_sentry_events_total"));

    // 4) 鉴权仅作用于 /metrics，不影响其它端点
    server.get("/healthz").await.assert_status(StatusCode::OK);
    server
        .get("/api/v1/traces")
        .add_header("x-admin-token", "test-admin-token")
        .await
        .assert_status(StatusCode::OK);
}
