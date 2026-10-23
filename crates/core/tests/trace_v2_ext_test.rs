//! trace v1.5 扩展测试（task-18）：turns 归一/warnings、tool_call 截断、run_id 归属与过滤、
//! count_traces / order_by。
//!
//! 断言依据 `.dev-team/trace-fix-contract.md` amend v1.5（§2/§4/§6/§7/§8）。
//! SQLite 用临时库文件；PG 用 `PONY_TEST_PG_URL` 的隔离 schema（未设置则显式 SKIP）。

use chrono::{Duration as ChronoDuration, Utc};
use pony_sentry_core::{
    create_pool,
    models::{EvalStatus, TraceFilter, TraceOrder, TraceRecord},
    repository::TraceRepository,
    PgIssueRepository, SqliteIssueRepository,
};
use serde_json::{json, Value};
use uuid::Uuid;

// ── 夹具 ────────────────────────────────────────────────────────────────────

struct TempDb {
    url: String,
    path: std::path::PathBuf,
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn sqlite_repo(tag: &str) -> (SqliteIssueRepository, TempDb, sqlx::SqlitePool) {
    let path = std::env::temp_dir().join(format!("ponysentry_ext_{tag}_{}.db", Uuid::new_v4()));
    let db = TempDb {
        url: format!("sqlite:{}?mode=rwc", path.display()),
        path,
    };
    let pool = create_pool(&db.url).await.expect("建池失败");
    let repo = SqliteIssueRepository::new(pool.clone());
    repo.migrate().await.expect("迁移失败");
    (repo, db, pool)
}

fn trace(session_id: &str, payload: Value) -> TraceRecord {
    let now = Utc::now();
    TraceRecord {
        id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        run_id: None,
        turn_id: None,
        environment: "development".to_string(),
        release: "dev".to_string(),
        project: Some("job_copilot".to_string()),
        eval_status: EvalStatus::Unreviewed,
        payload,
        total_input_tokens: None,
        total_output_tokens: None,
        total_duration_ms: None,
        turn_status: "unknown".to_string(),
        turns_count: 0,
        note: None,
        reported_at: now,
        created_at: now,
        updated_at: now,
    }
}

// ── I-02：turns 归一 + payload._warnings ─────────────────────────────────────

#[tokio::test]
async fn i02_invalid_turns_are_dropped_and_warnings_persisted() {
    let (repo, _db, _pool) = sqlite_repo("warnings").await;
    let saved = repo
        .record_trace(trace(
            "ext-warn",
            json!({"turns": [
                {"turn_id": "t1", "status": 123, "tool_calls": "not-an-array"},
                "garbage",
                {"turn_id": "t2", "status": "succeeded"}
            ]}),
        ))
        .await
        .expect("写入失败");

    let turns = saved.payload["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 2, "[I-02] 非对象 turn 必须被丢弃");
    assert_eq!(turns[0]["status"], "unknown", "[I-02] status 非字符串归一为 unknown");
    assert!(turns[0].get("tool_calls").is_none(), "[I-02] tool_calls 非数组忽略");
    assert_eq!(
        saved.payload["_warnings"],
        json!([{"turn_index": 1, "reason": "invalid_turn"}]),
        "[I-02] warnings 必须写入 payload._warnings"
    );

    // 二次上报（无新 warning）不得清掉旧 warning
    let saved = repo
        .record_trace(trace(
            "ext-warn",
            json!({"turns": [{"turn_id": "t2", "status": "failed"}]}),
        ))
        .await
        .expect("二次写入失败");
    assert_eq!(
        saved.payload["_warnings"],
        json!([{"turn_index": 1, "reason": "invalid_turn"}]),
        "[I-02] 旧 warnings 必须保留（合集）"
    );
}

// ── I-06：tool_call 文本截断 ────────────────────────────────────────────────

#[tokio::test]
async fn i06_long_tool_call_text_is_truncated_persisted_with_flag() {
    let (repo, _db, pool) = sqlite_repo("truncate").await;
    let long_output = "x".repeat(pony_sentry_core::trace_v2::TOOL_CALL_TEXT_MAX_CHARS + 500);
    let saved = repo
        .record_trace(trace(
            "ext-truncate",
            json!({"turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [{
                "tool_call_id": "c1",
                "tool_name": "format_jd_to_json",
                "status": "error",
                "output": long_output
            }]}]}),
        ))
        .await
        .expect("写入失败");

    let call = &saved.payload["turns"][0]["tool_calls"][0];
    assert_eq!(
        call["output"].as_str().map(|text| text.chars().count()),
        Some(pony_sentry_core::trace_v2::TOOL_CALL_TEXT_MAX_CHARS),
        "[I-06] 聚合 payload 内 output 必须截断"
    );
    assert_eq!(call["truncated"], true, "[I-06] 必须置 truncated:true");

    // 轮次存储（trace_turns.payload）同样落截断值
    let stored: String = sqlx::query("SELECT payload FROM trace_turns WHERE session_id = 'ext-truncate'")
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<String, _>(&row, "payload"))
        .expect("读取轮次 payload 失败");
    let stored: Value = serde_json::from_str(&stored).expect("轮次 payload 必须是 JSON");
    let stored_text = stored["tool_calls"][0]["output"].as_str().expect("output");
    assert_eq!(
        stored_text.chars().count(),
        pony_sentry_core::trace_v2::TOOL_CALL_TEXT_MAX_CHARS,
        "[I-06] trace_turns 内也必须落截断值"
    );
}

// ── W-04：trace_turns.run_id + ?run_id= 双来源过滤 ───────────────────────────

#[tokio::test]
async fn w04_turn_run_id_column_and_filter_matches_turn_or_trace() {
    let (repo, _db, pool) = sqlite_repo("runid").await;
    let mut record = trace(
        "ext-runid",
        json!({"run_id": "run-top", "turns": [
            {"turn_id": "t1", "status": "succeeded", "run_id": "run-turn-a"},
            {"turn_id": "t2", "status": "succeeded"}
        ]}),
    );
    record.run_id = Some("run-top".to_string());
    repo.record_trace(record).await.expect("写入失败");

    // 列填充：t1 用 turn.run_id；t2 回退顶层 run_id
    let rows = sqlx::query(
        "SELECT turn_id, run_id FROM trace_turns WHERE session_id = 'ext-runid' ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .expect("读取 run_id 失败");
    assert_eq!(
        sqlx::Row::get::<Option<String>, _>(&rows[0], "run_id").as_deref(),
        Some("run-turn-a"),
        "[W-04] turn.run_id 优先"
    );
    assert_eq!(
        sqlx::Row::get::<Option<String>, _>(&rows[1], "run_id").as_deref(),
        Some("run-top"),
        "[W-04] 缺失时回退 payload 顶层 run_id"
    );

    // 过滤：任一轮次 run_id 命中即返回；traces.run_id 命中亦返回；无关 run 不返回
    for run_id in ["run-turn-a", "run-top"] {
        let rows = repo
            .list_traces(TraceFilter {
                run_id: Some(run_id.to_string()),
                ..Default::default()
            })
            .await
            .expect("按 run_id 过滤失败");
        assert_eq!(rows.len(), 1, "[W-04] run_id={run_id} 必须命中");
    }
    let none = repo
        .list_traces(TraceFilter {
            run_id: Some("run-other".to_string()),
            ..Default::default()
        })
        .await
        .expect("过滤失败");
    assert!(none.is_empty(), "[W-04] 无关 run_id 不得命中");

    // 0006 迁移后列存在（幂等重跑 migrate 不报错）
    repo.migrate().await.expect("0006 幂等重跑失败");
}

// ── I-04 / W-05：count_traces 与 order_by ───────────────────────────────────

#[tokio::test]
async fn i04_count_traces_matches_filter_and_order_by_switches() {
    let (repo, _db, _pool) = sqlite_repo("count_order").await;
    // 让 created_at 顺序与 reported_at 顺序相反
    let now = Utc::now();
    for (session, reported_offset_minutes) in [("ext-ord-a", 0i64), ("ext-ord-b", -120), ("ext-ord-c", -60)] {
        let mut record = trace(
            session,
            json!({"turns": [{"turn_id": "t1", "status": "succeeded"}]}),
        );
        record.reported_at = now + ChronoDuration::minutes(reported_offset_minutes);
        repo.record_trace(record).await.expect("写入失败");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let default_order = repo
        .list_traces(TraceFilter::default())
        .await
        .expect("默认排序失败");
    let ids: Vec<&str> = default_order
        .iter()
        .map(|row| row.session_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["ext-ord-a", "ext-ord-c", "ext-ord-b"],
        "[v1.5-2] 默认必须按 reported_at DESC"
    );

    let by_created = repo
        .list_traces(TraceFilter {
            order_by: Some(TraceOrder::CreatedAt),
            ..Default::default()
        })
        .await
        .expect("created_at 排序失败");
    let ids: Vec<&str> = by_created.iter().map(|row| row.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["ext-ord-c", "ext-ord-b", "ext-ord-a"],
        "[v1.5-2] order=created_at 必须按入库时间 DESC"
    );

    let total = repo
        .count_traces(TraceFilter {
            project: Some("job_copilot".to_string()),
            ..Default::default()
        })
        .await
        .expect("计数失败");
    assert_eq!(total, 3, "[I-04] count 必须与过滤条件一致");
    let none = repo
        .count_traces(TraceFilter {
            session_id: Some("ext-ord-a".to_string()),
            ..Default::default()
        })
        .await
        .expect("计数失败");
    assert_eq!(none, 1, "[I-04] count 必须应用过滤条件");

    // limit 不影响 count
    let limited = repo
        .list_traces(TraceFilter {
            limit: Some(1),
            ..Default::default()
        })
        .await
        .expect("分页失败");
    assert_eq!(limited.len(), 1);
    assert_eq!(
        repo.count_traces(TraceFilter::default())
            .await
            .expect("计数失败"),
        3,
        "[I-04] count 不受 limit 影响"
    );
}

// ── PG（PONY_TEST_PG_URL 门控）───────────────────────────────────────────────

mod pg {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    async fn isolated_pool(schema: &str) -> Option<sqlx::PgPool> {
        let url = match std::env::var("PONY_TEST_PG_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ => {
                eprintln!("SKIP: PONY_TEST_PG_URL 未设置，跳过 PG 用例（{schema}）");
                return None;
            }
        };
        let base: PgConnectOptions = url.parse().expect("PONY_TEST_PG_URL 不是合法连接串");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(base.clone())
            .await
            .expect("连接 PONY_TEST_PG_URL 失败");
        sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("DROP SCHEMA 失败");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("CREATE SCHEMA 失败");
        Some(
            PgPoolOptions::new()
                .max_connections(4)
                .connect_with(base.options([("search_path", schema.to_string())]))
                .await
                .expect("连接隔离 schema 失败"),
        )
    }

    #[tokio::test]
    async fn w04_pg_run_id_filter_and_0006_schema() {
        let Some(pool) = isolated_pool("ponysentry_ext_runid").await else {
            return;
        };
        let repo = PgIssueRepository::new(pool.clone());
        repo.migrate().await.expect("PG 迁移失败");

        // 0006 幂等报告：列/索引均已存在 ⇒ 0 added / 0 created
        let report = repo
            .ensure_trace_turn_run_id_schema()
            .await
            .expect("0006 ensure 失败");
        assert_eq!(
            (report.columns_added, report.indexes_created),
            (0, 0),
            "[W-04] 二次启动不得重建 run_id 列/索引：{report:?}"
        );
        assert_eq!(report.columns_skipped, 1);
        assert_eq!(report.indexes_skipped, 1);
        let column: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'trace_turns'
               AND column_name = 'run_id'",
        )
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("查询列失败");
        assert_eq!(column, 1, "[W-04] trace_turns.run_id 列必须存在");

        let mut record = trace(
            "pg-ext-runid",
            json!({"run_id": "pg-run-top", "turns": [
                {"turn_id": "t1", "status": "succeeded", "run_id": "pg-run-turn"}
            ]}),
        );
        record.run_id = Some("pg-run-top".to_string());
        repo.record_trace(record).await.expect("PG 写入失败");

        for run_id in ["pg-run-turn", "pg-run-top"] {
            let rows = repo
                .list_traces(TraceFilter {
                    run_id: Some(run_id.to_string()),
                    ..Default::default()
                })
                .await
                .expect("PG run_id 过滤失败");
            assert_eq!(rows.len(), 1, "[W-04] PG run_id={run_id} 必须命中");
        }
    }

    #[tokio::test]
    async fn i04_pg_count_and_order_by() {
        let Some(pool) = isolated_pool("ponysentry_ext_count").await else {
            return;
        };
        let repo = PgIssueRepository::new(pool.clone());
        repo.migrate().await.expect("PG 迁移失败");

        let now = Utc::now();
        for (session, offset) in [("pg-ord-a", 0i64), ("pg-ord-b", -120), ("pg-ord-c", -60)] {
            let mut record = trace(
                session,
                json!({"turns": [{"turn_id": "t1", "status": "succeeded"}]}),
            );
            record.reported_at = now + ChronoDuration::minutes(offset);
            repo.record_trace(record).await.expect("PG 写入失败");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let ids: Vec<String> = repo
            .list_traces(TraceFilter::default())
            .await
            .expect("PG 默认排序失败")
            .into_iter()
            .map(|row| row.session_id)
            .collect();
        assert_eq!(
            ids,
            vec!["pg-ord-a", "pg-ord-c", "pg-ord-b"],
            "[v1.5-2] PG 默认按 reported_at DESC"
        );
        assert_eq!(
            repo.count_traces(TraceFilter {
                project: Some("job_copilot".to_string()),
                ..Default::default()
            })
            .await
            .expect("PG 计数失败"),
            3,
            "[I-04] PG count 必须与过滤一致"
        );
    }
}
