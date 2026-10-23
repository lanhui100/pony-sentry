//! trace v2 仓储契约测试（task-7）：合并语义、聚合口径、诊断标注、迁移回填、并发安全、
//! 列表过滤与保留期。断言依据 `.dev-team/trace-fix-contract.md` C1–C6。
//!
//! - SQLite：临时库文件（真实并发/事务语义，不用 `:memory:` 以免连接隔离干扰）；
//! - PG：`PONY_TEST_PG_URL` 指向可写测试库时运行（独立 schema，未设置则显式 SKIP）。

use chrono::{Duration as ChronoDuration, Utc};
use pony_sentry_core::{
    create_pool,
    models::{EvalStatus, TraceFilter, TraceRecord},
    repository::TraceRepository,
    SqliteIssueRepository,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

// ── 夹具 ────────────────────────────────────────────────────────────────────

struct TempDb {
    url: String,
    path: PathBuf,
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn temp_db(tag: &str) -> TempDb {
    let path = std::env::temp_dir().join(format!(
        "ponysentry_v2_{tag}_{}.db",
        Uuid::new_v4()
    ));
    TempDb {
        url: format!("sqlite:{}?mode=rwc", path.display()),
        path,
    }
}

async fn sqlite_repo(tag: &str) -> (SqliteIssueRepository, TempDb, sqlx::SqlitePool) {
    let db = temp_db(tag);
    let pool = create_pool(&db.url).await.expect("创建临时 SQLite 池失败");
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

fn session_filter(session_id: &str) -> TraceFilter {
    TraceFilter {
        session_id: Some(session_id.to_string()),
        ..Default::default()
    }
}

fn only_turn(record: &TraceRecord, turn_id: &str) -> Value {
    record.payload["turns"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|turn| turn["turn_id"] == turn_id)
        .unwrap_or_else(|| panic!("payload.turns 缺少 {turn_id}：{}", record.payload))
}

// ── C2 合并语义 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn v2_terminal_status_wins_and_tool_calls_merge_by_id() {
    let (repo, _db, _pool) = sqlite_repo("terminal").await;
    let session = "v2-terminal";

    repo.record_trace(trace(
        session,
        json!({"turns": [{"turn_id": "t1", "status": "running", "tool_calls": [
            {"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "ok"}
        ]}]}),
    ))
    .await
    .expect("首报 running 失败");

    let saved = repo
        .record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "t1", "status": "failed",
                "error": {"type": "TimeoutError", "message": "boom"},
                "tool_calls": [
                    {"tool_call_id": "c1", "tool_name": "parse", "status": "error", "error": {"message": "解析失败"}},
                    {"tool_call_id": "c2", "tool_name": "retry", "status": "success", "output": "ok"}
                ]}]}),
        ))
        .await
        .expect("二报 failed 失败");

    let turn = only_turn(&saved, "t1");
    assert_eq!(turn["status"], "failed", "[C2.1] 新终态必须覆盖 running");
    assert_eq!(turn["error"]["message"], "boom", "[C2.1] 迟到补全的 error 必须保留");
    let calls = turn["tool_calls"].as_array().expect("tool_calls");
    assert_eq!(calls.len(), 2, "[C2.3] 同 id 合并 + 新 id 追加");
    assert_eq!(calls[0]["tool_call_id"], "c1");
    assert_eq!(calls[0]["status"], "error", "[C2.3] 同 id 新状态覆盖旧条目");
    assert_eq!(calls[1]["tool_call_id"], "c2");
    assert_eq!(saved.turn_status, "failed", "[C1.2] turn_status 取末轮 status");

    // 非终态上报不得回退终态，也不得清空 error
    let saved = repo
        .record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "t1", "status": "running"}]}),
        ))
        .await
        .expect("三报 running 失败");
    let turn = only_turn(&saved, "t1");
    assert_eq!(turn["status"], "failed", "[C2.1] 旧终态 + 新非终态禁止回退");
    assert_eq!(turn["error"]["message"], "boom", "[C2.2] 非空旧值不得被清空");
}

#[tokio::test]
async fn v2_new_turn_is_appended_with_monotonic_seq() {
    let (repo, _db, _pool) = sqlite_repo("seq").await;
    let session = "v2-seq";

    for (turn_id, status) in [("t1", "succeeded"), ("t2", "failed")] {
        repo.record_trace(trace(
            session,
            json!({"turns": [{"turn_id": turn_id, "status": status}]}),
        ))
        .await
        .expect("写入轮次失败");
    }
    let record = repo
        .list_traces(session_filter(session))
        .await
        .expect("读取失败")
        .remove(0);
    let turns = record.payload["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["turn_id"], "t1");
    assert_eq!(turns[0]["seq"], 0, "[C2.5] seq 从 0 起");
    assert_eq!(turns[1]["turn_id"], "t2");
    assert_eq!(turns[1]["seq"], 1, "[C2.5] seq = max+1");
    assert_eq!(record.turns_count, 2);
    assert_eq!(record.turn_status, "failed");
}

// ── C2 会话级聚合口径 ────────────────────────────────────────────────────────

#[tokio::test]
async fn v2_totals_sum_turns_and_top_level_fields_merge() {
    let (repo, _db, _pool) = sqlite_repo("totals").await;
    let session = "v2-totals";

    let mut first = trace(
        session,
        json!({"session_id": session, "eval_status": "triage_good",
            "extra": {"project_path": "a", "keep": "x"},
            "total_input_tokens": 99999, "total_output_tokens": 99999,
            "turns": [{"turn_id": "t1", "status": "succeeded",
                       "input_tokens": 10, "output_tokens": 5, "duration_ms": 100}]}),
    );
    first.eval_status = EvalStatus::TriageGood;
    repo.record_trace(first).await.expect("首报失败");

    let saved = repo
        .record_trace(trace(
            session,
            json!({"session_id": session, "extra": {"project_path": "b"},
                "turns": [{"turn_id": "t2", "status": "failed",
                           "input_tokens": 20, "output_tokens": 7, "duration_ms": 200}]}),
        ))
        .await
        .expect("二报失败");

    assert_eq!(saved.total_input_tokens, Some(30), "[C2] total = turns 求和（拒绝客户端 99999）");
    assert_eq!(saved.total_output_tokens, Some(12));
    assert_eq!(saved.total_duration_ms, Some(300));
    assert_eq!(saved.payload["total_input_tokens"], 30, "[C2] payload 与列同口径");
    assert_eq!(saved.payload["extra"]["project_path"], "b", "[C2] extra 非空新值覆盖");
    assert_eq!(saved.payload["extra"]["keep"], "x", "[C2] 二次上报缺省字段保留旧值");
    assert_eq!(saved.eval_status, EvalStatus::TriageGood, "[C1.2] eval_status 列保留首报值");
    assert!(saved.payload.get("eval_status").is_none(), "[C2] payload.eval_status 必须移除");
    assert_eq!(saved.payload["session_id_source"], "session_id", "[C5] 主键来源回写");
    assert_eq!(saved.turn_status, "failed");
    assert_eq!(saved.turns_count, 2);
}

#[tokio::test]
async fn v2_totals_are_null_when_no_turn_carries_token_fields() {
    let (repo, _db, _pool) = sqlite_repo("nulltotals").await;
    let session = "v2-null-totals";
    let mut input = trace(
        session,
        json!({"total_input_tokens": 7, "turns": [{"turn_id": "t1", "status": "succeeded"}]}),
    );
    input.total_input_tokens = Some(7);
    let saved = repo.record_trace(input).await.expect("写入失败");
    assert_eq!(
        saved.total_input_tokens, None,
        "[C2] 无任何 turn 含 input_tokens ⇒ total 必须为 null（客户端直传值被拒）"
    );
    assert_eq!(saved.total_duration_ms, None);
}

#[tokio::test]
async fn v2_thread_id_fallback_marks_session_id_source() {
    let (repo, _db, _pool) = sqlite_repo("source").await;
    let saved = repo
        .record_trace(trace(
            "v2-thread",
            json!({"session_id": null, "thread_id": "v2-thread",
                "turns": [{"turn_id": "t1", "status": "succeeded"}]}),
        ))
        .await
        .expect("写入失败");
    assert_eq!(
        saved.payload["session_id_source"], "thread_id",
        "[C5] thread_id 回退必须标注 session_id_source=thread_id"
    );
}

// ── C4 诊断标注 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn v2_diagnostics_are_generated_by_server() {
    let (repo, _db, _pool) = sqlite_repo("diagnostics").await;

    let failed_without_error = repo
        .record_trace(trace(
            "v2-diag-1",
            json!({"turns": [{"turn_id": "t1", "status": "failed", "tool_calls": [
                {"tool_call_id": "c1", "tool_name": "parse", "status": "success", "output": "ok"}]}]}),
        ))
        .await
        .expect("写入失败");
    assert_eq!(
        only_turn(&failed_without_error, "t1")["diagnostics"]["error_contract"], "missing",
        "[C4.1] failed 且无错误明细必须标注 error_contract=missing"
    );

    let status_conflict = repo
        .record_trace(trace(
            "v2-diag-2",
            json!({"turns": [{"turn_id": "t1", "status": "completed", "tool_calls": [
                {"tool_call_id": "c1", "tool_name": "format_jd_to_json", "status": "success",
                 "output": "职位解析失败（原因: 模型返回空结果）"}]}]}),
        ))
        .await
        .expect("写入失败");
    assert_eq!(
        only_turn(&status_conflict, "t1")["diagnostics"]["status_conflict"], true,
        "[C4.2] success + 失败文案必须标注 status_conflict"
    );

    let error_missing = repo
        .record_trace(trace(
            "v2-diag-3",
            json!({"turns": [{"turn_id": "t1", "status": "completed", "tool_calls": [
                {"tool_call_id": "c1", "tool_name": "format_jd_to_json", "status": "error"}]}]}),
        ))
        .await
        .expect("写入失败");
    assert_eq!(
        only_turn(&error_missing, "t1")["diagnostics"]["error_missing"], true,
        "[C4.3] tool_call status=error 且无 error 必须标注 error_missing"
    );

    // 健康轮次不得出现 diagnostics 字段
    let healthy = repo
        .record_trace(trace(
            "v2-diag-4",
            json!({"turns": [{"turn_id": "t1", "status": "succeeded", "tool_calls": [
                {"tool_call_id": "c1", "status": "success", "output": "ok"}]}]}),
        ))
        .await
        .expect("写入失败");
    assert!(
        only_turn(&healthy, "t1").get("diagnostics").is_none(),
        "[C4] 无问题时不得写入 diagnostics"
    );
}

// ── C1.2 评审备注 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn v2_note_patch_persists_and_survives_reingest() {
    let (repo, _db, _pool) = sqlite_repo("note").await;
    let session = "v2-note";
    let created = repo
        .record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "t1", "status": "succeeded"}]}),
        ))
        .await
        .expect("写入失败");
    assert!(created.note.is_none());

    let patched = repo
        .update_trace_review(
            &created.id,
            EvalStatus::TriageBad,
            Some("人工复核：失败原因已确认"),
        )
        .await
        .expect("PATCH note 失败");
    assert_eq!(patched.note.as_deref(), Some("人工复核：失败原因已确认"));
    assert_eq!(patched.eval_status, EvalStatus::TriageBad);

    let detail = repo.get_trace(&created.id).await.expect("详情读取失败");
    assert_eq!(detail.note.as_deref(), Some("人工复核：失败原因已确认"), "[C9.7] 详情必须回显 note");
    let listed = repo
        .list_traces(session_filter(session))
        .await
        .expect("列表读取失败");
    assert_eq!(listed[0].note.as_deref(), Some("人工复核：失败原因已确认"), "[C9.7] 列表必须回显 note");

    // note=None 不覆盖；再次上报也不覆盖
    let kept = repo
        .update_trace_review(&created.id, EvalStatus::TriageGood, None)
        .await
        .expect("仅改状态失败");
    assert_eq!(kept.note.as_deref(), Some("人工复核：失败原因已确认"), "None 必须保留旧 note");

    repo.record_trace(trace(
        session,
        json!({"turns": [{"turn_id": "t2", "status": "failed"}]}),
    ))
    .await
    .expect("二次上报失败");
    let after = repo.get_trace(&created.id).await.expect("读取失败");
    assert_eq!(after.note.as_deref(), Some("人工复核：失败原因已确认"), "[C1.2] 上报不得覆盖 note");
    assert_eq!(after.turns_count, 2, "[C9.7] turns_count 必须随写入推进");
    assert_eq!(after.turn_status, "failed", "[C9.7] turn_status 必须随末轮推进");
}

// ── C5 列表过滤 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn v2_list_supports_run_id_turn_status_and_time_window() {
    let (repo, _db, _pool) = sqlite_repo("filters").await;

    let mut run_a = trace(
        "v2-filter-a",
        json!({"turns": [{"turn_id": "t1", "status": "failed"}]}),
    );
    run_a.run_id = Some("run-a".to_string());
    run_a.reported_at = Utc::now() - ChronoDuration::days(2);
    repo.record_trace(run_a).await.expect("写入失败");

    let mut run_b = trace(
        "v2-filter-b",
        json!({"turns": [{"turn_id": "t1", "status": "succeeded"}]}),
    );
    run_b.run_id = Some("run-b".to_string());
    repo.record_trace(run_b).await.expect("写入失败");

    let by_run = repo
        .list_traces(TraceFilter {
            run_id: Some("run-a".to_string()),
            ..Default::default()
        })
        .await
        .expect("按 run_id 过滤失败");
    assert_eq!(by_run.len(), 1, "[C5] run_id 过滤必须生效");
    assert_eq!(by_run[0].session_id, "v2-filter-a");

    let by_status = repo
        .list_traces(TraceFilter {
            turn_status: Some("failed".to_string()),
            ..Default::default()
        })
        .await
        .expect("按 turn_status 过滤失败");
    assert_eq!(by_status.len(), 1, "[C5] turn_status 过滤必须生效");
    assert_eq!(by_status[0].session_id, "v2-filter-a");

    let since = repo
        .list_traces(TraceFilter {
            from: Some(Utc::now() - ChronoDuration::days(1)),
            ..Default::default()
        })
        .await
        .expect("按时间窗过滤失败");
    assert_eq!(since.len(), 1, "[C5] from 必须按 reported_at 过滤");
    assert_eq!(since[0].session_id, "v2-filter-b");

    let until = repo
        .list_traces(TraceFilter {
            to: Some(Utc::now() - ChronoDuration::days(1)),
            ..Default::default()
        })
        .await
        .expect("按时间窗过滤失败");
    assert_eq!(until.len(), 1, "[C5] to 必须按 reported_at 过滤");
    assert_eq!(until[0].session_id, "v2-filter-a");

    // C5：limit clamp(1,100)、offset 负数兜底为 0（不得拼出非法 SQL）
    let clamped = repo
        .list_traces(TraceFilter {
            limit: Some(100_000),
            offset: Some(-5),
            ..Default::default()
        })
        .await
        .expect("clamp/offset 兜底失败");
    assert_eq!(clamped.len(), 2, "[C5] limit 必须 clamp（此处仅 2 行）且 offset<0 不得报错");
}

// ── C6 保留期 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn v2_retention_judges_by_updated_at() {
    let (repo, _db, pool) = sqlite_repo("retention").await;

    for session in ["v2-ret-stale", "v2-ret-fresh"] {
        repo.record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "t1", "status": "succeeded"}]}),
        ))
        .await
        .expect("写入失败");
    }
    let old = (Utc::now() - ChronoDuration::days(10)).to_rfc3339();
    let fresh = Utc::now().to_rfc3339();
    // stale：created/updated 都旧；fresh：created 旧但 updated 新（代表"近期仍在写入"的会话）
    sqlx::query("UPDATE traces SET created_at = ?, updated_at = ? WHERE session_id = 'v2-ret-stale'")
        .bind(&old)
        .bind(&old)
        .execute(&pool)
        .await
        .expect("回拨 stale 时间失败");
    sqlx::query("UPDATE traces SET created_at = ?, updated_at = ? WHERE session_id = 'v2-ret-fresh'")
        .bind(&old)
        .bind(&fresh)
        .execute(&pool)
        .await
        .expect("回拨 fresh 时间失败");

    let deleted = repo
        .delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 100)
        .await
        .expect("retention 失败");
    assert_eq!(deleted, 1, "[C6] 只应删除 updated_at 也过期的行");
    assert!(repo
        .list_traces(session_filter("v2-ret-stale"))
        .await
        .expect("读取失败")
        .is_empty());
    assert_eq!(
        repo.list_traces(session_filter("v2-ret-fresh"))
            .await
            .expect("读取失败")
            .len(),
        1,
        "[C6] updated_at 新的行不得因 created_at 旧被误删"
    );
    // trace_turns 无外键：聚合行被删后其轮次不得留下孤儿
    let orphans: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns WHERE session_id = 'v2-ret-stale'")
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计孤儿轮次失败");
    assert_eq!(orphans, 0, "[C6] retention 必须同时清理被删会话的 trace_turns");
    let survivors: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns WHERE session_id = 'v2-ret-fresh'")
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计存活轮次失败");
    assert_eq!(survivors, 1, "[C6] 存活会话的轮次不得被误删");
}

// ── C1.3 迁移回填 ───────────────────────────────────────────────────────────

/// 直接插入一行「旧形态」聚合行（只有 payload.turns，无 trace_turns 行）。
///
/// `turns_count = None` 表示库尚未迁移到 0005（无该列），用于验证迁移回填；`Some(n)` 用于
/// 构造"部分覆盖"（列已存在但行数不足）的场景。
async fn insert_raw_trace_row(
    pool: &sqlx::SqlitePool,
    id: &str,
    session_id: &str,
    payload: Value,
    turns_count: Option<i64>,
) {
    let sql = match turns_count {
        Some(_) => {
            "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project,
                 eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms,
                 turns_count, reported_at, created_at, updated_at)
             VALUES (?, ?, NULL, NULL, 'development', 'dev', 'job_copilot', 'unreviewed', ?, 12, 5, 24,
                 ?, '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00')"
        }
        None => {
            "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project,
                 eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms,
                 reported_at, created_at, updated_at)
             VALUES (?, ?, NULL, NULL, 'development', 'dev', 'job_copilot', 'unreviewed', ?, 12, 5, 24,
                 '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00')"
        }
    };
    let mut query = sqlx::query(sql).bind(id).bind(session_id).bind(payload.to_string());
    if let Some(count) = turns_count {
        query = query.bind(count);
    }
    query.execute(pool).await.expect("插入旧形态行失败");
}

async fn seed_legacy_sqlite(pool: &sqlx::SqlitePool) {
    for sql in [
        include_str!("../../../migrations/0001_init.sql"),
        include_str!("../../../migrations/0002_add_project.sql"),
        include_str!("../../../migrations/0003_add_traces.sql"),
        include_str!("../../../migrations/0004_add_issue_dimensions.sql"),
    ] {
        sqlx::raw_sql(sql)
            .execute(pool)
            .await
            .expect("应用旧迁移失败");
    }
    sqlx::query(
        "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project,
             eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms,
             reported_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("legacy-row-1")
    .bind("legacy-sess-1")
    .bind("run-legacy")
    .bind(None::<String>)
    .bind("development")
    .bind("dev")
    .bind("job_copilot")
    .bind("unreviewed")
    .bind(
        json!({
            "turns": [
                {"turn_id": "lt-1", "status": "succeeded", "input_tokens": 7},
                {"turn_id": "lt-2", "status": "failed", "input_tokens": 5}
            ],
            "extra": {"legacy": true}
        })
        .to_string(),
    )
    .bind(12)
    .bind(5)
    .bind(24)
    .bind("2026-10-01T00:00:00+00:00")
    .bind("2026-10-01T00:00:00+00:00")
    .bind("2026-10-01T00:00:00+00:00")
    .execute(pool)
    .await
    .expect("插入旧形态行失败");
}

#[tokio::test]
async fn v2_migration_backfills_trace_turns_from_legacy_payload() {
    let db = temp_db("backfill");
    let pool = create_pool(&db.url).await.expect("建池失败");
    seed_legacy_sqlite(&pool).await;

    let repo = SqliteIssueRepository::new(pool.clone());
    repo.migrate().await.expect("0005 迁移失败");

    let rows: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns WHERE session_id = 'legacy-sess-1'")
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计 trace_turns 失败");
    assert_eq!(rows, 2, "[C1.3] 旧 payload 必须回填 2 行");

    let seqs: String = sqlx::query(
        "SELECT GROUP_CONCAT(turn_id, ',') AS ids FROM (
            SELECT turn_id FROM trace_turns WHERE session_id = 'legacy-sess-1' ORDER BY seq)",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx::Row::get::<String, _>(&row, "ids"))
    .expect("读取 seq 顺序失败");
    assert_eq!(seqs, "lt-1,lt-2", "[C1.3] seq 必须按原 turns 顺序生成");

    let record = repo
        .list_traces(session_filter("legacy-sess-1"))
        .await
        .expect("读取旧行失败")
        .remove(0);
    assert_eq!(record.turns_count, 2, "[C1.3] turns_count 必须等于回填行数");
    assert_eq!(record.turn_status, "failed", "[C1.3] turn_status 取最后一个 turn 的 status");
    assert_eq!(record.payload["turns"].as_array().map(Vec::len), Some(2));

    // 幂等：重复 migrate 不得重复回填
    repo.migrate().await.expect("重复迁移必须幂等");
    let rows_again: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM trace_turns WHERE session_id = 'legacy-sess-1'")
            .fetch_one(&pool)
            .await
            .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
            .expect("统计失败");
    assert_eq!(rows_again, 2, "[C1.3] 重复迁移不得重复回填");
}

#[tokio::test]
async fn v2_legacy_payload_is_hydrated_on_next_ingest() {
    let (repo, _db, pool) = sqlite_repo("hydrate").await;
    // 迁移完成后再插入"只有 payload.turns、没有 trace_turns 行"的聚合行（模拟升级窗口 / 手工导入）
    sqlx::query(
        "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project,
             eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms,
             reported_at, created_at, updated_at)
         VALUES ('legacy-window-1', 'v2-hydrate', NULL, NULL, 'development', 'dev',
             'job_copilot', 'unreviewed', ?, NULL, NULL, NULL, '2026-10-01T00:00:00+00:00',
             '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00')",
    )
    .bind(json!({"turns": [
        {"turn_id": "lt-1", "status": "succeeded"},
        {"turn_id": "lt-2", "status": "failed"}
    ]})
    .to_string())
    .execute(&pool)
    .await
    .expect("插入升级窗口行失败");

    let saved = repo
        .record_trace(trace(
            "v2-hydrate",
            json!({"turns": [{"turn_id": "lt-3", "status": "failed"}]}),
        ))
        .await
        .expect("新上报失败");

    let turns = saved.payload["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 3, "[C1.3] 历史轮次必须先补齐再合并新轮次");
    assert_eq!(turns[0]["turn_id"], "lt-1");
    assert_eq!(turns[1]["turn_id"], "lt-2");
    assert_eq!(turns[2]["turn_id"], "lt-3");
    assert_eq!(saved.turns_count, 3);
    assert_eq!(saved.turn_status, "failed");
}

// ── C3 并发 ─────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn v2_concurrent_ingest_loses_no_turn_on_sqlite() {
    let (repo, _db, _pool) = sqlite_repo("concurrent").await;
    let session = "v2-concurrent";

    let mut handles = Vec::new();
    for worker in 0..20u32 {
        let repo = repo.clone();
        let session = session.to_string();
        handles.push(tokio::spawn(async move {
            for slot in 0..2u32 {
                repo.record_trace(trace(
                    &session,
                    json!({"turns": [{"turn_id": format!("r{worker}-t{slot}"), "status": "succeeded"}]}),
                ))
                .await
                .unwrap_or_else(|error| panic!("并发上报 {worker}/{slot} 失败：{error}"));
            }
        }));
    }
    for handle in handles {
        handle.await.expect("并发任务 panic");
    }

    let record = repo
        .list_traces(session_filter(session))
        .await
        .expect("读取失败")
        .remove(0);
    assert_eq!(record.turns_count, 40, "[C3] 20 并发 × 2 turn 必须全部入库（实际 {}）", record.turns_count);
    assert_eq!(
        record.payload["turns"].as_array().map(Vec::len),
        Some(40),
        "[C3] payload.turns 必须含 40 条"
    );
}

// ── amend v1.4-1（L3A-F1/F2）：回填键与运行时同构 ───────────────────────────

#[tokio::test]
async fn v2_backfill_missing_turn_ids_use_runtime_sha256_keys() {
    let db = temp_db("backfill_keys");
    let pool = create_pool(&db.url).await.expect("建池失败");
    seed_legacy_sqlite(&pool).await; // 已应用 0001–0004 且含一行 legacy（带 turn_id）

    // 追加一行：两条**没有 turn_id** 的 legacy turn（内容不同）
    let legacy_payload = json!({"turns": [
        {"status": "succeeded", "input_tokens": 3},
        {"status": "failed", "input_tokens": 4}
    ]});
    insert_raw_trace_row(
        &pool,
        "legacy-no-id",
        "legacy-no-id-sess",
        legacy_payload.clone(),
        None,
    )
    .await;

    let repo = SqliteIssueRepository::new(pool.clone());
    repo.migrate().await.expect("0005 迁移 + Rust 回填失败");

    let rows = sqlx::query(
        "SELECT turn_id FROM trace_turns WHERE session_id = 'legacy-no-id-sess' ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .expect("读取回填行失败");
    assert_eq!(rows.len(), 2, "[L3A-F1] 两条缺 turn_id 的 legacy turn 必须回填为两行");
    let keys: Vec<String> = rows
        .iter()
        .map(|row| sqlx::Row::get::<String, _>(row, "turn_id"))
        .collect();
    assert_ne!(keys[0], keys[1], "[L3A-F1] 不得塌成一行");
    // 与运行时 turn_key_of 同构（sha256 / turn_srv_ 前缀）
    let turns = legacy_payload["turns"].as_array().unwrap();
    for (index, key) in keys.iter().enumerate() {
        assert!(
            key.starts_with("turn_srv_") && key.len() == "turn_srv_".len() + 16,
            "回填键形态必须是 turn_srv_<16 hex>（amend v1.4-2）：{key}"
        );
        assert_eq!(
            key,
            &pony_sentry_core::trace_v2::turn_key_of(&turns[index]),
            "[L3A-F1] 回填键必须与运行时 turn_key_of 同构"
        );
    }

    let record = repo
        .list_traces(session_filter("legacy-no-id-sess"))
        .await
        .expect("读取失败")
        .remove(0);
    assert_eq!(record.turns_count, 2, "回填后 turns_count 必须为 2");
    assert_eq!(record.turn_status, "failed", "turn_status 取末轮 status");

    // 回填不重写 payload（C1.3 只写 trace_turns + 聚合列）；下一次写入时按回填键聚合，
    // 既有两轮不得重复生成、也不得丢。
    let saved = repo
        .record_trace(trace(
            "legacy-no-id-sess",
            json!({"turns": [{"turn_id": "c3", "status": "succeeded"}]}),
        ))
        .await
        .expect("新上报失败");
    let ids: Vec<String> = saved.payload["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .map(|turn| turn["turn_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![keys[0].clone(), keys[1].clone(), "c3".to_string()]);
}

#[tokio::test]
async fn v2_backfill_does_not_collapse_identical_legacy_turns() {
    let db = temp_db("backfill_collision");
    let pool = create_pool(&db.url).await.expect("建池失败");
    seed_legacy_sqlite(&pool).await;

    // 两条内容**完全相同**且都没有 turn_id 的 legacy turn
    let identical = json!({"turns": [
        {"status": "succeeded", "output": "same"},
        {"status": "succeeded", "output": "same"}
    ]});
    insert_raw_trace_row(&pool, "legacy-dup", "legacy-dup-sess", identical, None).await;

    let repo = SqliteIssueRepository::new(pool.clone());
    repo.migrate().await.expect("回填失败");

    let keys: Vec<String> = sqlx::query(
        "SELECT turn_id FROM trace_turns WHERE session_id = 'legacy-dup-sess' ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .expect("读取失败")
    .iter()
    .map(|row| sqlx::Row::get::<String, _>(row, "turn_id"))
    .collect();
    assert_eq!(keys.len(), 2, "[L3A-F1] 内容碰撞也必须两行");
    assert_ne!(keys[0], keys[1], "[L3A-F1] 内容碰撞不得塌陷");
    assert!(keys[1].ends_with("_2"), "碰撞后缀必须确定（_2）：{}", keys[1]);

    // 幂等：再次 migrate 不新增、不改键
    repo.migrate().await.expect("重复迁移失败");
    let keys_again: Vec<String> = sqlx::query(
        "SELECT turn_id FROM trace_turns WHERE session_id = 'legacy-dup-sess' ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .expect("读取失败")
    .iter()
    .map(|row| sqlx::Row::get::<String, _>(row, "turn_id"))
    .collect();
    assert_eq!(keys_again, keys, "重复回填必须幂等");
}

// ── amend v1.4-1（L3A-F3）：hydration 覆盖度 ────────────────────────────────

#[tokio::test]
async fn v2_hydration_fills_when_coverage_is_partial() {
    let (repo, _db, pool) = sqlite_repo("coverage").await;
    let session = "v2-coverage";

    // 造一条"部分覆盖"的存量：payload 有 2 条 turn，但 trace_turns 只有第 1 条
    let payload = json!({"turns": [
        {"turn_id": "c1", "status": "succeeded"},
        {"turn_id": "c2", "status": "failed"}
    ]});
    insert_raw_trace_row(&pool, "coverage-row", session, payload, Some(1)).await;
    sqlx::query(
        "INSERT INTO trace_turns (session_id, turn_id, seq, status, payload, created_at, updated_at)
         VALUES (?, 'c1', 0, 'succeeded', ?, '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00')",
    )
    .bind(session)
    .bind(json!({"turn_id": "c1", "status": "succeeded"}).to_string())
    .execute(&pool)
    .await
    .expect("插入部分覆盖行失败");

    let saved = repo
        .record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "c3", "status": "succeeded"}]}),
        ))
        .await
        .expect("写入失败");

    let turns = saved.payload["turns"].as_array().expect("turns");
    let ids: Vec<&str> = turns.iter().map(|turn| turn["turn_id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["c1", "c2", "c3"], "[L3A-F3] 覆盖度不足时必须补齐缺失轮次");
    assert_eq!(saved.turns_count, 3);
}

// ── amend v1.4-3（L3A-F4）：retention 不得误删存活会话的轮次 ──────────────────

#[tokio::test]
async fn v2_retention_never_deletes_turns_of_surviving_sessions() {
    let (repo, _db, pool) = sqlite_repo("retention_race").await;
    for session in ["v2-ret-a", "v2-ret-b"] {
        for turn_id in ["t1", "t2"] {
            repo.record_trace(trace(
                session,
                json!({"turns": [{"turn_id": turn_id, "status": "succeeded"}]}),
            ))
            .await
            .expect("写入失败");
        }
    }
    let old = (Utc::now() - ChronoDuration::days(10)).to_rfc3339();
    sqlx::query("UPDATE traces SET created_at = ?, updated_at = ?")
        .bind(&old)
        .bind(&old)
        .execute(&pool)
        .await
        .expect("回拨时间失败");

    // batch_size=1：每批只删一行，另一行必须连同其轮次完好保留
    assert_eq!(
        repo.delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 1)
            .await
            .expect("retention 失败"),
        1
    );
    let survivors: i64 = sqlx::query("SELECT COUNT(*) AS n FROM traces")
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计失败");
    assert_eq!(survivors, 1);
    let surviving_turns: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM trace_turns
         WHERE session_id IN (SELECT session_id FROM traces)",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
    .expect("统计失败");
    assert_eq!(surviving_turns, 2, "[L3A-F4] 存活会话的轮次不得被误删");

    // 再删一批：全部清空且无孤儿
    assert_eq!(
        repo.delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 1)
            .await
            .expect("retention 失败"),
        1
    );
    let leftovers: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns")
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计失败");
    assert_eq!(leftovers, 0, "[L3A-F4] 删除后不得留下孤儿轮次");
}

// ── amend v1.4-9（L3A-F9）：totals 溢出 ────────────────────────────────────

#[tokio::test]
async fn v2_totals_overflow_marks_stats_incomplete_instead_of_panicking() {
    let (repo, _db, _pool) = sqlite_repo("overflow").await;
    let saved = repo
        .record_trace(trace(
            "v2-overflow",
            json!({"turns": [
                {"turn_id": "t1", "status": "succeeded", "input_tokens": i64::MAX},
                {"turn_id": "t2", "status": "succeeded", "input_tokens": 1, "output_tokens": 7}
            ]}),
        ))
        .await
        .expect("溢出上报不得 panic/报错");
    assert_eq!(
        saved.total_input_tokens, None,
        "[L3A-F9] i64 溢出必须跳过该 total（列置 null）"
    );
    assert_eq!(saved.total_output_tokens, Some(7), "未溢出字段照常求和");
    assert_eq!(
        saved.payload["stats_incomplete"], true,
        "[L3A-F9] 溢出必须标记 payload.stats_incomplete"
    );
    assert!(saved.payload["total_input_tokens"].is_null());
}

// ── amend v1.4-1（L3A-F6）：迁移幂等（含回填插入路径真实重放）────────────────

#[tokio::test]
async fn v2_migration_twice_keeps_turn_store_stable() {
    let db = temp_db("migrate_twice");
    let pool = create_pool(&db.url).await.expect("建池失败");
    seed_legacy_sqlite(&pool).await;
    let repo = SqliteIssueRepository::new(pool.clone());

    repo.migrate().await.expect("首次迁移失败");
    let first = turn_store_snapshot(&pool, "legacy-sess-1").await;

    // 连续第二次 migrate：轮次行数 / turns_count / turn_status / 键序必须完全不变
    repo.migrate().await.expect("二次迁移失败");
    let second = turn_store_snapshot(&pool, "legacy-sess-1").await;
    assert_eq!(first, second, "[L3A-F6] 二次 migrate 必须不改变轮次存储");
    assert_eq!(first.0, 2, "回填应为 2 行");
    assert_eq!(first.1, 2, "turns_count 必须为 2");
    assert_eq!(first.2, "failed", "turn_status 取末轮 status");

    // 强制重放回填的 INSERT 路径：删掉一行 + turns_count 归零 → migrate 应恢复同一键与 seq
    sqlx::query("DELETE FROM trace_turns WHERE session_id = 'legacy-sess-1' AND turn_id = 'lt-2'")
        .execute(&pool)
        .await
        .expect("删除轮次失败");
    sqlx::query("UPDATE traces SET turns_count = 0 WHERE session_id = 'legacy-sess-1'")
        .execute(&pool)
        .await
        .expect("重置 turns_count 失败");
    repo.migrate().await.expect("回填重放失败");
    let third = turn_store_snapshot(&pool, "legacy-sess-1").await;
    assert_eq!(
        first, third,
        "[L3A-F6] 回填插入路径重放后必须回到与首次完全一致的状态（键/seq/聚合列）"
    );
}

/// 轮次存储快照：(行数, turns_count, turn_status, [(turn_id, seq)…按 seq 排序])。
async fn turn_store_snapshot(
    pool: &sqlx::SqlitePool,
    session_id: &str,
) -> (i64, i64, String, Vec<(String, i64)>) {
    let rows = sqlx::query(
        "SELECT turn_id, seq FROM trace_turns WHERE session_id = ? ORDER BY seq",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .expect("读取轮次失败");
    let keys: Vec<(String, i64)> = rows
        .iter()
        .map(|row| {
            (
                sqlx::Row::get::<String, _>(row, "turn_id"),
                sqlx::Row::get::<i64, _>(row, "seq"),
            )
        })
        .collect();
    let trace = sqlx::query("SELECT turns_count, turn_status FROM traces WHERE session_id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("读取聚合行失败");
    (
        keys.len() as i64,
        sqlx::Row::get::<i64, _>(&trace, "turns_count"),
        sqlx::Row::get::<String, _>(&trace, "turn_status"),
        keys,
    )
}

// ── amend v1.4-3/v1.4-8（L3A-F4）：retention 与 ingest 交错后聚合仍自洽 ─────────

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn v2_sqlite_retention_and_ingest_interleaving_stays_consistent() {
    let (repo, _db, pool) = sqlite_repo("interleave").await;
    let sessions: Vec<String> = (0..6).map(|index| format!("v2-race-{index}")).collect();
    for session in &sessions {
        repo.record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "t0", "status": "succeeded"}]}),
        ))
        .await
        .expect("预置失败");
    }
    let old = (Utc::now() - ChronoDuration::days(30)).to_rfc3339();
    sqlx::query("UPDATE traces SET created_at = ?, updated_at = ?")
        .bind(&old)
        .bind(&old)
        .execute(&pool)
        .await
        .expect("回拨时间失败");

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));
    let mut handles = Vec::new();
    for worker in 0..2u32 {
        let repo = repo.clone();
        let sessions = sessions.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for round in 0..6u32 {
                for session in &sessions {
                    repo.record_trace(trace(
                        session,
                        json!({"turns": [{"turn_id": format!("w{worker}-r{round}"), "status": "succeeded"}]}),
                    ))
                    .await
                    .expect("并发 ingest 失败");
                }
            }
        }));
    }
    for _ in 0..2 {
        let repo = repo.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..12 {
                repo.delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 2)
                    .await
                    .expect("并发 retention 失败");
            }
        }));
    }
    barrier.wait().await;
    for handle in handles {
        handle.await.expect("并发任务 panic");
    }

    let inconsistent: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM traces AS t
         WHERE t.turns_count <> (SELECT COUNT(*) FROM trace_turns AS tt
                                 WHERE tt.session_id = t.session_id)
            OR t.turns_count <> json_array_length(t.payload, '$.turns')",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
    .expect("一致性统计失败");
    assert_eq!(
        inconsistent, 0,
        "[L3A-F4] ingest/retention 交错后 turns_count 必须等于 trace_turns 行数且等于 payload.turns 长度"
    );
    let orphans: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM trace_turns AS tt
         WHERE NOT EXISTS (SELECT 1 FROM traces AS t WHERE t.session_id = tt.session_id)",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
    .expect("孤儿统计失败");
    assert_eq!(orphans, 0, "[L3A-F4] 不得留下孤儿轮次");
}

// ── PG（PONY_TEST_PG_URL 门控；未设置显式 SKIP）────────────────────────────────

mod pg {
    use super::*;
    use pony_sentry_core::PgIssueRepository;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    /// 独立空 schema 的连接池（未设置 `PONY_TEST_PG_URL` 时返回 None 并打印 SKIP）。
    pub async fn isolated_pool(schema: &str) -> Option<sqlx::PgPool> {
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
                .max_connections(8)
                .connect_with(base.options([("search_path", schema.to_string())]))
                .await
                .expect("连接隔离 schema 失败"),
        )
    }

    /// 独立 schema 的 PG 仓储（已 migrate）。
    pub async fn isolated(schema: &str) -> Option<(PgIssueRepository, sqlx::PgPool)> {
        let pool = isolated_pool(schema).await?;
        let repo = PgIssueRepository::new(pool.clone());
        repo.migrate().await.expect("PG 迁移失败");
        Some((repo, pool))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn v2_pg_terminal_merge_and_concurrency() {
        let Some((repo, _pool)) = isolated("ponysentry_v2_merge").await else {
            return;
        };
        let session = "v2-pg-merge";

        repo.record_trace(trace(
            session,
            json!({"turns": [{"turn_id": "t1", "status": "running"}]}),
        ))
        .await
        .expect("PG 首报失败");
        let saved = repo
            .record_trace(trace(
                session,
                json!({"turns": [{"turn_id": "t1", "status": "failed",
                    "error": {"message": "boom"},
                    "tool_calls": [{"tool_call_id": "c1", "status": "error", "error": {"message": "x"}}]}]}),
            ))
            .await
            .expect("PG 二报失败");
        assert_eq!(saved.turn_status, "failed", "[C2.1] PG 终态覆盖");
        assert_eq!(only_turn(&saved, "t1")["error"]["message"], "boom");

        let concurrent_session = "v2-pg-concurrent";
        let mut handles = Vec::new();
        for worker in 0..20u32 {
            let repo = repo.clone();
            handles.push(tokio::spawn(async move {
                for slot in 0..2u32 {
                    repo.record_trace(trace(
                        concurrent_session,
                        json!({"turns": [{"turn_id": format!("r{worker}-t{slot}"), "status": "succeeded"}]}),
                    ))
                    .await
                    .unwrap_or_else(|error| panic!("PG 并发上报 {worker}/{slot} 失败：{error}"));
                }
            }));
        }
        for handle in handles {
            handle.await.expect("PG 并发任务 panic");
        }
        let record = repo
            .list_traces(session_filter(concurrent_session))
            .await
            .expect("PG 读取失败")
            .remove(0);
        assert_eq!(record.turns_count, 40, "[C3] PG advisory lock 下 40 个 turn 必须全部入库");
        assert_eq!(record.payload["turns"].as_array().map(Vec::len), Some(40));
    }

    #[tokio::test]
    async fn v2_pg_backfill_generates_runtime_keys_for_missing_turn_ids() {
        let Some((repo, pool)) = isolated("ponysentry_v2_backfill_keys").await else {
            return;
        };
        // 两条没有 turn_id 的 legacy turn（内容不同）——回填必须生成与运行时同构的 turn_srv_* 键
        sqlx::query(
            "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project,
                 eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms,
                 reported_at, created_at, updated_at)
             VALUES ('pg-noid-1', 'pg-noid-sess', NULL, NULL, 'development', 'dev',
                 'job_copilot', 'unreviewed', $1::jsonb, 0, 0, 0, now(), now(), now())",
        )
        .bind(
            json!({"turns": [
                {"status": "succeeded", "input_tokens": 3},
                {"status": "failed", "input_tokens": 4}
            ]})
            .to_string(),
        )
        .execute(&pool)
        .await
        .expect("插入 PG 无 turn_id 行失败");

        repo.migrate().await.expect("PG 回填失败");

        let keys: Vec<String> = sqlx::query(
            "SELECT turn_id FROM trace_turns WHERE session_id = 'pg-noid-sess' ORDER BY seq",
        )
        .fetch_all(&pool)
        .await
        .expect("读取 PG 回填行失败")
        .iter()
        .map(|row| sqlx::Row::get::<String, _>(row, "turn_id"))
        .collect();
        assert_eq!(keys.len(), 2, "[L3A-F1] PG 回填两条缺 turn_id 的 turn 必须两行");
        assert_ne!(keys[0], keys[1], "[L3A-F1] PG 回填不得塌成一行");
        assert!(
            keys.iter().all(|key| key.starts_with("turn_srv_")),
            "PG 回填键必须是 turn_srv_*（运行时同构）：{keys:?}"
        );
        let record = repo
            .list_traces(session_filter("pg-noid-sess"))
            .await
            .expect("PG 读取失败")
            .remove(0);
        assert_eq!(record.turns_count, 2);
        assert_eq!(record.turn_status, "failed");
    }

    #[tokio::test]
    async fn v2_pg_retention_never_deletes_turns_of_surviving_sessions() {
        let Some((repo, pool)) = isolated("ponysentry_v2_retention").await else {
            return;
        };
        for session in ["pg-ret-a", "pg-ret-b"] {
            for turn_id in ["t1", "t2"] {
                repo.record_trace(trace(
                    session,
                    json!({"turns": [{"turn_id": turn_id, "status": "succeeded"}]}),
                ))
                .await
                .expect("PG 写入失败");
            }
        }
        sqlx::query("UPDATE traces SET created_at = now() - interval '10 days', updated_at = now() - interval '10 days'")
            .execute(&pool)
            .await
            .expect("PG 回拨时间失败");

        assert_eq!(
            repo.delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 1)
                .await
                .expect("PG retention 失败"),
            1,
            "[C6] 每批只删 1 行"
        );
        let survived: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM trace_turns
             WHERE session_id IN (SELECT session_id FROM traces)",
        )
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计失败");
        assert_eq!(survived, 2, "[L3A-F4] PG 存活会话的轮次不得被误删");

        assert_eq!(
            repo.delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 10)
                .await
                .expect("PG retention 失败"),
            1
        );
        let leftovers: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns")
            .fetch_one(&pool)
            .await
            .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
            .expect("统计失败");
        assert_eq!(leftovers, 0, "[L3A-F4] PG 删除后不得留孤儿轮次");
    }

    /// L3B-F4：首次 ensure 建列/索引；二次 ensure 0 次 ALTER / 0 次 CREATE INDEX，索引全部有效。
    #[tokio::test]
    async fn v2_pg_schema_ensure_is_idempotent_and_restart_light() {
        let Some(pool) = isolated_pool("ponysentry_v2_schema").await else {
            return;
        };
        // 先建 traces 表（等价于 0003 已应用），再验证 0005 的逐条 DDL 行为
        sqlx::raw_sql(include_str!("../../../migrations/postgres/0003_add_traces.sql"))
            .execute(&pool)
            .await
            .expect("预建 traces 表失败");
        let repo = PgIssueRepository::new(pool.clone());

        let first = repo.ensure_trace_v2_schema().await.expect("首次 schema 落地失败");
        println!("[L3B-F4] 首次 ensure 报告: {first:?}");
        assert_eq!(
            (first.columns_added, first.indexes_created),
            (3, 5),
            "[L3B-F4] 首次应新增 3 列 + 5 索引：{first:?}"
        );

        let indexes: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM pg_indexes
             WHERE schemaname = current_schema()
               AND indexname IN ('idx_trace_turns_session_seq','idx_trace_turns_status',
                                 'idx_traces_run_id','idx_traces_turn_status','idx_traces_updated_at')",
        )
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计索引失败");
        assert_eq!(indexes, 5, "[L3B-F4] 五个索引必须全部存在");

        // 二次 ensure（等价于重启）：不得再执行 ALTER / CREATE INDEX
        let second = repo.ensure_trace_v2_schema().await.expect("二次 schema 落地失败");
        println!("[L3B-F4] 二次 ensure 报告（重启等价）: {second:?}");
        assert_eq!(
            (second.columns_added, second.indexes_created),
            (0, 0),
            "[L3B-F4] 二次启动不得 ALTER / CREATE INDEX：{second:?}"
        );
        assert_eq!(second.columns_skipped, 3);
        assert_eq!(second.indexes_skipped, 5);

        // 完整 migrate 再跑一次也必须 0/0（migrate 内部调用 ensure）
        repo.migrate().await.expect("二次 migrate 失败");

        // 索引均有效（CONCURRENTLY 失败会留下 indisvalid=false 的残骸）
        let invalid: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM pg_index AS i
             JOIN pg_class AS c ON c.oid = i.indexrelid
             JOIN pg_namespace AS ns ON ns.oid = c.relnamespace
             WHERE ns.nspname = current_schema() AND NOT i.indisvalid",
        )
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("统计无效索引失败");
        println!("[L3B-F4] schema 内索引数={indexes}，invalid 索引数={invalid}");
        assert_eq!(invalid, 0, "[L3B-F4] 不得留下 invalid 索引");
    }

    /// L3A-F4：PG 上 ingest 与 retention 并发交错后，聚合列与轮次表必须自洽。
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn v2_pg_retention_and_ingest_interleaving_stays_consistent() {
        let Some((repo, pool)) = isolated("ponysentry_v2_interleave").await else {
            return;
        };
        let sessions: Vec<String> = (0..6).map(|index| format!("pg-race-{index}")).collect();
        for session in &sessions {
            repo.record_trace(trace(
                session,
                json!({"turns": [{"turn_id": "t0", "status": "succeeded"}]}),
            ))
            .await
            .expect("PG 预置失败");
        }
        sqlx::query(
            "UPDATE traces SET created_at = now() - interval '30 days',
                 updated_at = now() - interval '30 days'",
        )
        .execute(&pool)
        .await
        .expect("PG 回拨时间失败");

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));
        let mut handles = Vec::new();
        for worker in 0..2u32 {
            let repo = repo.clone();
            let sessions = sessions.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for round in 0..6u32 {
                    for session in &sessions {
                        repo.record_trace(trace(
                            session,
                            json!({"turns": [{"turn_id": format!("w{worker}-r{round}"), "status": "succeeded"}]}),
                        ))
                        .await
                        .expect("PG 并发 ingest 失败");
                    }
                }
            }));
        }
        for _ in 0..2 {
            let repo = repo.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for _ in 0..12 {
                    repo.delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 2)
                        .await
                        .expect("PG 并发 retention 失败");
                }
            }));
        }
        barrier.wait().await;
        for handle in handles {
            handle.await.expect("PG 并发任务 panic");
        }

        let inconsistent: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM traces AS t
             WHERE t.turns_count <> (SELECT COUNT(*) FROM trace_turns AS tt
                                     WHERE tt.session_id = t.session_id)
                OR t.turns_count <> jsonb_array_length(t.payload -> 'turns')",
        )
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("PG 一致性统计失败");
        let remaining: i64 = sqlx::query("SELECT COUNT(*) AS n FROM traces")
            .fetch_one(&pool)
            .await
            .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
            .expect("统计失败");
        let turns: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns")
            .fetch_one(&pool)
            .await
            .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
            .expect("统计失败");
        println!(
            "[L3A-F4] PG 交错后: 聚合行={remaining} 轮次行={turns} 不一致行={inconsistent}"
        );
        assert_eq!(
            inconsistent, 0,
            "[L3A-F4] PG ingest/retention 交错后 turns_count 必须等于 trace_turns 行数与 payload.turns 长度"
        );
        let orphans: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM trace_turns AS tt
             WHERE NOT EXISTS (SELECT 1 FROM traces AS t WHERE t.session_id = tt.session_id)",
        )
        .fetch_one(&pool)
        .await
        .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
        .expect("PG 孤儿统计失败");
        assert_eq!(orphans, 0, "[L3A-F4] PG 不得留下孤儿轮次");
    }

    #[tokio::test]
    async fn v2_pg_migration_backfills_legacy_payload() {
        let Some((repo, pool)) = isolated("ponysentry_v2_backfill").await else {
            return;
        };
        // 模拟 0005 之前写入的旧行（不含聚合列值），再重跑迁移触发回填
        sqlx::query(
            "INSERT INTO traces (id, session_id, run_id, turn_id, environment, release, project,
                 eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms,
                 reported_at, created_at, updated_at)
             VALUES ('pg-legacy-1', 'pg-legacy-sess', 'run-legacy', NULL, 'development', 'dev',
                 'job_copilot', 'unreviewed', $1::jsonb, 12, 5, 24, now(), now(), now())",
        )
        .bind(json!({"turns": [
            {"turn_id": "lt-1", "status": "succeeded", "input_tokens": 7},
            {"turn_id": "lt-2", "status": "failed", "input_tokens": 5}
        ]})
        .to_string())
        .execute(&pool)
        .await
        .expect("插入 PG 旧形态行失败");

        repo.migrate().await.expect("PG 重复迁移失败");

        let record = repo
            .list_traces(session_filter("pg-legacy-sess"))
            .await
            .expect("PG 读取失败")
            .remove(0);
        assert_eq!(record.turns_count, 2, "[C1.3] PG 回填后 turns_count 必须为 2");
        assert_eq!(record.turn_status, "failed", "[C1.3] PG 回填 turn_status 取末轮 status");
        assert_eq!(
            record.payload["turns"].as_array().map(Vec::len),
            Some(2),
            "[C1.3] PG 回填后 payload.turns 必须含 2 条"
        );

        // retention：回填行被删除后 trace_turns 不得留孤儿
        sqlx::query("UPDATE traces SET created_at = now() - interval '10 days', updated_at = now() - interval '10 days' WHERE session_id = 'pg-legacy-sess'")
            .execute(&pool)
            .await
            .expect("回拨时间失败");
        let deleted = repo
            .delete_traces_older_than(Utc::now() - ChronoDuration::days(1), 100)
            .await
            .expect("PG retention 失败");
        assert_eq!(deleted, 1, "[C6] PG 应删除 1 行过期聚合行");
        let orphans: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trace_turns WHERE session_id = 'pg-legacy-sess'")
            .fetch_one(&pool)
            .await
            .map(|row| sqlx::Row::get::<i64, _>(&row, "n"))
            .expect("统计 PG 孤儿失败");
        assert_eq!(orphans, 0, "[C6] PG retention 必须同时清理 trace_turns");
    }
}
