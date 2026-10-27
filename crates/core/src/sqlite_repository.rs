use crate::{
    db::DatabasePool,
    models::{
        EvalStatus, Event, EventStatsCounts, IngestGovernanceProjectRow, IngestGovernanceQuery,
        IngestGovernanceSeriesPoint, IngestGovernanceStats, IngestGovernanceTotals,
        IngestMetricOutcome, Issue, IssueArchiveCriteria, IssueArchiveOutcome, IssueArchiveRequest,
        IssueEventsCursor, IssueEventsOrder, IssueStatsCounts, IssueStatsQuery, IssueStatsResponse,
        IssueStatus, IssueStatusEnvironmentCount, StatsBucket, StatsRange, StatsSeriesPoint,
        TraceFilter, TraceRecord,
    },
    reindex::{plan_reindex, FingerprintOut, ReindexSummary},
    repository::{
        IssueFilter, IssueGovernanceRepository, IssueRepository, NewEventRecord, RepositoryError,
        TraceRepository,
    },
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use sqlx::Row;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

/// issues 表全量列（对齐 `migrations/0001_init.sql` + `0002` + `0004` + `0007`），
/// 集中一处避免 SELECT/INSERT 列清单漂移。
const ISSUE_COLUMNS: &str =
    "id, fingerprint, title, culprit, platform, status, assigned_to, count, \
     last_release, project, environment, app, namespace, component, level, source_kind, \
     first_seen_at, last_seen_at, archived_at, archive_restored_at";

/// events 表全量列（含 0004 新增 event_key / level）。
const EVENT_COLUMNS: &str =
    "id, issue_id, payload, release, environment, event_key, level, created_at";

#[derive(Clone)]
pub struct SqliteIssueRepository {
    pool: DatabasePool,
}

impl SqliteIssueRepository {
    pub fn new(pool: DatabasePool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        let migration_sql = include_str!("../../../migrations/0001_init.sql");
        sqlx::raw_sql(migration_sql).execute(&self.pool).await?;
        // SQLite 的 ALTER TABLE ADD COLUMN 没有 IF NOT EXISTS 语法，
        // 直接重放会在第二次启动时报 "duplicate column name"；先查列，缺了才补。
        let has_project: bool = sqlx::query("SELECT project FROM issues LIMIT 0")
            .fetch_all(&self.pool)
            .await
            .is_ok();
        if !has_project {
            sqlx::raw_sql(include_str!("../../../migrations/0002_add_project.sql"))
                .execute(&self.pool)
                .await?;
        }
        let traces_sql = include_str!("../../../migrations/0003_add_traces.sql");
        // 存量库可能已建过 traces 表（缺 project 列）：0003 的 CREATE TABLE IF NOT EXISTS
        // 是 no-op，但其内的 idx_traces_project 依赖 project 列，缺列时建索引会报错，
        // 因此先判断表是否存在：全新库整文件执行；存量库先补列（幂等）再建索引。
        let traces_table_exists: Option<String> =
            sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'traces'")
                .fetch_optional(&self.pool)
                .await?
                .map(|row| row.get("name"));

        match traces_table_exists {
            None => {
                sqlx::raw_sql(traces_sql).execute(&self.pool).await?;
            }
            Some(_) => {
                let has_trace_project: bool = sqlx::query("SELECT project FROM traces LIMIT 0")
                    .fetch_all(&self.pool)
                    .await
                    .is_ok();
                if !has_trace_project {
                    sqlx::raw_sql("ALTER TABLE traces ADD COLUMN project TEXT;")
                        .execute(&self.pool)
                        .await?;
                }
                sqlx::raw_sql("CREATE INDEX IF NOT EXISTS idx_traces_project ON traces(project);")
                    .execute(&self.pool)
                    .await?;
            }
        }

        // 0004：多环境维度 + 事件幂等键。
        // SQLite 的 ADD COLUMN 无 IF NOT EXISTS，故按列探测决定是否执行整份迁移
        // （契约 R5.1a；索引与回填语句自身幂等）。整批在单事务内执行，
        // 任一句失败整体回滚，不留"半套列"的不可重跑状态（契约 R5.2）。
        let has_issue_dimensions: bool = sqlx::query("SELECT environment FROM issues LIMIT 0")
            .fetch_all(&self.pool)
            .await
            .is_ok();
        let has_event_dimensions: bool = sqlx::query("SELECT event_key FROM events LIMIT 0")
            .fetch_all(&self.pool)
            .await
            .is_ok();
        if !has_issue_dimensions || !has_event_dimensions {
            let mut tx = self.pool.begin().await?;
            sqlx::raw_sql(include_str!(
                "../../../migrations/0004_add_issue_dimensions.sql"
            ))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }

        // 0005（C1.1–C1.3）：`trace_turns` 轮次表 + `traces` 聚合列 + 存量回填。
        // SQLite 无 `ADD COLUMN IF NOT EXISTS`，按列探测守卫整份迁移（范式同 0004）；
        // 整批在单事务内执行，任一句失败整体回滚，故不会留下"半套列"的不可重跑状态。
        let has_trace_aggregates: bool = sqlx::query("SELECT turns_count FROM traces LIMIT 0")
            .fetch_all(&self.pool)
            .await
            .is_ok();
        if !has_trace_aggregates {
            let mut tx = self.pool.begin().await?;
            sqlx::raw_sql(include_str!("../../../migrations/0005_trace_turns.sql"))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }

        // 0006（W-04 / amend v1.5-6）：`trace_turns.run_id` + 索引。
        // SQLite 无 ADD COLUMN IF NOT EXISTS：按列探测守卫整份执行（范式同 0002/0004/0005）。
        // `amend v1.6-5`（L3A-E6）：列与索引**分别探测创建**，部分迁移可自愈。
        let has_turn_run_id: bool = sqlx::query("SELECT run_id FROM trace_turns LIMIT 0")
            .fetch_all(&self.pool)
            .await
            .is_ok();
        if !has_turn_run_id {
            sqlx::query("ALTER TABLE trace_turns ADD COLUMN run_id TEXT;")
                .execute(&self.pool)
                .await?;
        }
        let has_run_id_index: bool = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_trace_turns_run_id'",
        )
        .fetch_optional(&self.pool)
        .await?
        .is_some();
        if !has_run_id_index {
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_trace_turns_run_id ON trace_turns(run_id);",
            )
            .execute(&self.pool)
            .await?;
        }
        // `amend v1.6-5`（L3A-E8）：存量轮次 run_id 按 `traces.run_id` 回填一次（幂等）。
        sqlx::query(
            "UPDATE trace_turns SET run_id = (
                 SELECT run_id FROM traces WHERE traces.session_id = trace_turns.session_id)
             WHERE run_id IS NULL",
        )
        .execute(&self.pool)
        .await?;

        // C1.3 / amend v1.4-1：回填在 Rust 侧执行——turn_id 必须与运行时
        // `trace_v2::turn_key_of`（sha256）同构，SQL 方言无 sha256 故不写在迁移文件里。
        self.backfill_trace_turns().await?;

        // 0007（契约 C1.2 / C1.4）：issue 归档列 + 治理读路径索引 + `ingest_metrics`。
        // SQLite 无 `ADD COLUMN IF NOT EXISTS`，故：
        //  - 两列**分别探测、分别 ALTER**（`SELECT <col> FROM issues LIMIT 0` 失败才执行），
        //    半迁移库（只有一列）可自愈，范式同 0006；
        //  - `CREATE TABLE/INDEX IF NOT EXISTS` 自身幂等，每次启动都执行（0005/0006 同）。
        // 语句来源统一取自迁移文件本身，避免「文件 vs 代码」双份真相漂移。
        for statement in split_sql_statements(include_str!(
            "../../../migrations/0007_issue_archive.sql"
        )) {
            if statement.to_ascii_uppercase().starts_with("ALTER TABLE") {
                match add_column_target(&statement) {
                    Some((table, column)) => {
                        let probe = format!("SELECT {column} FROM {table} LIMIT 0");
                        if sqlx::query(&probe).fetch_all(&self.pool).await.is_err() {
                            sqlx::query(&statement).execute(&self.pool).await?;
                        }
                    }
                    // 解析失败不静默吞：按原语句执行，让 DB 报错暴露契约漂移。
                    None => {
                        sqlx::query(&statement).execute(&self.pool).await?;
                    }
                }
                continue;
            }
            sqlx::query(&statement).execute(&self.pool).await?;
        }

        Ok(())
    }

    /// 存量回填（幂等）：仅扫描「`trace_turns` 行数 < `payload.turns` 条数」的聚合行。
    ///
    /// 正常情况下启动扫描为 0 行；对命中的行按 turn_id 补齐缺失轮次并刷新聚合列。
    async fn backfill_trace_turns(&self) -> Result<(), sqlx::Error> {
        let rows = sqlx::query(
            "SELECT session_id, payload FROM traces
             WHERE json_type(payload, '$.turns') = 'array'
               AND json_array_length(payload, '$.turns') > turns_count",
        )
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let session_id: String = row.get("session_id");
            let payload_str: String = row.get("payload");
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);
            let mut tx = self.pool.begin().await?;
            hydrate_trace_turns(&mut tx, &session_id, &payload).await?;
            refresh_trace_turn_aggregates(&mut tx, &session_id).await?;
            tx.commit().await?;
        }
        Ok(())
    }

    async fn issue_by_id(&self, id: &str) -> Result<Issue, RepositoryError> {
        let row = sqlx::query(&format!("SELECT {ISSUE_COLUMNS} FROM issues WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| RepositoryError::NotFound(id.to_string()))?;
        Ok(issue_from_row(&row))
    }

    /// 幂等命中路径：按 event_key 取既有事件与其归属 issue（不触碰计数与时间戳）。
    async fn duplicate_hit(&self, event_key: &str) -> Result<Option<UpsertDuplicate>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE event_key = ?"
        ))
        .bind(event_key)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let event = event_from_row(&row);
        let issue_id = event.issue_id.clone();
        Ok(Some(UpsertDuplicate { issue_id, event }))
    }
}

/// 幂等命中时待回填的既有行（issue 由调用方按 id 查询）。
struct UpsertDuplicate {
    issue_id: String,
    event: Event,
}

/// 仅当错误是 SQLite 的 `SQLITE_BUSY`（code 5 / "database is locked"）且仍有重试预算时，
/// 返回本次退避毫秒数（10 / 20 / 40ms，最多 3 次重试）。**其它错误一律不重试**。
fn sqlite_busy_retry_delay(error: &RepositoryError, attempt: usize) -> Option<u64> {
    const DELAYS_MS: [u64; 3] = [10, 20, 40];
    let delay = *DELAYS_MS.get(attempt)?;
    let RepositoryError::Database(sqlx::Error::Database(db_error)) = error else {
        return None;
    };
    let message = db_error.message().to_ascii_lowercase();
    // 仅 `SQLITE_BUSY`（primary code 5；扩展码 517 = BUSY_SNAPSHOT 亦归此主码）。
    // `SQLITE_LOCKED (6)` 等其它码不重试，避免掩盖真实缺陷。
    let busy_code = db_error.code().map(|code| code == "5").unwrap_or(false);
    if busy_code || message.contains("database is locked") || message.contains("database is busy") {
        Some(delay)
    } else {
        None
    }
}

/// `record_event_and_upsert_issue` 的**单次尝试**（不含重试）：一个 `BEGIN IMMEDIATE` 写事务内
/// 完成幂等预查 → issue upsert → events 插入 → （按需）活跃即解档 → 提交。
async fn record_event_and_upsert_issue_once(
    repo: &SqliteIssueRepository,
    record: &NewEventRecord<'_>,
) -> Result<crate::repository::UpsertIssueResult, RepositoryError> {
    let now = Utc::now();
    let occurred_at = record.occurred_at.unwrap_or(now);
    let environment = normalize_environment(record.environment);

    // 1. 幂等前置检查：同 event_key 已存在 → 直接返回既有 issue/event，
    //    不新增 events 行、不递增 count、不推进 last_seen_at（契约 R4.1）。
    if let Some(key) = record.event_key {
        if let Some(hit) = repo.duplicate_hit(key).await? {
            let issue = repo.issue_by_id(&hit.issue_id).await?;
            return Ok(duplicate_result(issue, hit.event));
        }
    }

    // **BEGIN IMMEDIATE**（不是 `pool.begin()` 的 deferred 语义）：一开始就取写锁，
    // 多写者按 busy_timeout 排队；deferred 事务会「先 SHARED 再升级写锁」，并发升级即
    // `SQLITE_BUSY: database is locked`（且 busy handler 不重试升级死锁）→ 整请求 500。
    // 先例：本文件 trace 路径（`record_trace` / `delete_traces_older_than`）。
    let mut tx = repo.pool.begin_with("BEGIN IMMEDIATE").await?;

    let existing = sqlx::query(&format!(
        "SELECT {ISSUE_COLUMNS} FROM issues WHERE fingerprint = ? AND environment = ?"
    ))
    .bind(record.fingerprint)
    .bind(&environment)
    .fetch_optional(&mut *tx)
    .await?;

    let mut is_new = false;
    let mut is_regression_trigger = false;

    let issue = match existing {
        Some(row) => {
            let stored = issue_from_row(&row);
            let mut status = stored.status;
            // 核心状态机：仅当首次处于 Resolved 且新事件再次出现时，才触发一次 Regression 状态跃迁
            if status == IssueStatus::Resolved {
                status = IssueStatus::Regression;
                is_regression_trigger = true;
            }
            let new_count = stored.count + 1;
            // 乱序事件（补投历史 timestamp）不得把 last_seen_at 拉回过去
            let last_seen_at = if occurred_at > stored.last_seen_at {
                occurred_at
            } else {
                stored.last_seen_at
            };
            sqlx::query(
                "UPDATE issues SET count = ?, status = ?, last_release = ?, last_seen_at = ? WHERE id = ?"
            )
            .bind(new_count)
            .bind(status.to_string())
            .bind(record.release)
            .bind(last_seen_at.to_rfc3339())
            .bind(&stored.id)
            .execute(&mut *tx)
            .await?;

            // 维度以首报为准（与项目名同策略）：后续上报若缺维度，不把已有值擦成 NULL。
            let app = stored.app.or_else(|| record.app.map(str::to_string));
            let namespace = stored
                .namespace
                .or_else(|| record.namespace.map(str::to_string));
            let component = stored
                .component
                .or_else(|| record.component.map(str::to_string));
            sqlx::query(
                "UPDATE issues SET app = ?, namespace = ?, component = ?, level = ?, source_kind = ? WHERE id = ?",
            )
            .bind(&app)
            .bind(&namespace)
            .bind(&component)
            .bind(record.level)
            .bind(record.source_kind)
            .bind(&stored.id)
            .execute(&mut *tx)
            .await?;

            // C2.5（`amend v1.1 / A-2`）：活跃即解档——**同一事务内**追加一条 UPDATE。
            // 零变更硬约束（C7.1）：不动 count/status/响应字段/事件语义，只清归档标记。
            let mut archived_at = stored.archived_at;
            let mut archive_restored_at = stored.archive_restored_at;
            if archived_at.is_some() && crate::repository::is_active_issue_status(&status) {
                sqlx::query(
                    "UPDATE issues SET archived_at = NULL, archive_restored_at = NULL WHERE id = ?",
                )
                .bind(&stored.id)
                .execute(&mut *tx)
                .await?;
                archived_at = None;
                archive_restored_at = None;
            }

            Issue {
                id: stored.id,
                fingerprint: record.fingerprint.to_string(),
                title: record.title.to_string(),
                culprit: record.culprit.map(str::to_string),
                platform: record.platform.to_string(),
                status,
                assigned_to: stored.assigned_to,
                count: new_count,
                last_release: record.release.map(str::to_string),
                project: stored
                    .project
                    .or_else(|| record.project.map(str::to_string)),
                environment: environment.clone(),
                app,
                namespace,
                component,
                level: record.level.to_string(),
                source_kind: record.source_kind.map(str::to_string),
                first_seen_at: stored.first_seen_at,
                last_seen_at,
                archived_at,
                archive_restored_at,
                // 读侧派生字段（最近一条事件 payload 的 trace_id），仓储层不赋值。
                trace_id: None,
            }
        }
        None => {
            is_new = true;
            let id = Uuid::new_v4().to_string();
            let status = IssueStatus::Unresolved;
            sqlx::query(
                "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, count, \
                 last_release, project, environment, app, namespace, component, level, source_kind, \
                 first_seen_at, last_seen_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(record.fingerprint)
            .bind(record.title)
            .bind(record.culprit)
            .bind(record.platform)
            .bind(status.to_string())
            .bind(1i64)
            .bind(record.release)
            .bind(record.project)
            .bind(&environment)
            .bind(record.app)
            .bind(record.namespace)
            .bind(record.component)
            .bind(record.level)
            .bind(record.source_kind)
            .bind(occurred_at.to_rfc3339())
            .bind(occurred_at.to_rfc3339())
            .execute(&mut *tx)
            .await?;

            Issue {
                id,
                fingerprint: record.fingerprint.to_string(),
                title: record.title.to_string(),
                culprit: record.culprit.map(str::to_string),
                platform: record.platform.to_string(),
                status,
                assigned_to: None,
                count: 1,
                last_release: record.release.map(str::to_string),
                project: record.project.map(str::to_string),
                environment: environment.clone(),
                app: record.app.map(str::to_string),
                namespace: record.namespace.map(str::to_string),
                component: record.component.map(str::to_string),
                level: record.level.to_string(),
                source_kind: record.source_kind.map(str::to_string),
                first_seen_at: occurred_at,
                last_seen_at: occurred_at,
                // 新建 issue 必然未归档（0007 默认 NULL）。
                archived_at: None,
                archive_restored_at: None,
                // 读侧派生字段（最近一条事件 payload 的 trace_id），仓储层不赋值。
                trace_id: None,
            }
        }
    };

    let event_id = Uuid::new_v4().to_string();
    let insert_result = sqlx::query(
        "INSERT INTO events (id, issue_id, payload, release, environment, event_key, level, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(&issue.id)
    .bind(serde_json::to_string(&record.payload).unwrap_or_default())
    .bind(record.release)
    .bind(&environment)
    .bind(record.event_key)
    .bind(record.level)
    .bind(occurred_at.to_rfc3339())
    .execute(&mut *tx)
    .await;

    if let Err(err) = insert_result {
        // 并发同 event_key：唯一索引兜底，归一为幂等命中（契约 R4.2，不得 500）。
        if is_unique_violation(&err) {
            tx.rollback().await?;
            if let Some(key) = record.event_key {
                if let Some(hit) = repo.duplicate_hit(key).await? {
                    let issue = repo.issue_by_id(&hit.issue_id).await?;
                    return Ok(duplicate_result(issue, hit.event));
                }
            }
        }
        return Err(err.into());
    }

    tx.commit().await?;

    let event = Event {
        id: event_id,
        issue_id: issue.id.clone(),
        payload: record.payload.clone(),
        release: record.release.map(str::to_string),
        environment: Some(environment),
        event_key: record.event_key.map(str::to_string),
        level: Some(record.level.to_string()),
        created_at: occurred_at,
    };

    Ok(crate::repository::UpsertIssueResult {
        issue,
        event,
        is_new,
        is_regression_trigger,
        is_duplicate: false,
    })
}

fn issue_from_row(row: &sqlx::sqlite::SqliteRow) -> Issue {
    let status_str: String = row.get("status");
    let first_seen_str: String = row.get("first_seen_at");
    let last_seen_str: String = row.get("last_seen_at");
    Issue {
        id: row.get("id"),
        fingerprint: row.get("fingerprint"),
        title: row.get("title"),
        culprit: row.get("culprit"),
        platform: row.get("platform"),
        status: status_str.parse().unwrap_or(IssueStatus::Unresolved),
        assigned_to: row.get("assigned_to"),
        count: row.get("count"),
        last_release: row.get("last_release"),
        project: row.get("project"),
        environment: row.get("environment"),
        app: row.get("app"),
        namespace: row.get("namespace"),
        component: row.get("component"),
        level: row.get("level"),
        source_kind: row.get("source_kind"),
        first_seen_at: chrono::DateTime::parse_from_rfc3339(&first_seen_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        last_seen_at: chrono::DateTime::parse_from_rfc3339(&last_seen_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        // 0007：归档列为 ISO8601 UTC 文本；解析失败按 `None` 并 `warn!`（**不得 panic**）。
        archived_at: optional_timestamp(row, "archived_at"),
        archive_restored_at: optional_timestamp(row, "archive_restored_at"),
        // 读侧派生字段：issues 表无此列，由 handler 读取时从最近一条事件 payload 取。
        trace_id: None,
    }
}

/// 读取可空时间列（0007 归档列）：非 NULL 但解析失败 → `None` + `warn!`，不 panic。
fn optional_timestamp(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<DateTime<Utc>> {
    let raw: Option<String> = row.get(column);
    let raw = raw?;
    match DateTime::parse_from_rfc3339(&raw) {
        Ok(value) => Some(value.with_timezone(&Utc)),
        Err(error) => {
            tracing::warn!(
                column,
                raw = %raw,
                error = %error,
                "issues 时间列不是合法 RFC3339，按 None 处理"
            );
            None
        }
    }
}

fn event_from_row(row: &sqlx::sqlite::SqliteRow) -> Event {
    let created_str: String = row.get("created_at");
    let payload_str: String = row.get("payload");
    let payload: serde_json::Value = serde_json::from_str(&payload_str).unwrap_or(json!({}));
    Event {
        id: row.get("id"),
        issue_id: row.get("issue_id"),
        payload,
        release: row.get("release"),
        environment: row.get("environment"),
        event_key: row.get("event_key"),
        level: row.get("level"),
        created_at: chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}

/// 环境维度归一：空白/缺省一律落 `unknown`（契约 R2 / R5 默认值）。
fn normalize_environment(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// 幂等命中结果的统一构造（重复路径与并发冲突路径共用）。
fn duplicate_result(issue: Issue, event: Event) -> crate::repository::UpsertIssueResult {
    crate::repository::UpsertIssueResult {
        issue,
        event,
        is_new: false,
        is_regression_trigger: false,
        is_duplicate: true,
    }
}

#[async_trait]
impl IssueRepository for SqliteIssueRepository {
    async fn record_event_and_upsert_issue(
        &self,
        record: NewEventRecord<'_>,
    ) -> Result<crate::repository::UpsertIssueResult, RepositoryError> {
        // 语义红线（契约 R4.1 / C7.1）：同 event_id 只产生 1 次真实写入，其余为 duplicate；
        // 重试只作用于「整事务尚未提交」的 SQLITE_BUSY（code 5），不改变响应字段与状态码集合。
        let mut attempt = 0usize;
        loop {
            match record_event_and_upsert_issue_once(self, &record).await {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let Some(delay_ms) = sqlite_busy_retry_delay(&error, attempt) else {
                        return Err(error);
                    };
                    attempt += 1;
                    tracing::warn!(
                        attempt,
                        delay_ms,
                        error = %error,
                        "SQLite 写事务命中 BUSY（database is locked），整事务退避重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }

    async fn get_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        self.issue_by_id(id).await
    }

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, RepositoryError> {
        // 过滤子句与 `count_issues` / stats 聚合共用同一构造器（`C3.3` 要求「同 WHERE」）。
        let clause = crate::repository::issue_filter_clause(&filter, "", |_| "?".to_string());
        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let offset = filter.offset.unwrap_or(0).max(0);
        let sql = format!(
            "SELECT {ISSUE_COLUMNS} FROM issues{} ORDER BY last_seen_at DESC LIMIT ? OFFSET ?",
            clause.clause
        );
        let mut query = sqlx::query(&sql);
        for bind in clause.binds {
            query = query.bind(bind);
        }
        let rows = query
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(issue_from_row).collect())
    }

    async fn update_issue_status(
        &self,
        id: &str,
        status: IssueStatus,
        assigned_to: Option<String>,
    ) -> Result<Issue, RepositoryError> {
        let issue = self.get_issue(id).await?;
        let new_assigned = assigned_to.or(issue.assigned_to);

        sqlx::query("UPDATE issues SET status = ?, assigned_to = ? WHERE id = ?")
            .bind(status.to_string())
            .bind(&new_assigned)
            .bind(id)
            .execute(&self.pool)
            .await?;

        self.get_issue(id).await
    }

    async fn get_events_for_issue(
        &self,
        issue_id: &str,
        limit: i64,
    ) -> Result<Vec<Event>, RepositoryError> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE issue_id = ? ORDER BY created_at DESC LIMIT ?"
        ))
        .bind(issue_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(event_from_row).collect())
    }

    async fn latest_event_payloads(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<(String, serde_json::Value)>, RepositoryError> {
        if issue_ids.is_empty() {
            // 空集直接短路：避免拼出 `IN ()` 这种非法 SQL。
            return Ok(Vec::new());
        }
        // `ROW_NUMBER()` 每 issue 只留最近一行；`id DESC` 仅作同 created_at 的确定性 tie-break。
        let placeholders = vec!["?"; issue_ids.len()].join(", ");
        let sql = format!(
            "SELECT issue_id, payload FROM ( \
                SELECT issue_id, payload, \
                       ROW_NUMBER() OVER (PARTITION BY issue_id ORDER BY created_at DESC, id DESC) AS rn \
                FROM events WHERE issue_id IN ({placeholders}) \
             ) ranked WHERE rn = 1"
        );
        let mut query = sqlx::query(&sql);
        for issue_id in issue_ids {
            query = query.bind(issue_id);
        }
        let rows = query.fetch_all(&self.pool).await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let issue_id: String = row.get("issue_id");
                let payload_str: String = row.get("payload");
                // 载荷损坏只丢该 issue 的派生字段，不影响列表其余行（与 `event_from_row` 同策略）。
                let payload = serde_json::from_str(&payload_str).unwrap_or_else(|_| json!({}));
                (issue_id, payload)
            })
            .collect())
    }

    async fn list_projects(&self) -> Result<Vec<String>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT DISTINCT project FROM issues WHERE project IS NOT NULL ORDER BY project ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| row.get::<String, _>("project"))
            .collect())
    }

    async fn health_check(&self) -> Result<(), RepositoryError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    async fn issue_counts_by_status_environment(
        &self,
    ) -> Result<Vec<IssueStatusEnvironmentCount>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT status, environment, COUNT(*) AS cnt FROM issues GROUP BY status, environment \
             ORDER BY environment ASC, status ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| IssueStatusEnvironmentCount {
                status: row.get("status"),
                environment: row.get("environment"),
                count: row.get("cnt"),
            })
            .collect())
    }

    async fn count_events(&self) -> Result<i64, RepositoryError> {
        let row = sqlx::query("SELECT COUNT(*) AS cnt FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("cnt"))
    }

    async fn delete_events_older_than(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: i64,
    ) -> Result<u64, RepositoryError> {
        // 分批删除：单批最多 batch_size 行，每批独立事务，避免长事务阻塞 ingest 写入。
        let batch_size = batch_size.max(1);
        let sql =
            "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE created_at < ? LIMIT ?)";
        let deleted = sqlx::query(sql)
            .bind(cutoff.to_rfc3339())
            .bind(batch_size)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(deleted)
    }

    async fn reindex_fingerprints(
        &self,
        fingerprint_of: &(dyn for<'a> Fn(&'a serde_json::Value) -> Option<FingerprintOut> + Sync),
    ) -> Result<ReindexSummary, RepositoryError> {
        // 1. 全量读取 events 与 issues
        let event_rows = sqlx::query(&format!("SELECT {EVENT_COLUMNS} FROM events"))
            .fetch_all(&self.pool)
            .await?;
        let events: Vec<Event> = event_rows.iter().map(event_from_row).collect();

        let issue_rows = sqlx::query(&format!("SELECT {ISSUE_COLUMNS} FROM issues"))
            .fetch_all(&self.pool)
            .await?;
        let issues: Vec<Issue> = issue_rows.iter().map(issue_from_row).collect();

        // 2. 规划（纯函数）
        let plan = plan_reindex(&events, &issues, fingerprint_of);

        // 3. 事务持久化
        let mut tx = self.pool.begin().await?;
        for row in &plan.issue_rows {
            if plan.reused_ids.contains(&row.id) {
                sqlx::query(
                    "UPDATE issues SET fingerprint = ?, title = ?, culprit = ?, platform = ?, status = ?, assigned_to = ?, count = ?, last_release = ?, project = ?, environment = ?, app = ?, namespace = ?, component = ?, level = ?, source_kind = ?, first_seen_at = ?, last_seen_at = ? WHERE id = ?",
                )
                .bind(&row.fingerprint)
                .bind(&row.title)
                .bind(&row.culprit)
                .bind(&row.platform)
                .bind(row.status.to_string())
                .bind(&row.assigned_to)
                .bind(row.count)
                .bind(&row.last_release)
                .bind(&row.project)
                .bind(&row.environment)
                .bind(&row.app)
                .bind(&row.namespace)
                .bind(&row.component)
                .bind(&row.level)
                .bind(&row.source_kind)
                .bind(row.first_seen_at.to_rfc3339())
                .bind(row.last_seen_at.to_rfc3339())
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, environment, app, namespace, component, level, source_kind, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&row.id)
                .bind(&row.fingerprint)
                .bind(&row.title)
                .bind(&row.culprit)
                .bind(&row.platform)
                .bind(row.status.to_string())
                .bind(&row.assigned_to)
                .bind(row.count)
                .bind(&row.last_release)
                .bind(&row.project)
                .bind(&row.environment)
                .bind(&row.app)
                .bind(&row.namespace)
                .bind(&row.component)
                .bind(&row.level)
                .bind(&row.source_kind)
                .bind(row.first_seen_at.to_rfc3339())
                .bind(row.last_seen_at.to_rfc3339())
                .execute(&mut *tx)
                .await?;
            }
        }

        for (event_id, issue_id) in &plan.assignments {
            sqlx::query("UPDATE events SET issue_id = ? WHERE id = ?")
                .bind(issue_id)
                .bind(event_id)
                .execute(&mut *tx)
                .await?;
        }

        for id in &plan.delete_ids {
            sqlx::query("DELETE FROM issues WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;

        Ok(ReindexSummary {
            issues_created: plan.issue_rows.len() - plan.reused_ids.len(),
            issues_reused: plan.reused_ids.len(),
            issues_deleted: plan.delete_ids.len(),
            events_moved: plan.assignments.len(),
            events_skipped: plan.skipped_events,
        })
    }
}

#[async_trait]
impl TraceRepository for SqliteIssueRepository {
    async fn record_trace(&self, trace: TraceRecord) -> Result<TraceRecord, RepositoryError> {
        // C3：`BEGIN IMMEDIATE` 立即取写锁，杜绝并发 read-modify-write 丢轮次（修 W-02）。
        // 用 `begin_with` 而非裸 BEGIN：事务对象 Drop 时自动回滚，连接不会带着未结束事务回池。
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let record = record_trace_locked(&mut tx, trace).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn get_trace(&self, id: &str) -> Result<TraceRecord, RepositoryError> {
        let row = sqlx::query(&format!("SELECT {TRACE_COLUMNS} FROM traces WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| RepositoryError::TraceNotFound(id.to_string()))?;

        Ok(trace_from_row(&row))
    }

    async fn list_traces(&self, filter: TraceFilter) -> Result<Vec<TraceRecord>, RepositoryError> {
        let (clause, binds) = sqlite_trace_filter_clause(&filter);
        // `amend v1.5-2`：默认按 `reported_at DESC`，`order_by=CreatedAt` 可切回历史行为。
        let order = filter.order_by.unwrap_or_default().order_clause();
        // C5：limit clamp 到 [1,100]；offset 不得为负（HTTP 层负责 400，仓储层兜底）。
        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let offset = filter.offset.unwrap_or(0).max(0);
        let query = format!(
            "SELECT {TRACE_COLUMNS} FROM traces{clause} {order} LIMIT {limit} OFFSET {offset}"
        );

        let mut q = sqlx::query(&query);
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows.iter().map(trace_from_row).collect())
    }

    async fn count_traces(&self, filter: TraceFilter) -> Result<i64, RepositoryError> {
        let (clause, binds) = sqlite_trace_filter_clause(&filter);
        let query = format!("SELECT COUNT(*) AS n FROM traces{clause}");
        let mut q = sqlx::query(&query);
        for b in binds {
            q = q.bind(b);
        }
        let row = q.fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("n"))
    }

    async fn update_trace_review(
        &self,
        id: &str,
        status: EvalStatus,
        note: Option<&str>,
    ) -> Result<TraceRecord, RepositoryError> {
        let now = Utc::now();
        // `note` 为 None 时保留库内旧值（COALESCE），Some("") 则显式清空。
        let rows_affected = sqlx::query(
            "UPDATE traces SET eval_status = ?, note = COALESCE(?, note), updated_at = ? WHERE id = ?",
        )
        .bind(status.to_string())
        .bind(note)
        .bind(now.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();

        if rows_affected == 0 {
            return Err(RepositoryError::TraceNotFound(id.to_string()));
        }

        self.get_trace(id).await
    }

    async fn list_projects(&self) -> Result<Vec<String>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT DISTINCT project FROM traces
             WHERE project IS NOT NULL AND project != ''
             ORDER BY project ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| row.get::<String, _>("project"))
            .collect())
    }

    async fn delete_traces_older_than(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: i64,
    ) -> Result<u64, RepositoryError> {
        let batch_size = batch_size.max(1);
        // C6：保留期按 `COALESCE(updated_at, created_at)` 判定（新活跃行不因首写时间早被误删）。
        // amend v1.4-3：固定 id 集后再删轮次——两条 DELETE 在同一事务内（BEGIN IMMEDIATE 取写锁），
        // 先按**已选定的 id 集**删 `trace_turns`，再删 `traces`，禁止误删未过期会话的轮次。
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query(
            "SELECT id FROM traces
             WHERE COALESCE(updated_at, created_at) < ?
             ORDER BY COALESCE(updated_at, created_at) ASC
             LIMIT ?",
        )
        .bind(cutoff.to_rfc3339())
        .bind(batch_size)
        .fetch_all(&mut *tx)
        .await?;
        let ids: Vec<String> = rows.iter().map(|row| row.get::<String, _>("id")).collect();
        if ids.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let delete_turns_sql = format!(
            "DELETE FROM trace_turns
             WHERE session_id IN (SELECT session_id FROM traces WHERE id IN ({placeholders}))"
        );
        let mut delete_turns = sqlx::query(&delete_turns_sql);
        for id in &ids {
            delete_turns = delete_turns.bind(id);
        }
        delete_turns.execute(&mut *tx).await?;
        let delete_traces_sql = format!("DELETE FROM traces WHERE id IN ({placeholders})");
        let mut delete_traces = sqlx::query(&delete_traces_sql);
        for id in &ids {
            delete_traces = delete_traces.bind(id);
        }
        let deleted = delete_traces.execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(deleted)
    }
}

/// 拼接 `traces` 过滤子句（`list_traces` / `count_traces` 共用，保证两者口径一致）。
///
/// `run_id` 语义（`amend v1.5-6` / W-04）：命中 `traces.run_id` **或**其任一轮次的
/// `trace_turns.run_id`。
fn sqlite_trace_filter_clause(filter: &TraceFilter) -> (String, Vec<String>) {
    let mut clause = " WHERE 1=1".to_string();
    let mut binds: Vec<String> = Vec::new();
    if let Some(session_id) = &filter.session_id {
        clause.push_str(" AND session_id = ?");
        binds.push(session_id.clone());
    }
    if let Some(eval_status) = &filter.eval_status {
        clause.push_str(" AND eval_status = ?");
        binds.push(eval_status.to_string());
    }
    if let Some(environment) = &filter.environment {
        clause.push_str(" AND environment = ?");
        binds.push(environment.clone());
    }
    if let Some(release) = &filter.release {
        clause.push_str(" AND release = ?");
        binds.push(release.clone());
    }
    if let Some(project) = &filter.project {
        clause.push_str(" AND project = ?");
        binds.push(project.clone());
    }
    if let Some(run_id) = &filter.run_id {
        clause.push_str(
            " AND (run_id = ? OR EXISTS (SELECT 1 FROM trace_turns AS tt
                 WHERE tt.session_id = traces.session_id AND tt.run_id = ?))",
        );
        binds.push(run_id.clone());
        binds.push(run_id.clone());
    }
    if let Some(turn_status) = &filter.turn_status {
        clause.push_str(" AND turn_status = ?");
        binds.push(turn_status.clone());
    }
    if let Some(from) = &filter.from {
        clause.push_str(" AND reported_at >= ?");
        binds.push(from.to_rfc3339());
    }
    if let Some(to) = &filter.to {
        clause.push_str(" AND reported_at <= ?");
        binds.push(to.to_rfc3339());
    }
    (clause, binds)
}

/// `traces` 行读取列（对齐 0003 + 0005 新增的 `turn_status` / `turns_count` / `note`）。
const TRACE_COLUMNS: &str = "id, session_id, run_id, turn_id, environment, release, project, \
     eval_status, payload, total_input_tokens, total_output_tokens, total_duration_ms, \
     turn_status, turns_count, note, reported_at, created_at, updated_at";

/// 事务内完成一次上报写入（契约 `C2` / `C3` / `C4`）：
///
/// 1. 读旧聚合行（供 `extra`/`tags` 深合并与 `eval_status`/`note` 保留）；
/// 2. 逐 turn `merge_turn`（终态优先）→ `annotate_turn`（诊断标注）→ upsert `trace_turns`；
/// 3. 按 `seq` 读回全部 turn → `merge_top_level` + `aggregate_session`（token 单一真相）；
/// 4. upsert `traces` 聚合行并**回查库内真实行**返回（响应与库一致，修 W-08）。
async fn record_trace_locked(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    trace: TraceRecord,
) -> Result<TraceRecord, RepositoryError> {
    let now = Utc::now();
    let session_id = trace.session_id.clone();
    // `amend v1.5-4`（I-02）：就地归一 turns（丢弃非法元素 / status 归一 / tool_calls 非数组忽略 /
    // 单 tool_call 超长文本截断），warnings 随后写入 `payload._warnings`。
    let mut incoming_top = trace.payload.clone();
    let turn_warnings = crate::trace_v2::normalize_incoming_turns(&mut incoming_top);
    // `amend v1.6-4`：先脱敏（routes 侧）→ 再截断（此处）；截断只作用于已脱敏文本。
    crate::trace_v2::truncate_turns_tool_text(&mut incoming_top);
    let existing = fetch_trace_by_session(&mut *tx, &session_id).await?;
    let old_payload = existing
        .as_ref()
        .map(|row| row.payload.clone())
        .unwrap_or(serde_json::Value::Null);

    // 覆盖度兜底（amend v1.4-1）：`trace_turns` 行数 < payload.turns 条数时按 turn_id 补齐，
    // 否则本次聚合只看得见新 turn，会把缺失的历史轮次整体丢掉。
    hydrate_trace_turns(&mut *tx, &session_id, &old_payload).await?;

    let incoming_turns = incoming_top
        .get("turns")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    // W-04（amend v1.5-6）：轮次 run 归属 = turn.run_id，缺省回退 payload 顶层 run_id。
    let top_run_id = incoming_top
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_string)
        .or_else(|| trace.run_id.clone());
    for raw_turn in incoming_turns {
        let incoming = crate::trace_v2::normalize_incoming_turn(&raw_turn);
        let turn_id = crate::trace_v2::turn_key_of(&incoming);
        let turn_run_id = turn_run_id_of(&incoming, top_run_id.as_deref());
        let (seq, mut merged) = match fetch_trace_turn(&mut *tx, &session_id, &turn_id).await? {
            Some((seq, stored)) => (seq, crate::trace_v2::merge_turn(&stored, &incoming)),
            None => (next_turn_seq(&mut *tx, &session_id).await?, incoming),
        };
        if let Some(object) = merged.as_object_mut() {
            object.insert(
                "turn_id".to_string(),
                serde_json::Value::String(turn_id.clone()),
            );
            object.insert("seq".to_string(), serde_json::Value::from(seq));
        }
        crate::trace_v2::annotate_turn(&mut merged);
        let status = crate::trace_v2::turn_status_of(&merged);
        store_trace_turn(
            &mut *tx,
            &session_id,
            &turn_id,
            seq,
            &status,
            turn_run_id.as_deref(),
            &merged,
            now,
        )
        .await?;
    }

    let turns = list_turn_payloads(&mut *tx, &session_id).await?;
    if let Some(object) = incoming_top.as_object_mut() {
        object.insert(
            "session_id_source".to_string(),
            serde_json::Value::String(crate::trace_v2::session_id_source_of(&trace.payload)),
        );
    }
    let top = crate::trace_v2::merge_top_level(&old_payload, &incoming_top);
    let mut payload = crate::trace_v2::aggregate_session(&turns, &top);
    merge_warnings(&mut payload, &old_payload, turn_warnings);
    let turn_status = turns
        .last()
        .map(crate::trace_v2::turn_status_of)
        .or_else(|| existing.as_ref().map(|row| row.turn_status.clone()))
        .unwrap_or_else(|| "unknown".to_string());
    let turns_count = turns.len() as i64;
    let total_input_tokens = payload
        .get("total_input_tokens")
        .and_then(serde_json::Value::as_i64);
    let total_output_tokens = payload
        .get("total_output_tokens")
        .and_then(serde_json::Value::as_i64);
    let total_duration_ms = payload
        .get("total_duration_ms")
        .and_then(serde_json::Value::as_i64);

    upsert_trace_row(
        &mut *tx,
        &trace,
        &payload,
        &turn_status,
        turns_count,
        total_input_tokens,
        total_output_tokens,
        total_duration_ms,
        now,
    )
    .await?;

    fetch_trace_by_session(&mut *tx, &session_id)
        .await?
        .ok_or_else(|| RepositoryError::TraceNotFound(session_id.clone()))
}

/// 事务内按 `session_id` 读聚合行（回写后回查用）。
async fn fetch_trace_by_session(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
) -> Result<Option<TraceRecord>, sqlx::Error> {
    let row = sqlx::query(&format!(
        "SELECT {TRACE_COLUMNS} FROM traces WHERE session_id = ?"
    ))
    .bind(session_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(trace_from_row))
}

/// 按覆盖度补齐 `trace_turns`（amend v1.4-1 / L3A-F3）：
/// 仅当「已入库行数 < `payload.turns` 条数」时，用与运行时**同一套** `trace_v2::turn_keys_for`
/// （显式 turn_id 优先，缺失时内容 sha256 生成 `turn_srv_*` 并对同批内容碰撞加序号后缀）逐条补齐；
/// 已存在的键跳过（幂等，不覆盖库内更新的轮次）。
async fn hydrate_trace_turns(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    payload: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let top_run_id = payload
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty());
    let legacy_turns = payload
        .get("turns")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if legacy_turns.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query("SELECT turn_id, seq FROM trace_turns WHERE session_id = ?")
        .bind(session_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut used_seqs: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for row in &rows {
        existing.insert(row.get::<String, _>("turn_id"));
        used_seqs.insert(row.get::<i64, _>("seq"));
    }
    // 覆盖度判据：行数达到 payload.turns 条数即视为完整（不再只判"有行"）。
    if existing.len() >= legacy_turns.len() {
        return Ok(());
    }
    let keys = crate::trace_v2::turn_keys_for(&legacy_turns);
    let now = Utc::now();
    for (index, (raw_turn, turn_id)) in legacy_turns.iter().zip(keys.iter()).enumerate() {
        if existing.contains(turn_id) {
            continue;
        }
        let mut turn = crate::trace_v2::normalize_incoming_turn(raw_turn);
        let mut seq = index as i64;
        while used_seqs.contains(&seq) {
            seq += 1;
        }
        used_seqs.insert(seq);
        existing.insert(turn_id.clone());
        if let Some(object) = turn.as_object_mut() {
            object.insert(
                "turn_id".to_string(),
                serde_json::Value::String(turn_id.clone()),
            );
            object.insert("seq".to_string(), serde_json::Value::from(seq));
        }
        let status = crate::trace_v2::turn_status_of(&turn);
        let run_id = turn_run_id_of(&turn, top_run_id);
        store_trace_turn(
            conn,
            session_id,
            turn_id,
            seq,
            &status,
            run_id.as_deref(),
            &turn,
            now,
        )
        .await?;
    }
    Ok(())
}

/// 回填/补齐后刷新聚合列（`turns_count` = 行数、`turn_status` = 末轮 status），幂等。
async fn refresh_trace_turn_aggregates(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE traces SET
             turns_count = (SELECT COUNT(*) FROM trace_turns WHERE session_id = ?),
             turn_status = COALESCE((
                 SELECT status FROM trace_turns WHERE session_id = ?
                 ORDER BY seq DESC LIMIT 1), 'unknown')
         WHERE session_id = ?",
    )
    .bind(session_id)
    .bind(session_id)
    .bind(session_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 读某轮已入库的 `(seq, payload)`；不存在返回 `None`。
async fn fetch_trace_turn(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    turn_id: &str,
) -> Result<Option<(i64, serde_json::Value)>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT seq, payload FROM trace_turns WHERE session_id = ? AND turn_id = ?",
    )
    .bind(session_id)
    .bind(turn_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let seq: i64 = row.get("seq");
    let payload: String = row.get("payload");
    let payload = serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
    Ok(Some((seq, payload)))
}

/// `C2.5`：新 turn 的 `seq = MAX(seq) + 1`（会话内从 0 起单调）。
async fn next_turn_seq(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(seq) + 1, 0) AS next_seq FROM trace_turns WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.get("next_seq"))
}

/// 写入/合并单条 turn（`seq` 与 `created_at` 在冲突时保持不变，保证顺序稳定）。
#[allow(clippy::too_many_arguments)]
async fn store_trace_turn(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    turn_id: &str,
    seq: i64,
    status: &str,
    run_id: Option<&str>,
    payload: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let payload = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    sqlx::query(
        "INSERT INTO trace_turns
             (session_id, turn_id, seq, status, run_id, payload, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (session_id, turn_id) DO UPDATE SET
             status = excluded.status,
             run_id = COALESCE(excluded.run_id, trace_turns.run_id),
             payload = excluded.payload,
             updated_at = excluded.updated_at",
    )
    .bind(session_id)
    .bind(turn_id)
    .bind(seq)
    .bind(status)
    .bind(run_id)
    .bind(payload)
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 轮次的 run 归属（`W-04`）：`turn.run_id` 优先，缺失回退顶层 `run_id`。
fn turn_run_id_of(turn: &serde_json::Value, top_run_id: Option<&str>) -> Option<String> {
    turn.get("run_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_string)
        .or_else(|| top_run_id.map(str::to_string))
}

/// warnings 去重身份：`(turn_index, reason)`（`amend v1.6-3`）。
fn warning_identity(warning: &serde_json::Value) -> (i64, String) {
    (
        warning
            .get("turn_index")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1),
        warning
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// 把本轮 warnings 与旧 `payload._warnings` 合并写回（`turn_index+reason` 去重，最多 100 条 —— `I-02`/`v1.6-3`）。
fn merge_warnings(
    payload: &mut serde_json::Value,
    old_payload: &serde_json::Value,
    warnings: Vec<serde_json::Value>,
) {
    // `amend v1.6-3`：入参 warnings（仓储归一产生）与**入参 payload 已携带的 `_warnings`**
    // （HTTP 层归一产生）都要参与并集，否则二次上报会以旧值覆盖新值。
    let incoming: Vec<serde_json::Value> = payload
        .get("_warnings")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if warnings.is_empty() && incoming.is_empty() && old_payload.get("_warnings").is_none() {
        return;
    }
    let mut merged: Vec<serde_json::Value> = old_payload
        .get("_warnings")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen: std::collections::HashSet<(i64, String)> =
        merged.iter().map(warning_identity).collect();
    for warning in incoming.into_iter().chain(warnings) {
        if seen.insert(warning_identity(&warning)) {
            merged.push(warning);
        }
    }
    merged.truncate(100);
    if let Some(object) = payload.as_object_mut() {
        if merged.is_empty() {
            object.remove("_warnings");
        } else {
            object.insert("_warnings".to_string(), serde_json::Value::Array(merged));
        }
    }
}

/// 按 `seq` 升序读回会话全部 turn（聚合的唯一数据源）。
async fn list_turn_payloads(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT payload FROM trace_turns WHERE session_id = ? ORDER BY seq ASC",
    )
    .bind(session_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let payload: String = row.get("payload");
            serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null)
        })
        .collect())
}

/// upsert `traces` 聚合行（`created_at` 与既有 `note`/`eval_status` 不被覆盖）。
#[allow(clippy::too_many_arguments)]
async fn upsert_trace_row(
    conn: &mut sqlx::SqliteConnection,
    trace: &TraceRecord,
    payload: &serde_json::Value,
    turn_status: &str,
    turns_count: i64,
    total_input_tokens: Option<i64>,
    total_output_tokens: Option<i64>,
    total_duration_ms: Option<i64>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let payload = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    sqlx::query(
        "INSERT INTO traces (
            id, session_id, run_id, turn_id, environment, release, project, eval_status, payload,
            total_input_tokens, total_output_tokens, total_duration_ms,
            turn_status, turns_count, note, reported_at, created_at, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (session_id) DO UPDATE SET
            run_id = excluded.run_id,
            turn_id = excluded.turn_id,
            environment = excluded.environment,
            release = excluded.release,
            project = COALESCE(excluded.project, traces.project),
            eval_status = CASE WHEN traces.eval_status != 'unreviewed'
                               THEN traces.eval_status ELSE excluded.eval_status END,
            payload = excluded.payload,
            total_input_tokens = excluded.total_input_tokens,
            total_output_tokens = excluded.total_output_tokens,
            total_duration_ms = excluded.total_duration_ms,
            turn_status = excluded.turn_status,
            turns_count = excluded.turns_count,
            note = COALESCE(traces.note, excluded.note),
            reported_at = excluded.reported_at,
            updated_at = excluded.updated_at",
    )
    .bind(&trace.id)
    .bind(&trace.session_id)
    .bind(&trace.run_id)
    .bind(&trace.turn_id)
    .bind(&trace.environment)
    .bind(&trace.release)
    .bind(&trace.project)
    .bind(trace.eval_status.to_string())
    .bind(payload)
    .bind(total_input_tokens)
    .bind(total_output_tokens)
    .bind(total_duration_ms)
    .bind(turn_status)
    .bind(turns_count)
    .bind(&trace.note)
    .bind(trace.reported_at.to_rfc3339())
    .bind(trace.created_at.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn trace_from_row(row: &sqlx::sqlite::SqliteRow) -> TraceRecord {
    let eval_status_str: String = row.get("eval_status");
    let eval_status = eval_status_str
        .parse::<EvalStatus>()
        .unwrap_or(EvalStatus::Unreviewed);
    let payload_str: String = row.get("payload");
    let payload: serde_json::Value =
        serde_json::from_str(&payload_str).unwrap_or_else(|_| serde_json::json!({}));
    let reported_at_str: String = row.get("reported_at");
    let created_at_str: String = row.get("created_at");
    let updated_at_str: String = row.get("updated_at");

    TraceRecord {
        id: row.get("id"),
        session_id: row.get("session_id"),
        run_id: row.get("run_id"),
        turn_id: row.get("turn_id"),
        environment: row.get("environment"),
        release: row.get("release"),
        project: row.get("project"),
        eval_status,
        payload,
        total_input_tokens: row.get("total_input_tokens"),
        total_output_tokens: row.get("total_output_tokens"),
        total_duration_ms: row.get("total_duration_ms"),
        // C1.2 聚合列（0005 迁移新增）：`turn_status` 缺失时按 `unknown` 降级。
        turn_status: row
            .try_get("turn_status")
            .unwrap_or_else(|_| "unknown".to_string()),
        turns_count: row.try_get("turns_count").unwrap_or(0),
        note: row.try_get("note").unwrap_or(None),
        reported_at: chrono::DateTime::parse_from_rfc3339(&reported_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    }
}


// ============================================================================
// 契约 `.dev-team/contract-issue-governance.md`（FROZEN **v1.2**）治理能力：SQLite 实现
//
// 口径单一真相：`list_issues` / `count_issues` / stats 聚合共用
// `repository::issue_filter_clause`。时间列在 SQLite 侧是 **ISO8601 UTC 文本**，
// 故比较统一按「库内同一格式的原始串」做字典序（契约 C4.2 / C7.1：存储格式零变更）。
// ============================================================================
#[async_trait]
impl IssueGovernanceRepository for SqliteIssueRepository {
    async fn count_issues(&self, filter: IssueFilter) -> Result<i64, RepositoryError> {
        let clause = crate::repository::issue_filter_clause(&filter, "", |_| "?".to_string());
        sqlite_scalar(
            &self.pool,
            &format!("SELECT COUNT(*) AS n FROM issues{}", clause.clause),
            &clause.binds,
        )
        .await
    }

    async fn archive_issue(
        &self,
        id: &str,
        archived_at: DateTime<Utc>,
    ) -> Result<Issue, RepositoryError> {
        // 幂等（C2.3）：已归档 → `archived_at` 不变（COALESCE 保留原值）；
        // `archive_restored_at` 不被覆盖（`amend v1.1 A-1`）。
        let affected = sqlx::query(
            "UPDATE issues SET archived_at = COALESCE(archived_at, ?) WHERE id = ?",
        )
        .bind(archived_at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(RepositoryError::NotFound(id.to_string()));
        }
        self.issue_by_id(id).await
    }

    async fn unarchive_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        // 同语句写 `archived_at = NULL` + `archive_restored_at = now`（`amend v1.1 A-1`）。
        let affected =
            sqlx::query("UPDATE issues SET archived_at = NULL, archive_restored_at = ? WHERE id = ?")
                .bind(Utc::now().to_rfc3339())
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected();
        if affected == 0 {
            return Err(RepositoryError::NotFound(id.to_string()));
        }
        self.issue_by_id(id).await
    }

    async fn archive_issues(
        &self,
        request: &IssueArchiveRequest,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<IssueArchiveOutcome, RepositoryError> {
        let statuses = crate::repository::effective_archive_statuses(request);
        let project = request
            .project
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let ids = request
            .ids
            .as_ref()
            .map(|raw| crate::repository::dedup_ids(raw))
            .filter(|deduped| !deduped.is_empty());
        let ids_take_priority = ids.is_some();
        let mut outcome = IssueArchiveOutcome {
            dry_run,
            matched: 0,
            archived: 0,
            skipped_already_archived: 0,
            skipped_not_archivable: 0,
            skipped_missing: 0,
            criteria: IssueArchiveCriteria {
                project: project.clone(),
                statuses: statuses.clone(),
                older_than_days: request.older_than_days,
                ids: request.ids.clone(),
                ids_take_priority,
            },
        };

        match ids {
            // ---- `ids` 模式（`amend v1.1 A-3` + `v1.2`）：ids 定候选集，statuses 仍是白名单 ----
            Some(ids) => {
                outcome.matched = ids.len() as i64;
                // SQLite 绑定变量上限 999：ids 必须分批（每批 ≤ [`SQLITE_MAX_BIND_VARS`]），
                // 且为 statuses 留出余量，超限不报错。
                // `BEGIN IMMEDIATE`：先 SELECT 分桶再 UPDATE 的两次读写必须在同一写锁内，
                // 否则并发归档会在锁升级处 `database is locked`（deferred 事务升级竞态）。
                let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
                let mut found: HashMap<String, (String, bool)> = HashMap::new();
                for chunk in ids.chunks(SQLITE_MAX_BIND_VARS) {
                    let sql = format!(
                        "SELECT id, status, archived_at FROM issues WHERE id IN ({})",
                        sqlite_placeholders(chunk.len())
                    );
                    let mut query = sqlx::query(&sql);
                    for id in chunk {
                        query = query.bind(id.as_str());
                    }
                    for row in query.fetch_all(&mut *tx).await? {
                        found.insert(
                            row.get::<String, _>("id"),
                            (
                                row.get::<String, _>("status"),
                                row.get::<Option<String>, _>("archived_at").is_some(),
                            ),
                        );
                    }
                }
                for id in &ids {
                    match found.get(id) {
                        None => outcome.skipped_missing += 1,
                        Some((status, archived)) => {
                            if !statuses.iter().any(|allowed| allowed == status) {
                                outcome.skipped_not_archivable += 1;
                            } else if *archived {
                                outcome.skipped_already_archived += 1;
                            }
                        }
                    }
                }
                if !dry_run {
                    let status_placeholders = sqlite_placeholders(statuses.len());
                    let mut archived_total = 0u64;
                    for chunk in ids.chunks(SQLITE_MAX_BIND_VARS) {
                        let sql = format!(
                            "UPDATE issues SET archived_at = ? \
                             WHERE id IN ({}) AND status IN ({status_placeholders}) AND archived_at IS NULL",
                            sqlite_placeholders(chunk.len())
                        );
                        let mut query = sqlx::query(&sql).bind(now.to_rfc3339());
                        for id in chunk {
                            query = query.bind(id.as_str());
                        }
                        for status in &statuses {
                            query = query.bind(status.as_str());
                        }
                        archived_total += query.execute(&mut *tx).await?.rows_affected();
                    }
                    outcome.archived = archived_total as i64;
                }
                tx.commit().await?;
            }
            // ---- `statuses`/`project` 模式：`matched ≡ archived`，`skipped_*` 恒 0 ----
            None => {
                let cutoff = request
                    .older_than_days
                    .map(|days| now - chrono::Duration::days(days.max(0)));
                let mut condition = format!(
                    "status IN ({}) AND archived_at IS NULL",
                    sqlite_placeholders(statuses.len())
                );
                if project.is_some() {
                    condition.push_str(" AND project = ?");
                }
                if cutoff.is_some() {
                    condition.push_str(" AND last_seen_at < ?");
                }
                if dry_run {
                    let sql = format!("SELECT COUNT(*) AS n FROM issues WHERE {condition}");
                    let mut query = sqlx::query(&sql);
                    for status in &statuses {
                        query = query.bind(status.as_str());
                    }
                    if let Some(project) = &project {
                        query = query.bind(project.as_str());
                    }
                    if let Some(cutoff) = cutoff {
                        query = query.bind(cutoff.to_rfc3339());
                    }
                    let row = query.fetch_one(&self.pool).await?;
                    outcome.matched = row.get::<i64, _>("n");
                    outcome.archived = 0;
                } else {
                    let sql = format!("UPDATE issues SET archived_at = ? WHERE {condition}");
                    let mut query = sqlx::query(&sql).bind(now.to_rfc3339());
                    for status in &statuses {
                        query = query.bind(status.as_str());
                    }
                    if let Some(project) = &project {
                        query = query.bind(project.as_str());
                    }
                    if let Some(cutoff) = cutoff {
                        query = query.bind(cutoff.to_rfc3339());
                    }
                    let updated = query.execute(&self.pool).await?.rows_affected();
                    outcome.matched = updated as i64;
                    outcome.archived = updated as i64;
                }
            }
        }
        Ok(outcome)
    }

    async fn archive_stale_issues(
        &self,
        cutoff: DateTime<Utc>,
        statuses: &[IssueStatus],
    ) -> Result<u64, RepositoryError> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let statuses: Vec<String> = statuses.iter().map(|status| status.to_string()).collect();
        // C2.1 / `amend v1.1 A-1`：末项 `archive_restored_at` 抑制项必须在内；不覆盖 `archive_restored_at`。
        let sql = format!(
            "UPDATE issues SET archived_at = ? \
             WHERE status IN ({}) AND archived_at IS NULL AND last_seen_at < ? \
               AND (archive_restored_at IS NULL OR archive_restored_at < last_seen_at)",
            sqlite_placeholders(statuses.len())
        );
        // 绑定顺序必须与 SQL 占位符顺序一致：`archived_at = ?` → `status IN (?...)` → `last_seen_at < ?`。
        let mut query = sqlx::query(&sql).bind(Utc::now().to_rfc3339());
        for status in &statuses {
            query = query.bind(status.as_str());
        }
        let query = query.bind(cutoff.to_rfc3339());
        Ok(query.execute(&self.pool).await?.rows_affected())
    }

    async fn list_issue_events_page(
        &self,
        issue_id: &str,
        limit: i64,
        cursor: Option<IssueEventsCursor>,
        order: IssueEventsOrder,
    ) -> Result<Vec<Event>, RepositoryError> {
        let limit = limit.clamp(1, 100);
        let (comparator, direction) = match order {
            IssueEventsOrder::Desc => ("<", "DESC"),
            IssueEventsOrder::Asc => (">", "ASC"),
        };
        // `amend v1.1 B-2` / C7.1：游标原样携带库内 RFC3339 串，SQLite 侧按 TEXT 字典序比较。
        // 行为稳健起见把等价时刻归一为**库内写入格式**（`DateTime::to_rfc3339`，AutoSi + `+00:00`），
        // 避免上游用 `Z` 后缀重排格式时字典序失真；解析失败则原样比较（不 panic、不 500）。
        let cursor_at = cursor.as_ref().map(|payload| {
            DateTime::parse_from_rfc3339(&payload.created_at)
                .map(|value| value.with_timezone(&Utc).to_rfc3339())
                .unwrap_or_else(|_| payload.created_at.clone())
        });
        let mut sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE issue_id = ?");
        if cursor_at.is_some() {
            sql.push_str(&format!(
                " AND (created_at {comparator} ? OR (created_at = ? AND id {comparator} ?))"
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at {direction}, id {direction} LIMIT ?"));
        let mut query = sqlx::query(&sql).bind(issue_id);
        if let (Some(at), Some(payload)) = (cursor_at, cursor.as_ref()) {
            // 匿名 `?` 逐位绑定：`created_at < ?` 与 `created_at = ?` 是**两个**占位符，
            // 必须把同一个游标时刻绑两次（`id < ?` 第三次）。
            query = query.bind(at.clone()).bind(at).bind(&payload.id);
        }
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(event_from_row).collect())
    }

    async fn count_events_for_issue(&self, issue_id: &str) -> Result<i64, RepositoryError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM events WHERE issue_id = ?")
            .bind(issue_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("n"))
    }

    async fn issue_stats(&self, query: &IssueStatsQuery) -> Result<IssueStatsResponse, RepositoryError> {
        let scope = crate::repository::stats_scope(query, Utc::now());
        // ---- issues 口径（受 project/environment/level/include_archived；**不受** from/to，C5.0）----
        let clause = crate::repository::issue_filter_clause(&scope.filter, "", |_| "?".to_string());
        let total = sqlite_scalar(
            &self.pool,
            &format!("SELECT COUNT(*) AS n FROM issues{}", clause.clause),
            &clause.binds,
        )
        .await?;
        let by_status = sqlite_grouped(
            &self.pool,
            &format!(
                "SELECT status AS k, COUNT(*) AS n FROM issues{} GROUP BY 1",
                clause.clause
            ),
            &clause.binds,
        )
        .await?;
        let by_level = sqlite_grouped(
            &self.pool,
            &format!(
                "SELECT level AS k, COUNT(*) AS n FROM issues{} GROUP BY 1",
                clause.clause
            ),
            &clause.binds,
        )
        .await?;
        let by_environment = sqlite_grouped(
            &self.pool,
            &format!(
                "SELECT environment AS k, COUNT(*) AS n FROM issues{} GROUP BY 1",
                clause.clause
            ),
            &clause.binds,
        )
        .await?;
        // `events.total` = SUM(issues.count)（聚合口径，全时段）。
        let events_total = sqlite_scalar(
            &self.pool,
            &format!(
                "SELECT COALESCE(SUM(count), 0) AS n FROM issues{}",
                clause.clause
            ),
            &clause.binds,
        )
        .await?;

        // ---- events 行口径（受 from/to；include_archived=false 时只计未归档 issue 的行）----
        let row_clause = crate::repository::issue_filter_clause(&scope.filter, "i.", |_| "?".to_string());
        let row_from = format!(
            " FROM events e JOIN issues i ON i.id = e.issue_id{} \
             AND e.created_at >= ? AND e.created_at <= ?",
            row_clause.clause
        );
        let mut row_binds = row_clause.binds.clone();
        row_binds.push(scope.from.to_rfc3339());
        row_binds.push(scope.to.to_rfc3339());
        let stored_rows = sqlite_scalar(
            &self.pool,
            &format!("SELECT COUNT(*) AS n{row_from}"),
            &row_binds,
        )
        .await?;
        let ev_by_level = sqlite_grouped(
            &self.pool,
            &format!("SELECT COALESCE(e.level, 'unknown') AS k, COUNT(*) AS n{row_from} GROUP BY 1"),
            &row_binds,
        )
        .await?;
        let ev_by_environment = sqlite_grouped(
            &self.pool,
            &format!(
                "SELECT COALESCE(e.environment, 'unknown') AS k, COUNT(*) AS n{row_from} GROUP BY 1"
            ),
            &row_binds,
        )
        .await?;
        let ev_by_release = sqlite_grouped(
            &self.pool,
            &format!(
                "SELECT COALESCE(e.release, 'unknown') AS k, COUNT(*) AS n{row_from} GROUP BY 1"
            ),
            &row_binds,
        )
        .await?;

        // ---- series（空桶补 0；`sum(series.events) == stored_rows` 由同 WHERE 保证）----
        let issue_bucket = sqlite_bucket_prefix("first_seen_at", scope.bucket);
        let mut issue_binds = clause.binds.clone();
        issue_binds.push(scope.from.to_rfc3339());
        issue_binds.push(scope.to.to_rfc3339());
        let issue_series = sqlite_grouped(
            &self.pool,
            &format!(
                "SELECT {issue_bucket} AS k, COUNT(*) AS n FROM issues{} \
                 AND first_seen_at >= ? AND first_seen_at <= ? GROUP BY 1",
                clause.clause
            ),
            &issue_binds,
        )
        .await?;
        let event_bucket = sqlite_bucket_prefix("e.created_at", scope.bucket);
        let event_series = sqlite_grouped(
            &self.pool,
            &format!("SELECT {event_bucket} AS k, COUNT(*) AS n{row_from} GROUP BY 1"),
            &row_binds,
        )
        .await?;

        let issue_series = sqlite_rekey_buckets(issue_series, scope.bucket);
        let event_series = sqlite_rekey_buckets(event_series, scope.bucket);
        let series = crate::repository::bucket_keys(scope.from, scope.to, scope.bucket)
            .into_iter()
            .map(|key| StatsSeriesPoint {
                issues: issue_series.get(&key).copied().unwrap_or(0),
                events: event_series.get(&key).copied().unwrap_or(0),
                bucket: key,
            })
            .collect();

        Ok(IssueStatsResponse {
            project: scope.filter.project.clone(),
            range: StatsRange {
                from: scope.from,
                to: scope.to,
                bucket: scope.bucket,
            },
            include_archived: scope.include_archived,
            issues: IssueStatsCounts {
                total,
                by_status,
                by_level,
                by_environment,
            },
            events: EventStatsCounts {
                total: events_total,
                basis: "sum_issues_count".to_string(),
                stored_rows,
                by_level: ev_by_level,
                by_environment: ev_by_environment,
                by_release: ev_by_release,
            },
            series,
        })
    }

    async fn ingest_governance_stats(
        &self,
        query: &IngestGovernanceQuery,
    ) -> Result<IngestGovernanceStats, RepositoryError> {
        let now = Utc::now();
        let window = match query.window.as_deref().map(str::trim) {
            Some("7d") => "7d",
            Some("30d") => "30d",
            _ => "24h",
        };
        let span = match window {
            "7d" => chrono::Duration::days(7),
            "30d" => chrono::Duration::days(30),
            _ => chrono::Duration::hours(24),
        };
        let to = now;
        let from = now - span;
        let bucket = match query.bucket.as_deref().map(str::trim) {
            Some("hour") => StatsBucket::Hour,
            _ => StatsBucket::Day,
        };
        let project = query
            .project
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        // 计数按 UTC 日期落库（`bucket_date` 为 `YYYY-MM-DD` 文本），按日期闭区间过滤。
        let mut where_clause = " WHERE bucket_date >= ? AND bucket_date <= ?".to_string();
        if project.is_some() {
            where_clause.push_str(" AND project = ?");
        }
        let from_date = from.date_naive().format("%Y-%m-%d").to_string();
        let to_date = to.date_naive().format("%Y-%m-%d").to_string();
        let mut base_binds: Vec<String> = vec![from_date, to_date];
        if let Some(project) = &project {
            base_binds.push(project.clone());
        }

        let outcome_sql = format!(
            "SELECT outcome AS k, COALESCE(SUM(count), 0) AS n FROM ingest_metrics{where_clause} GROUP BY 1"
        );
        let mut sums: HashMap<String, i64> = HashMap::new();
        let mut query_handle = sqlx::query(&outcome_sql);
        for bind in &base_binds {
            query_handle = query_handle.bind(bind.as_str());
        }
        for row in query_handle.fetch_all(&self.pool).await? {
            sums.insert(row.get::<String, _>("k"), row.get::<i64, _>("n"));
        }
        let value_of = |outcome: &str| sums.get(outcome).copied().unwrap_or(0);
        let totals = IngestGovernanceTotals {
            received: value_of("received"),
            created: value_of("created"),
            duplicate: value_of("duplicate"),
            suppressed_ignored: value_of("suppressed_ignored"),
            rejected_invalid: value_of("rejected_invalid"),
            accepted: value_of("created") + value_of("duplicate"),
        };

        let by_project_sql = format!(
            "SELECT project AS k, \
             COALESCE(SUM(CASE WHEN outcome = 'received' THEN count ELSE 0 END), 0) AS received, \
             COALESCE(SUM(CASE WHEN outcome IN ('created','duplicate') THEN count ELSE 0 END), 0) AS accepted, \
             COALESCE(SUM(CASE WHEN outcome = 'suppressed_ignored' THEN count ELSE 0 END), 0) AS suppressed \
             FROM ingest_metrics{where_clause} GROUP BY project"
        );
        let mut query_handle = sqlx::query(&by_project_sql);
        for bind in &base_binds {
            query_handle = query_handle.bind(bind.as_str());
        }
        let mut by_project: Vec<IngestGovernanceProjectRow> = query_handle
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|row| IngestGovernanceProjectRow {
                project: Some(row.get::<String, _>("k")).filter(|value| !value.is_empty()),
                received: row.get("received"),
                accepted: row.get("accepted"),
                suppressed_ignored: row.get("suppressed"),
            })
            .collect();
        // `null` 排最后（`C6.1` 排序口径）。
        by_project.sort_by(|left, right| match (&left.project, &right.project) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        let series_sql = format!(
            "SELECT bucket_date AS k, \
             COALESCE(SUM(CASE WHEN outcome = 'received' THEN count ELSE 0 END), 0) AS received, \
             COALESCE(SUM(CASE WHEN outcome IN ('created','duplicate') THEN count ELSE 0 END), 0) AS accepted, \
             COALESCE(SUM(CASE WHEN outcome = 'suppressed_ignored' THEN count ELSE 0 END), 0) AS suppressed \
             FROM ingest_metrics{where_clause} GROUP BY bucket_date"
        );
        let mut query_handle = sqlx::query(&series_sql);
        for bind in &base_binds {
            query_handle = query_handle.bind(bind.as_str());
        }
        let mut day_sums: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
        for row in query_handle.fetch_all(&self.pool).await? {
            day_sums.insert(
                row.get::<String, _>("k"),
                (row.get("received"), row.get("accepted"), row.get("suppressed")),
            );
        }
        let mut series: Vec<IngestGovernanceSeriesPoint> = Vec::new();
        for key in crate::repository::bucket_keys(from, to, bucket) {
            // 存储粒度为「UTC 日期」：hour 桶时计数只可能落在当日 00:00 桶（其余桶补 0）。
            let day = key.chars().take(10).collect::<String>();
            let (received, accepted, suppressed_ignored) = match bucket {
                StatsBucket::Day => day_sums.get(&day).copied().unwrap_or((0, 0, 0)),
                StatsBucket::Hour => {
                    if key.ends_with("T00:00:00Z") {
                        day_sums.get(&day).copied().unwrap_or((0, 0, 0))
                    } else {
                        (0, 0, 0)
                    }
                }
            };
            series.push(IngestGovernanceSeriesPoint {
                bucket: key,
                received,
                accepted,
                suppressed_ignored,
            });
        }

        Ok(IngestGovernanceStats {
            window: window.to_string(),
            from,
            to,
            bucket: match bucket {
                StatsBucket::Day => "day".to_string(),
                StatsBucket::Hour => "hour".to_string(),
            },
            totals,
            by_project,
            series,
        })
    }

    async fn record_ingest_metric(
        &self,
        bucket_date: NaiveDate,
        project: &str,
        outcome: IngestMetricOutcome,
    ) -> Result<(), RepositoryError> {
        // `C6.2`（`amend v1.1 B-3`）：单语句 UPSERT；`ON CONFLICT` 目标列**必须**与主键同序。
        sqlx::query(
            "INSERT INTO ingest_metrics (bucket_date, project, outcome, count, updated_at) \
             VALUES (?, ?, ?, 1, ?) \
             ON CONFLICT(bucket_date, project, outcome) \
             DO UPDATE SET count = count + 1, updated_at = excluded.updated_at",
        )
        .bind(bucket_date.format("%Y-%m-%d").to_string())
        .bind(project)
        .bind(crate::repository::ingest_metric_outcome_str(outcome))
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// SQLite 单条语句的绑定变量上限（999）留出的安全批次：ids 分批后仍需容纳 statuses。
const SQLITE_MAX_BIND_VARS: usize = 500;

/// 生成 `?,?,?` 形态的匿名占位符串。
fn sqlite_placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

/// 标量计数查询。
async fn sqlite_scalar(
    pool: &DatabasePool,
    sql: &str,
    binds: &[String],
) -> Result<i64, RepositoryError> {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(bind.as_str());
    }
    let row = query.fetch_one(pool).await?;
    Ok(row.get::<i64, _>("n"))
}

/// 分组计数查询（`k` 文本键 / `n` 计数）。
async fn sqlite_grouped(
    pool: &DatabasePool,
    sql: &str,
    binds: &[String],
) -> Result<BTreeMap<String, i64>, RepositoryError> {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(bind.as_str());
    }
    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|row| (row.get::<String, _>("k"), row.get::<i64, _>("n")))
        .collect())
}

/// SQL 侧桶前缀（day = 10 字符 `YYYY-MM-DD`；hour = 13 字符 `YYYY-MM-DDTHH`）。
///
/// 用 `substr` 而非 `strftime`：库内时间串恒为 UTC 且格式统一（C7.1 零变更），
/// 前缀即 UTC 桶键，既不依赖 SQLite 的时区解析也不做任何精度截断。
fn sqlite_bucket_prefix(column: &str, bucket: StatsBucket) -> String {
    match bucket {
        StatsBucket::Day => format!("substr({column}, 1, 10)"),
        StatsBucket::Hour => format!("substr({column}, 1, 13)"),
    }
}

/// 把 SQL 产出的桶前缀归一为 `series[].bucket` 键。
fn sqlite_rekey_buckets(raw: BTreeMap<String, i64>, bucket: StatsBucket) -> BTreeMap<String, i64> {
    raw.into_iter()
        .map(|(prefix, count)| {
            (
                crate::repository::bucket_key_from_prefix(&prefix, bucket),
                count,
            )
        })
        .collect()
}

/// 按 `;` 拆分迁移文件为独立语句（先剔除 `--` 行注释；本批迁移文件不含字符串内分号）。
fn split_sql_statements(sql: &str) -> Vec<String> {
    sql.lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_string)
        .collect()
}

/// 解析 `ALTER TABLE <table> ADD COLUMN <column>`（SQLite 无 `IF NOT EXISTS`）。
fn add_column_target(statement: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = statement.split_whitespace().collect();
    let table = tokens.get(2)?.to_string();
    let column_keyword = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("COLUMN"))?;
    Some((table, tokens.get(column_keyword + 1)?.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_pool;
    use serde_json::json;

    fn record<'a>(fingerprint: &'a str, event_key: Option<&'a str>) -> NewEventRecord<'a> {
        NewEventRecord {
            fingerprint,
            title: "TypeError: boom",
            culprit: Some("src/main.rs in main"),
            platform: "rust",
            release: Some("1.0.0"),
            environment: Some("production"),
            project: Some("demo"),
            app: Some("api_v2"),
            namespace: Some("production"),
            component: Some("agent"),
            level: "error",
            source_kind: Some("log"),
            event_key,
            payload: json!({"message": "boom"}),
            occurred_at: None,
        }
    }

    #[tokio::test]
    async fn migrate_is_idempotent_three_times() {
        let pool = create_pool("sqlite::memory:").await.unwrap();
        let repo = SqliteIssueRepository::new(pool);
        repo.migrate().await.expect("first migrate");
        repo.record_event_and_upsert_issue(record("fp-migrate", Some("evt-migrate")))
            .await
            .expect("seed row");

        async fn counts(repo: &SqliteIssueRepository) -> (i64, i64, usize) {
            let issues = repo.issue_counts_by_status_environment().await.unwrap();
            let events = repo.count_events().await.unwrap();
            (
                issues.iter().map(|c| c.count).sum::<i64>(),
                events,
                issues.len(),
            )
        }

        let before = counts(&repo).await;
        // 契约 R5.2：连续执行 3 次不报错且行数不变
        for round in 1..=3 {
            repo.migrate()
                .await
                .unwrap_or_else(|e| panic!("migrate round {round} failed: {e}"));
            assert_eq!(counts(&repo).await, before, "round {round} 行数必须不变");
        }
    }

    #[tokio::test]
    async fn duplicate_event_key_does_not_advance_issue() {
        let pool = create_pool("sqlite::memory:").await.unwrap();
        let repo = SqliteIssueRepository::new(pool);
        repo.migrate().await.unwrap();

        let first = repo
            .record_event_and_upsert_issue(record("fp-dup", Some("evt-dup")))
            .await
            .unwrap();
        assert!(!first.is_duplicate);
        assert_eq!(first.issue.count, 1);

        let second = repo
            .record_event_and_upsert_issue(record("fp-dup", Some("evt-dup")))
            .await
            .unwrap();
        assert!(second.is_duplicate, "同 event_key 必须判定为幂等命中");
        assert_eq!(second.issue.count, 1, "duplicate 不得递增 count");
        assert_eq!(
            second.issue.last_seen_at, first.issue.last_seen_at,
            "duplicate 不得推进 last_seen_at"
        );
        assert_eq!(second.issue.id, first.issue.id);
        assert_eq!(repo.count_events().await.unwrap(), 1, "不得新增 events 行");
    }

    #[tokio::test]
    async fn environment_scopes_issue_rows() {
        let pool = create_pool("sqlite::memory:").await.unwrap();
        let repo = SqliteIssueRepository::new(pool);
        repo.migrate().await.unwrap();

        // 指纹由 v3 计算时已含 environment（契约 R6），故跨环境上报的指纹必然不同；
        // SQLite 侧因此可保留 0001 的内联 UNIQUE(fingerprint)（契约 R5.1 / R19.1）。
        let mut dev = record("fp-env-dev", None);
        dev.environment = Some("dev");
        let mut prod = record("fp-env-production", None);
        prod.environment = Some("production");

        let a = repo.record_event_and_upsert_issue(dev).await.unwrap();
        let b = repo.record_event_and_upsert_issue(prod).await.unwrap();
        assert_ne!(a.issue.id, b.issue.id, "跨环境必须形成两条 issue");
        assert_eq!(a.issue.environment, "dev");
        assert_eq!(b.issue.environment, "production");
        assert_eq!(a.issue.count, 1);
        assert_eq!(b.issue.count, 1);

        // 同环境再报一次：必须落回同一条 issue
        let mut dev_again = record("fp-env-dev", None);
        dev_again.environment = Some("dev");
        let again = repo.record_event_and_upsert_issue(dev_again).await.unwrap();
        assert_eq!(again.issue.id, a.issue.id);
        assert_eq!(again.issue.count, 2);
    }
}
