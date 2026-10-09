//! PonySentry 服务端 Trace 数据模型、状态机与存储契约验收及对抗测试
use chrono::Utc;
use pony_sentry_core::{
    create_pool,
    models::{EvalStatus, TraceFilter, TraceRecord},
    repository::{RepositoryError, TraceRepository},
    SqliteIssueRepository,
};
use serde_json::json;

#[test]
fn test_trace_eval_status_state_machine_roundtrip() {
    let cases = vec![
        (EvalStatus::Unreviewed, "\"unreviewed\""),
        (EvalStatus::TriageGood, "\"triage_good\""),
        (EvalStatus::TriageBad, "\"triage_bad\""),
        (EvalStatus::EvalDataset, "\"eval_dataset\""),
        (EvalStatus::Optimized, "\"optimized\""),
        (EvalStatus::Wontfix, "\"wontfix\""),
    ];

    for (status, expected_json) in cases {
        let serialized = serde_json::to_string(&status).expect("Serialization failed");
        assert_eq!(serialized, expected_json);
        let deserialized: EvalStatus =
            serde_json::from_str(&serialized).expect("Deserialization failed");
        assert_eq!(deserialized, status);
    }

    assert_eq!(EvalStatus::default(), EvalStatus::Unreviewed);
}

#[test]
fn test_trace_record_serialization_contract() {
    let now = Utc::now();
    let record = TraceRecord {
        id: "trc-test-001".to_string(),
        session_id: "sess-abc-001".to_string(),
        run_id: Some("run-123".to_string()),
        turn_id: Some("turn-456".to_string()),
        environment: "production".to_string(),
        release: "0.1.109".to_string(),
        project: None,
        eval_status: EvalStatus::Unreviewed,
        payload: json!({
            "turns": [
                {
                    "turn_id": "turn-456",
                    "model": "claude-3-7-sonnet",
                    "input_tokens": 1500,
                    "output_tokens": 200
                }
            ]
        }),
        total_input_tokens: Some(1500),
        total_output_tokens: Some(200),
        total_duration_ms: Some(1250),
        reported_at: now,
        created_at: now,
        updated_at: now,
    };

    let serialized = serde_json::to_string(&record).expect("Record serialization failed");
    let val: serde_json::Value =
        serde_json::from_str(&serialized).expect("Deserialization to Value failed");

    assert_eq!(val["id"], "trc-test-001");
    assert_eq!(val["session_id"], "sess-abc-001");
    assert_eq!(val["eval_status"], "unreviewed");
    assert_eq!(val["total_input_tokens"], 1500);

    let roundtrip: TraceRecord =
        serde_json::from_str(&serialized).expect("Roundtrip to TraceRecord failed");
    assert_eq!(roundtrip.id, record.id);
    assert_eq!(roundtrip.eval_status, EvalStatus::Unreviewed);
}

#[tokio::test]
async fn test_trace_repository_contract_and_lifecycle() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");

    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    let now = Utc::now();
    let trace = TraceRecord {
        id: "trc-lifecycle-001".to_string(),
        session_id: "sess-lifecycle-001".to_string(),
        run_id: Some("run-001".to_string()),
        turn_id: Some("turn-001".to_string()),
        environment: "production".to_string(),
        release: "0.1.109".to_string(),
        project: None,
        eval_status: EvalStatus::Unreviewed,
        payload: json!({"mock": "trace_data"}),
        total_input_tokens: Some(500),
        total_output_tokens: Some(100),
        total_duration_ms: Some(400),
        reported_at: now,
        created_at: now,
        updated_at: now,
    };

    let saved = repo.record_trace(trace).await.expect("record_trace failed");
    assert_eq!(saved.eval_status, EvalStatus::Unreviewed);

    let fetched = repo.get_trace(&saved.id).await.expect("get_trace failed");
    assert_eq!(fetched.id, saved.id);

    let updated = repo
        .update_trace_eval_status(&saved.id, EvalStatus::TriageGood)
        .await
        .expect("update_trace_eval_status failed");
    assert_eq!(updated.eval_status, EvalStatus::TriageGood);

    let list = repo
        .list_traces(TraceFilter {
            session_id: Some("sess-lifecycle-001".to_string()),
            eval_status: Some(EvalStatus::TriageGood),
            ..Default::default()
        })
        .await
        .expect("list_traces failed");
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn test_trace_not_found_and_adversarial_payload() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");
    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    let err = repo.get_trace("non-existent-id").await.unwrap_err();
    match err {
        RepositoryError::TraceNotFound(_) | RepositoryError::NotFound(_) => {}
        other => panic!("Expected NotFound/TraceNotFound error, got: {:?}", other),
    }

    let mut large_array = Vec::with_capacity(5000);
    for i in 0..5000 {
        large_array.push(json!({
            "step": i,
            "huge_text": "A".repeat(1000)
        }));
    }
    let large_trace = TraceRecord {
        id: "trc-adversarial-huge".to_string(),
        session_id: "sess-huge".to_string(),
        run_id: None,
        turn_id: None,
        environment: "stress".to_string(),
        release: "0.1.0".to_string(),
        project: None,
        eval_status: EvalStatus::Unreviewed,
        payload: json!({ "steps": large_array }),
        total_input_tokens: None,
        total_output_tokens: None,
        total_duration_ms: None,
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let result = repo.record_trace(large_trace).await;
    assert!(result.is_ok(), "Large trace record must be handled without crashing");
}

#[tokio::test]
async fn test_migrate_upgrades_existing_traces_table_with_project_column() {
    // 模拟存量库：traces 表已按旧 schema 建好（无 project 列），
    // migrate() 必须补列并建索引，而不是整体重放 0003（其内索引依赖 project 列）。
    let tmp = std::env::temp_dir().join(format!("ponysentry_old_traces_{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite:{}?mode=rwc", tmp.display());

    let pool = create_pool(&url).await.expect("Failed to create file pool");
    sqlx::raw_sql(
        "CREATE TABLE traces (
            id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL UNIQUE,
            run_id TEXT,
            turn_id TEXT,
            environment TEXT NOT NULL,
            release TEXT NOT NULL,
            eval_status TEXT NOT NULL DEFAULT 'unreviewed',
            payload TEXT NOT NULL,
            total_input_tokens INTEGER,
            total_output_tokens INTEGER,
            total_duration_ms INTEGER,
            reported_at TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )
    .execute(&pool)
    .await
    .expect("Seed old-schema traces table");

    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("migrate() must upgrade old traces schema");

    // 补列后写入与读取 project 必须可用
    let now = Utc::now();
    let trace = TraceRecord {
        id: "trc-upgrade-001".to_string(),
        session_id: "sess-upgrade-001".to_string(),
        run_id: None,
        turn_id: None,
        environment: "prod".to_string(),
        release: "1.0.0".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: EvalStatus::Unreviewed,
        payload: json!({"turns": []}),
        total_input_tokens: None,
        total_output_tokens: None,
        total_duration_ms: None,
        reported_at: now,
        created_at: now,
        updated_at: now,
    };
    repo.record_trace(trace).await.expect("record after upgrade");

    let projects = repo
        .list_projects()
        .await
        .expect("traces list_projects after upgrade");
    assert_eq!(projects, vec!["job_copilot".to_string()]);

    // 幂等：再次 migrate() 不得报错（重复启动重放）
    repo.migrate().await.expect("migrate() must be replay-idempotent");

    let _ = std::fs::remove_file(&tmp);
}

#[tokio::test]
async fn test_trace_upsert_on_same_session_id() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");
    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    let session_id = "pa_018f6e2b-8c5d-7a2f-9a2f-1e8c9d0b3f4a".to_string();
    let initial_trace = TraceRecord {
        id: "trc-1".to_string(),
        session_id: session_id.clone(),
        run_id: None,
        turn_id: Some("turn-1".to_string()),
        environment: "dev".to_string(),
        release: "0.1.0".to_string(),
        project: None,
        eval_status: EvalStatus::Unreviewed,
        payload: json!({ "turns": [{ "turn_id": "turn-1" }] }),
        total_input_tokens: Some(100),
        total_output_tokens: Some(50),
        total_duration_ms: Some(500),
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    repo.record_trace(initial_trace)
        .await
        .expect("Initial trace insert");

    // 第二次上传：包含 turn-1 和 turn-2，更新相同 session_id
    let updated_trace = TraceRecord {
        id: "trc-2".to_string(),
        session_id: session_id.clone(),
        run_id: None,
        turn_id: Some("turn-2".to_string()),
        environment: "dev".to_string(),
        release: "0.1.0".to_string(),
        project: None,
        eval_status: EvalStatus::Unreviewed,
        payload: json!({ "turns": [{ "turn_id": "turn-1" }, { "turn_id": "turn-2" }] }),
        total_input_tokens: Some(300),
        total_output_tokens: Some(150),
        total_duration_ms: Some(1200),
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    repo.record_trace(updated_trace)
        .await
        .expect("Updated trace upsert");

    let list = repo
        .list_traces(TraceFilter {
            session_id: Some(session_id.clone()),
            eval_status: None,
            environment: None,
            release: None,
            project: None,
            limit: None,
            offset: None,
        })
        .await
        .expect("list_traces");

    // 幂等：必须只有 1 条记录，且是最新的 tokens 累计
    assert_eq!(list.len(), 1, "同一 session_id 必须幂等更新为 1 条记录");
    assert_eq!(list[0].total_input_tokens, Some(300));
    assert_eq!(list[0].total_duration_ms, Some(1200));
    assert_eq!(list[0].turn_id.as_deref(), Some("turn-2"));
}

#[tokio::test]
async fn test_trace_incremental_merge_upsert_dedup() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");

    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    let session_id = "sess-merge-dedup-001".to_string();

    // 第 1 次：含 turns [t1, t2]，带 project
    let trace_1 = TraceRecord {
        id: "trc-merge-1".to_string(),
        session_id: session_id.clone(),
        run_id: None,
        turn_id: Some("t2".to_string()),
        environment: "production".to_string(),
        release: "0.2.0".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: EvalStatus::Unreviewed,
        payload: json!({
            "turns": [
                { "turn_id": "t1", "sequence": 1, "input": "query 1" },
                { "turn_id": "t2", "sequence": 2, "input": "query 2" }
            ]
        }),
        total_input_tokens: Some(200),
        total_output_tokens: Some(100),
        total_duration_ms: Some(500),
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    repo.record_trace(trace_1)
        .await
        .expect("First trace recorded");

    // 第 2 次：同一个 session_id，含 turns [t2, t3]，带 project
    let trace_2 = TraceRecord {
        id: "trc-merge-2".to_string(),
        session_id: session_id.clone(),
        run_id: None,
        turn_id: Some("t3".to_string()),
        environment: "production".to_string(),
        release: "0.2.0".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: EvalStatus::Unreviewed,
        payload: json!({
            "turns": [
                { "turn_id": "t2", "sequence": 2, "input": "query 2" },
                { "turn_id": "t3", "sequence": 3, "input": "query 3" }
            ]
        }),
        total_input_tokens: Some(300),
        total_output_tokens: Some(150),
        total_duration_ms: Some(800),
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    repo.record_trace(trace_2)
        .await
        .expect("Second trace recorded with upsert");

    let list = repo
        .list_traces(TraceFilter {
            session_id: Some(session_id.clone()),
            eval_status: None,
            environment: None,
            release: None,
            project: None,
            limit: None,
            offset: None,
        })
        .await
        .expect("list_traces");

    // 断言合并后最终 traces 表中只有 1 行记录
    assert_eq!(list.len(), 1, "同一个 session_id 经增量合并后只有 1 行记录");
    let record = &list[0];

    // 断言 project 字段被正确保存
    assert_eq!(record.project.as_deref(), Some("job_copilot"));

    // 断言 payload.turns 共有 3 个且按序 [t1, t2, t3]，t2 保持原样不重复
    let turns = record.payload["turns"]
        .as_array()
        .expect("payload.turns must be an array");
    assert_eq!(turns.len(), 3, "turns 必须增量合并去重为 3 个");

    let turn_ids: Vec<&str> = turns
        .iter()
        .map(|t| t["turn_id"].as_str().expect("turn_id must exist"))
        .collect();
    assert_eq!(
        turn_ids,
        vec!["t1", "t2", "t3"],
        "turns 顺序必须为 [t1, t2, t3]"
    );
}

#[tokio::test]
async fn test_trace_filter_by_project() {
    let pool = create_pool("sqlite::memory:")
        .await
        .expect("Failed to create memory pool");

    let repo = SqliteIssueRepository::new(pool);
    repo.migrate().await.expect("Failed to run migrations");

    // 写入 project="job_copilot" 和 project="pony-agent" 的两条记录
    let trace_copilot = TraceRecord {
        id: "trc-copilot-001".to_string(),
        session_id: "sess-copilot-001".to_string(),
        run_id: None,
        turn_id: Some("t1".to_string()),
        environment: "production".to_string(),
        release: "0.2.0".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: EvalStatus::Unreviewed,
        payload: json!({ "turns": [{ "turn_id": "t1" }] }),
        total_input_tokens: Some(100),
        total_output_tokens: Some(50),
        total_duration_ms: Some(200),
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let trace_agent = TraceRecord {
        id: "trc-agent-001".to_string(),
        session_id: "sess-agent-001".to_string(),
        run_id: None,
        turn_id: Some("t1".to_string()),
        environment: "production".to_string(),
        release: "0.2.0".to_string(),
        project: Some("pony-agent".to_string()),
        eval_status: EvalStatus::Unreviewed,
        payload: json!({ "turns": [{ "turn_id": "t1" }] }),
        total_input_tokens: Some(150),
        total_output_tokens: Some(70),
        total_duration_ms: Some(300),
        reported_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    repo.record_trace(trace_copilot)
        .await
        .expect("Record copilot trace");
    repo.record_trace(trace_agent)
        .await
        .expect("Record agent trace");

    // 使用 TraceFilter(project="job_copilot") 查询，断言只返回对应项目的记录
    let copilot_filter = TraceFilter {
        session_id: None,
        eval_status: None,
        environment: None,
        release: None,
        project: Some("job_copilot".to_string()),
        limit: None,
        offset: None,
    };
    let copilot_results = repo
        .list_traces(copilot_filter)
        .await
        .expect("Filter copilot");
    assert_eq!(
        copilot_results.len(),
        1,
        "Filtered project 'job_copilot' should return exactly 1 record"
    );
    assert_eq!(copilot_results[0].session_id, "sess-copilot-001");
    assert_eq!(
        copilot_results[0].project.as_deref(),
        Some("job_copilot")
    );

    // 查询 pony-agent
    let agent_filter = TraceFilter {
        session_id: None,
        eval_status: None,
        environment: None,
        release: None,
        project: Some("pony-agent".to_string()),
        limit: None,
        offset: None,
    };
    let agent_results = repo
        .list_traces(agent_filter)
        .await
        .expect("Filter agent");
    assert_eq!(
        agent_results.len(),
        1,
        "Filtered project 'pony-agent' should return exactly 1 record"
    );
    assert_eq!(agent_results[0].session_id, "sess-agent-001");
    assert_eq!(
        agent_results[0].project.as_deref(),
        Some("pony-agent")
    );
}
