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
