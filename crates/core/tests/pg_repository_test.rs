//! PostgreSQL 回归用例（覆盖 events/traces 的 JSONB 载荷解码路径）。
//!
//! 需要 `PONY_TEST_PG_URL` 指向一个**可写的临时测试库**（例如隔离的 `initdb` 集群）；
//! 未设置时整组跳过，避免在无 PG 的 CI 宿主上误报。
//!
//! 缺陷回归背景：`duplicate_hit` 曾用 `payload`（JSONB）直接解成 `String`，
//! 导致「同 event_id 重复投递」必现 `ColumnDecode … JSONB vs TEXT` panic（空回复 → worker 重试 → quarantine）。

use pony_sentry_core::{
    IssueRepository, NewEventRecord, PgIssueRepository, TraceRecord, TraceRepository,
};
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// 为每个用例创建**独立 schema** 的连接池：避免并行用例同时 `migrate()` 触发
/// PG 目录表竞态（`pg_type_typname_nsp_index`），也避免用例之间互相污染数据。
async fn isolated_repo(schema: &str) -> Option<PgIssueRepository> {
    let url = match std::env::var("PONY_TEST_PG_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("SKIP: PONY_TEST_PG_URL 未设置，跳过 PG 回归用例（{schema}）");
            return None;
        }
    };
    let base: PgConnectOptions = url.parse().expect("PONY_TEST_PG_URL 不是合法 PG 连接串");

    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(base.clone())
        .await
        .expect("连接 PONY_TEST_PG_URL 失败");
    // 每次从零开始（DROP + CREATE），用例自清理且可重复执行
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
    let repo = PgIssueRepository::new(pool);
    repo.migrate().await.expect("migrate 失败");
    Some(repo)
}

fn record<'a>(
    fingerprint: &'a str,
    environment: Option<&'a str>,
    event_key: Option<&'a str>,
) -> NewEventRecord<'a> {
    NewEventRecord {
        fingerprint,
        title: "TypeError: pg regression",
        culprit: Some("api_v2/src/x.py in run"),
        platform: "python",
        release: Some("0.24.35"),
        environment,
        project: Some("job_copilot"),
        app: Some("api_v2"),
        namespace: environment,
        component: Some("agent"),
        level: "error",
        source_kind: Some("log"),
        event_key,
        payload: json!({"message": "pg regression", "extra": {"k": "v"}}),
        occurred_at: None,
    }
}

/// 缺陷回归：同 `event_id` 重复写入必须判定为 duplicate，不得 panic、不得递增计数。
#[tokio::test]
async fn duplicate_event_key_is_idempotent_on_pg() {
    let Some(repo) = isolated_repo("ponysentry_test_dup").await else {
        return;
    };

    let first = repo
        .record_event_and_upsert_issue(record("fp-pg-dup", Some("production"), Some("evt-pg-1")))
        .await
        .expect("首次写入失败");
    assert!(!first.is_duplicate);
    assert_eq!(first.issue.count, 1);
    assert_eq!(
        first.event.payload["message"], "pg regression",
        "写入返回值必须携带原始载荷"
    );

    // 曾在此 panic：ColumnDecode payload JSONB ≠ TEXT
    let second = repo
        .record_event_and_upsert_issue(record("fp-pg-dup", Some("production"), Some("evt-pg-1")))
        .await
        .expect("重复写入（幂等路径）不得失败");
    assert!(second.is_duplicate, "同 event_key 必须判定为 duplicate");
    assert_eq!(second.issue.count, 1, "duplicate 不得递增 count");
    assert_eq!(
        second.issue.last_seen_at, first.issue.last_seen_at,
        "duplicate 不得推进 last_seen_at"
    );
    assert_eq!(second.issue.id, first.issue.id);
    assert_eq!(
        second.event.payload["message"], "pg regression",
        "幂等命中返回的 event 载荷必须可解码"
    );

    let events = repo
        .get_events_for_issue(&first.issue.id, 10)
        .await
        .expect("读取事件失败");
    assert_eq!(events.len(), 1, "duplicate 不得新增 events 行");
    assert_eq!(events[0].payload["extra"]["k"], "v");
    assert_eq!(events[0].event_key.as_deref(), Some("evt-pg-1"));
    assert_eq!(events[0].level.as_deref(), Some("error"));
}

/// events.payload 在 JSONB 与读取路径上的完整往返（含中文/嵌套/null）。
#[tokio::test]
async fn event_payload_roundtrip_on_pg() {
    let Some(repo) = isolated_repo("ponysentry_test_payload").await else {
        return;
    };

    let payload = json!({
        "platform": "python",
        "message": "中文载荷",
        "extra": {"stacktrace_text": "File \"x.py\", line 1, in f", "n": null},
        "breadcrumbs": [{"category": "http", "data": {"url": "https://example.com"}}]
    });
    let mut rec = record("fp-pg-payload", Some("dev"), None);
    rec.payload = payload.clone();
    let saved = repo
        .record_event_and_upsert_issue(rec)
        .await
        .expect("写入失败");
    let events = repo
        .get_events_for_issue(&saved.issue.id, 5)
        .await
        .expect("读取失败");
    assert_eq!(events[0].payload, payload, "JSONB 往返必须无损");
}

/// traces.payload 同属 JSONB：写入合并与读取都必须可用。
#[tokio::test]
async fn trace_payload_roundtrip_on_pg() {
    let Some(repo) = isolated_repo("ponysentry_test_trace").await else {
        return;
    };

    let now = chrono::Utc::now();
    let trace = TraceRecord {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: "pg-session-1".to_string(),
        run_id: None,
        turn_id: None,
        environment: "production".to_string(),
        release: "0.24.35".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: pony_sentry_core::EvalStatus::Unreviewed,
        payload: json!({"turns": [{"turn_id": "t1", "output": "第一轮"}]}),
        total_input_tokens: Some(10),
        total_output_tokens: Some(20),
        total_duration_ms: Some(30),
        reported_at: now,
        created_at: now,
        updated_at: now,
    };
    repo.record_trace(trace).await.expect("trace 首次写入失败");

    // 同 session 增量合并（覆盖 traces.payload 的 JSONB 读取 + 合并 + 回写）
    let second = TraceRecord {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: "pg-session-1".to_string(),
        run_id: None,
        turn_id: None,
        environment: "production".to_string(),
        release: "0.24.35".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: pony_sentry_core::EvalStatus::Unreviewed,
        payload: json!({"turns": [{"turn_id": "t2", "output": "第二轮"}]}),
        total_input_tokens: Some(10),
        total_output_tokens: Some(20),
        total_duration_ms: Some(30),
        reported_at: now,
        created_at: now,
        updated_at: now,
    };
    let merged = repo.record_trace(second).await.expect("trace 合并失败");
    let turns = merged.payload["turns"]
        .as_array()
        .expect("turns 必须是数组");
    assert_eq!(
        turns.len(),
        2,
        "增量上报必须合并轮次，实际：{}",
        merged.payload
    );

    let traces = repo
        .list_traces(Default::default())
        .await
        .expect("列表读取失败");
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].session_id, "pg-session-1");
}

/// 跨环境作用域在 PG 上的行为（唯一索引 (fingerprint, environment) 生效）。
#[tokio::test]
async fn environment_scopes_issues_on_pg() {
    let Some(repo) = isolated_repo("ponysentry_test_env").await else {
        return;
    };

    let dev = repo
        .record_event_and_upsert_issue(record("fp-pg-env-dev", Some("dev"), None))
        .await
        .expect("dev 写入失败");
    let prod = repo
        .record_event_and_upsert_issue(record("fp-pg-env-production", Some("production"), None))
        .await
        .expect("production 写入失败");
    assert_ne!(dev.issue.id, prod.issue.id);
    assert_eq!(dev.issue.environment, "dev");
    assert_eq!(prod.issue.environment, "production");
}
