use crate::{
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
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

/// issues 表全量列（对齐 `migrations/postgres/0001` + `0002` + `0004` + `0007`）。
const ISSUE_COLUMNS: &str =
    "id, fingerprint, title, culprit, platform, status, assigned_to, count, \
     last_release, project, environment, app, namespace, component, level, source_kind, \
     first_seen_at, last_seen_at, archived_at, archive_restored_at";

/// events 表全量列（含 0004 新增 event_key / level）。
///
/// **payload 必须显式 `::text`**：PG 里 `events.payload` 是 JSONB，而读取侧按 `String` 解码后
/// 再 `serde_json` 解析（与 traces 读取路径保持一致）。直接 SELECT `payload` 会得到
/// `ColumnDecode … Rust type String (as SQL type TEXT) is not compatible with SQL type JSONB`。
const EVENT_COLUMNS: &str =
    "id, issue_id, payload::text AS payload, release, environment, event_key, level, created_at";

#[derive(Clone)]
pub struct PgIssueRepository {
    pool: PgPool,
}

impl PgIssueRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        let migration_sql = include_str!("../../../migrations/postgres/0001_init.sql");
        sqlx::raw_sql(migration_sql).execute(&self.pool).await?;
        // 0002 用 ADD COLUMN IF NOT EXISTS 表达，重复启动重放保持幂等。
        let migration_sql = include_str!("../../../migrations/postgres/0002_add_project.sql");
        sqlx::raw_sql(migration_sql).execute(&self.pool).await?;
        let migration_traces_sql = include_str!("../../../migrations/postgres/0003_add_traces.sql");
        sqlx::raw_sql(migration_traces_sql)
            .execute(&self.pool)
            .await?;
        // 0004：多环境维度 + 事件幂等键。全部语句幂等（IF NOT EXISTS / DROP ... IF EXISTS），
        // 每次启动重放安全；失败即启动失败（契约 R5.3）。
        let migration_ext_sql =
            include_str!("../../../migrations/postgres/0004_add_issue_dimensions.sql");
        sqlx::raw_sql(migration_ext_sql).execute(&self.pool).await?;
        // 0005（C1.1–C1.3）：`trace_turns` 轮次表 + `traces` 聚合列 + 幂等回填。
        // 全部语句幂等（IF NOT EXISTS / ON CONFLICT DO NOTHING），每次启动重放安全。
        // 0005（C1.1–C1.3 / amend v1.4-6 / L3B-F4）：**逐条 autocommit** 执行，不再把整段
        // 0005 塞进一条 multi-statement `raw_sql`（后者隐式单事务，会把全部 DDL 拖进一把
        // ACCESS EXCLUSIVE）。ADD COLUMN 先查 `information_schema.columns`（存在即跳过），
        // 索引改 `CREATE INDEX CONCURRENTLY`（事务外、先查 `pg_indexes`、失败清理后重试一次）。
        let schema_report = self.ensure_trace_v2_schema().await?;
        tracing::debug!(
            columns_added = schema_report.columns_added,
            columns_skipped = schema_report.columns_skipped,
            indexes_created = schema_report.indexes_created,
            indexes_skipped = schema_report.indexes_skipped,
            "trace v2 schema ensured"
        );
        // 0006（W-04 / amend v1.5-6）：`trace_turns.run_id` + 索引（同样逐条 autocommit + 跳过已存在）。
        let run_id_report = self.ensure_trace_turn_run_id_schema().await?;
        tracing::debug!(
            columns_added = run_id_report.columns_added,
            indexes_created = run_id_report.indexes_created,
            "trace turn run_id schema ensured"
        );
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
        // 0007（契约 C1.1 / C1.4）：issue 归档列 + 治理读路径索引 + `ingest_metrics`。
        // 同样逐条 autocommit：ADD COLUMN 走列探测、索引走 `CREATE INDEX CONCURRENTLY` + 存在性探测，
        // 二次启动为全 skip（不重放取锁 DDL）。
        let archive_report = self.ensure_issue_archive_schema().await?;
        tracing::debug!(
            columns_added = archive_report.columns_added,
            columns_skipped = archive_report.columns_skipped,
            indexes_created = archive_report.indexes_created,
            indexes_skipped = archive_report.indexes_skipped,
            "issue archive schema ensured"
        );
        Ok(())
    }

    /// 0007：`issues.archived_at` / `issues.archive_restored_at` + 治理读路径索引 + `ingest_metrics`
    /// （契约 C1.1 / C1.4；`ensure_schema` 逐条 autocommit + 存在性探测，幂等）。
    pub async fn ensure_issue_archive_schema(&self) -> Result<TraceSchemaReport, sqlx::Error> {
        self.ensure_schema(include_str!(
            "../../../migrations/postgres/0007_issue_archive.sql"
        ))
        .await
    }

    /// 落实 0005 schema（幂等、逐条 autocommit；`amend v1.4-6` / L3B-F4）。
    ///
    /// 与「整段 `raw_sql`」的区别：每条语句独立提交，长 DDL 不会合并成一次超长 ACCESS EXCLUSIVE；
    /// 已存在的列/索引直接跳过（不再每次启动重放 `ADD COLUMN IF NOT EXISTS` 取表锁）；
    /// 索引用 `CREATE INDEX CONCURRENTLY`（不在事务内，允许写入并行）。
    /// 返回逐类计数，供启动日志与测试断言（`TraceSchemaReport`）。
    pub async fn ensure_trace_v2_schema(&self) -> Result<TraceSchemaReport, sqlx::Error> {
        self.ensure_schema(include_str!("../../../migrations/postgres/0005_trace_turns.sql"))
            .await
    }

    /// 0006：`trace_turns.run_id` 列 + 索引（`amend v1.5-6` / W-04）。
    pub async fn ensure_trace_turn_run_id_schema(&self) -> Result<TraceSchemaReport, sqlx::Error> {
        self.ensure_schema(include_str!(
            "../../../migrations/postgres/0006_trace_turn_run_id.sql"
        ))
        .await
    }

    /// 逐条 autocommit 落实一份迁移文件（见 [`Self::ensure_trace_v2_schema`] 的规则）。
    async fn ensure_schema(&self, sql: &str) -> Result<TraceSchemaReport, sqlx::Error> {
        let mut report = TraceSchemaReport::default();
        for statement in schema_statements(sql) {
            let upper = statement.to_ascii_uppercase();
            if upper.starts_with("ALTER TABLE") && upper.contains("ADD COLUMN") {
                match add_column_target(&statement) {
                    Some((table, column)) => {
                        if self.column_exists(&table, &column).await? {
                            report.columns_skipped += 1;
                            continue;
                        }
                        sqlx::query(&statement).execute(&self.pool).await?;
                        report.columns_added += 1;
                    }
                    None => {
                        // 解析失败不静默吞：按原语句执行，让 DB 报错暴露契约漂移。
                        sqlx::query(&statement).execute(&self.pool).await?;
                    }
                }
                continue;
            }
            if upper.starts_with("CREATE INDEX") || upper.starts_with("CREATE UNIQUE INDEX") {
                match index_name_of(&statement) {
                    Some(index) => {
                        if self.index_exists(&index).await? {
                            report.indexes_skipped += 1;
                            continue;
                        }
                        let concurrent = statement.replacen(
                            "CREATE INDEX",
                            "CREATE INDEX CONCURRENTLY",
                            1,
                        );
                        match sqlx::query(&concurrent).execute(&self.pool).await {
                            Ok(_) => report.indexes_created += 1,
                            Err(error) => {
                                // CONCURRENTLY 失败可能留下 invalid 索引：清理后重试一次，仍失败则上抛。
                                tracing::warn!(
                                    index = %index,
                                    error = %error,
                                    "CREATE INDEX CONCURRENTLY 失败，清理后重试一次"
                                );
                                let drop_sql =
                                    format!("DROP INDEX CONCURRENTLY IF EXISTS {index}");
                                let _ = sqlx::query(&drop_sql).execute(&self.pool).await;
                                sqlx::query(&concurrent).execute(&self.pool).await?;
                                report.indexes_created += 1;
                            }
                        }
                    }
                    None => {
                        sqlx::query(&statement).execute(&self.pool).await?;
                    }
                }
                continue;
            }
            // CREATE TABLE IF NOT EXISTS 等幂等语句：直接执行（已存在时为 catalog no-op）。
            sqlx::query(&statement).execute(&self.pool).await?;
        }
        Ok(report)
    }

    /// 列是否存在（按当前 schema 解析，兼容测试的隔离 search_path）。
    async fn column_exists(&self, table: &str, column: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT 1 AS ok FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = $1 AND column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// 索引是否已存在（`pg_indexes`，按当前 schema）。
    async fn index_exists(&self, index: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT 1 AS ok FROM pg_indexes
             WHERE schemaname = current_schema() AND indexname = $1",
        )
        .bind(index)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// 存量回填（幂等）：仅扫描「`trace_turns` 行数 < `payload->'turns'` 条数」的聚合行。
    ///
    /// 正常情况下启动扫描为 0 行；对命中的行按 turn_id 补齐缺失轮次并刷新聚合列。
    async fn backfill_trace_turns(&self) -> Result<(), sqlx::Error> {
        let rows = sqlx::query(
            "SELECT session_id, payload::text AS payload FROM traces
             WHERE jsonb_typeof(payload -> 'turns') = 'array'
               AND jsonb_array_length(payload -> 'turns') > turns_count",
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
        let row = sqlx::query(&format!("SELECT {ISSUE_COLUMNS} FROM issues WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| RepositoryError::NotFound(id.to_string()))?;
        Ok(issue_from_row(&row))
    }

    /// 幂等命中路径：按 event_key 取既有事件与其归属 issue（不触碰计数与时间戳）。
    async fn duplicate_hit(
        &self,
        event_key: &str,
    ) -> Result<Option<UpsertDuplicate>, RepositoryError> {
        let row = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE event_key = $1"
        ))
        .bind(event_key)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let event = event_from_row(&row)?;
        let issue_id = event.issue_id.clone();
        Ok(Some(UpsertDuplicate { issue_id, event }))
    }
}

/// 幂等命中时待回填的既有行（issue 由调用方按 id 查询）。
struct UpsertDuplicate {
    issue_id: String,
    event: Event,
}

fn issue_from_row(row: &sqlx::postgres::PgRow) -> Issue {
    let status_str: String = row.get("status");
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
        first_seen_at: row.get("first_seen_at"),
        last_seen_at: row.get("last_seen_at"),
        // 0007：归档列（PG `TIMESTAMPTZ` → `Option<DateTime<Utc>>`）。
        archived_at: row.get("archived_at"),
        archive_restored_at: row.get("archive_restored_at"),
    }
}

fn event_from_row(row: &sqlx::postgres::PgRow) -> Result<Event, RepositoryError> {
    // try_get（而非 get）把解码失败归一为 RepositoryError：单条事件解码异常只影响该请求
    // （500 persistence_error），绝不允许 panic 打断连接（历史上正是这样丢掉 duplicate 响应、
    // 让消费侧把 200 幂等命中当成传输故障重试）。
    let payload_text: String = row.try_get("payload")?;
    let payload: serde_json::Value =
        serde_json::from_str(&payload_text).unwrap_or_else(|_| serde_json::json!({}));
    Ok(Event {
        id: row.get("id"),
        issue_id: row.get("issue_id"),
        payload,
        release: row.get("release"),
        environment: row.get("environment"),
        event_key: row.get("event_key"),
        level: row.get("level"),
        created_at: row.get("created_at"),
    })
}

/// PG 的 `TIMESTAMPTZ` 精度是微秒，而 `chrono::Utc::now()` 带纳秒。
/// 写入前统一截断到微秒，保证「本次响应返回的时间 == 再次读取的时间」——
/// 否则客户端会看到「没有重复投递，last_seen_at 却变了」的假漂移。
/// `amend v1.4-6`（L3B-F4）：0005 schema 落地报告（逐条 autocommit 执行的统计）。
///
/// `columns_added/columns_skipped` 对应 `traces` 三个新列；`indexes_created/indexes_skipped`
/// 对应 0005 的五个索引。二次启动应为 `added=0, created=0`（证明未重放取锁 DDL）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceSchemaReport {
    pub columns_added: u32,
    pub columns_skipped: u32,
    pub indexes_created: u32,
    pub indexes_skipped: u32,
}

/// 按 `;` 拆分迁移文件为独立语句（先剔除 `--` 行注释；本文件不含字符串内分号）。
fn schema_statements(sql: &str) -> Vec<String> {
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

/// 解析 `ALTER TABLE <table> ADD COLUMN [IF NOT EXISTS] <column>`。
fn add_column_target(statement: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = statement.split_whitespace().collect();
    let table = tokens.get(2)?.to_string();
    let column_keyword = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("COLUMN"))?;
    let mut index = column_keyword + 1;
    if tokens
        .get(index)
        .is_some_and(|token| token.eq_ignore_ascii_case("IF"))
    {
        index += 3; // IF NOT EXISTS
    }
    Some((table, tokens.get(index)?.to_string()))
}

/// 解析 `CREATE [UNIQUE] INDEX [CONCURRENTLY] [IF NOT EXISTS] <name> ...`。
fn index_name_of(statement: &str) -> Option<String> {
    let tokens: Vec<&str> = statement.split_whitespace().collect();
    let index_keyword = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("INDEX"))?;
    let mut index = index_keyword + 1;
    if tokens
        .get(index)
        .is_some_and(|token| token.eq_ignore_ascii_case("CONCURRENTLY"))
    {
        index += 1;
    }
    if tokens
        .get(index)
        .is_some_and(|token| token.eq_ignore_ascii_case("IF"))
    {
        index += 3; // IF NOT EXISTS
    }
    Some(tokens.get(index)?.to_string())
}

fn to_micros(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(dt.timestamp_micros()).unwrap_or(dt)
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
impl IssueRepository for PgIssueRepository {
    async fn record_event_and_upsert_issue(
        &self,
        record: NewEventRecord<'_>,
    ) -> Result<crate::repository::UpsertIssueResult, RepositoryError> {
        let now = to_micros(Utc::now());
        let occurred_at: DateTime<Utc> = to_micros(record.occurred_at.unwrap_or(now));
        let environment = normalize_environment(record.environment);

        // 1. 幂等前置检查（契约 R4.1）：命中即不产生新 events 行、不动计数与时间戳。
        if let Some(key) = record.event_key {
            if let Some(hit) = self.duplicate_hit(key).await? {
                let issue = self.issue_by_id(&hit.issue_id).await?;
                return Ok(duplicate_result(issue, hit.event));
            }
        }

        let mut tx = self.pool.begin().await?;

        let existing = sqlx::query(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issues WHERE fingerprint = $1 AND environment = $2"
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
                    "UPDATE issues SET count = $1, status = $2, last_release = $3, last_seen_at = $4 WHERE id = $5"
                )
                .bind(new_count)
                .bind(status.to_string())
                .bind(record.release)
                .bind(last_seen_at)
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
                    "UPDATE issues SET app = $1, namespace = $2, component = $3, level = $4, source_kind = $5 WHERE id = $6",
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
                        "UPDATE issues SET archived_at = NULL, archive_restored_at = NULL WHERE id = $1",
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
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
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
                .bind(occurred_at)
                .bind(occurred_at)
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
                }
            }
        };

        let event_id = Uuid::new_v4().to_string();
        let insert_result = sqlx::query(
            "INSERT INTO events (id, issue_id, payload, release, environment, event_key, level, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&event_id)
        .bind(&issue.id)
        .bind(&record.payload)
        .bind(record.release)
        .bind(&environment)
        .bind(record.event_key)
        .bind(record.level)
        .bind(occurred_at)
        .execute(&mut *tx)
        .await;

        if let Err(err) = insert_result {
            // 并发同 event_key：唯一索引兜底，归一为幂等命中（契约 R4.2，不得 500）。
            if is_unique_violation(&err) {
                tx.rollback().await?;
                if let Some(key) = record.event_key {
                    if let Some(hit) = self.duplicate_hit(key).await? {
                        let issue = self.issue_by_id(&hit.issue_id).await?;
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
            payload: record.payload,
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

    async fn get_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        self.issue_by_id(id).await
    }

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, RepositoryError> {
        // 过滤子句与 `count_issues` / stats 聚合共用同一构造器（`C3.3` 要求「同 WHERE」）。
        let clause = crate::repository::issue_filter_clause(&filter, "", |index| format!("${index}"));
        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let offset = filter.offset.unwrap_or(0).max(0);
        let sql = format!(
            "SELECT {ISSUE_COLUMNS} FROM issues{} ORDER BY last_seen_at DESC LIMIT ${} OFFSET ${}",
            clause.clause,
            clause.binds.len() + 1,
            clause.binds.len() + 2
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

        sqlx::query("UPDATE issues SET status = $1, assigned_to = $2 WHERE id = $3")
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
        let rows = sqlx::query(
            "SELECT id, issue_id, payload::text AS payload, release, environment, event_key, level, created_at \
             FROM events WHERE issue_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(issue_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(event_from_row).collect()
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
        let batch_size = batch_size.max(1);
        let deleted = sqlx::query(
            "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE created_at < $1 LIMIT $2)",
        )
        .bind(cutoff)
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
        let event_rows = sqlx::query(
            "SELECT id, issue_id, payload::text AS payload, release, environment, event_key, level, created_at FROM events",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut events = Vec::with_capacity(event_rows.len());
        for row in &event_rows {
            events.push(event_from_row(row)?);
        }

        let issue_rows = sqlx::query(&format!("SELECT {ISSUE_COLUMNS} FROM issues"))
            .fetch_all(&self.pool)
            .await?;
        let mut issues = Vec::with_capacity(issue_rows.len());
        for row in &issue_rows {
            issues.push(issue_from_row(row));
        }

        let plan = plan_reindex(&events, &issues, fingerprint_of);

        let mut tx = self.pool.begin().await?;
        for row in &plan.issue_rows {
            if plan.reused_ids.contains(&row.id) {
                sqlx::query(
                    "UPDATE issues SET fingerprint = $1, title = $2, culprit = $3, platform = $4, status = $5, assigned_to = $6, count = $7, last_release = $8, project = $9, environment = $10, app = $11, namespace = $12, component = $13, level = $14, source_kind = $15, first_seen_at = $16, last_seen_at = $17 WHERE id = $18",
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
                .bind(row.first_seen_at)
                .bind(row.last_seen_at)
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, environment, app, namespace, component, level, source_kind, first_seen_at, last_seen_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
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
                .bind(row.first_seen_at)
                .bind(row.last_seen_at)
                .execute(&mut *tx)
                .await?;
            }
        }

        for (event_id, issue_id) in &plan.assignments {
            sqlx::query("UPDATE events SET issue_id = $1 WHERE id = $2")
                .bind(issue_id)
                .bind(event_id)
                .execute(&mut *tx)
                .await?;
        }

        for id in &plan.delete_ids {
            sqlx::query("DELETE FROM issues WHERE id = $1")
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
impl TraceRepository for PgIssueRepository {
    async fn record_trace(&self, trace: TraceRecord) -> Result<TraceRecord, RepositoryError> {
        // C3：事务 + advisory 锁串行化同会话写入，杜绝并发 read-modify-write 丢轮次（修 W-02）；
        // 事务结束自动释放锁。
        // amend v1.4-8（L3A-F8）：锁键用 `hashtextextended(session_id, 0)`（64 位）替代 32 位
        // `hashtext`，把碰撞概率从 2^-32 压到 2^-64；retention 路径使用**同一把**锁（见
        // `delete_traces_older_than`），二者必须保持一致，否则无法互斥。
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&trace.session_id)
            .execute(&mut *tx)
            .await?;
        let record = record_trace_locked(&mut tx, trace).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn get_trace(&self, id: &str) -> Result<TraceRecord, RepositoryError> {
        let row = sqlx::query(&format!("SELECT {TRACE_COLUMNS} FROM traces WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| RepositoryError::TraceNotFound(id.to_string()))?;

        Self::trace_from_row(row)
    }

    async fn list_traces(&self, filter: TraceFilter) -> Result<Vec<TraceRecord>, RepositoryError> {
        let (clause, binds, from, to) = pg_trace_filter_clause(&filter);
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
        // 时间窗按 `DateTime<Utc>` 绑定（TIMESTAMPTZ 语义，避免文本比较）。
        if let Some(from) = from {
            q = q.bind(to_micros(from));
        }
        if let Some(to) = to {
            q = q.bind(to_micros(to));
        }
        let rows = q.fetch_all(&self.pool).await?;
        let mut traces = Vec::new();
        for row in rows {
            traces.push(Self::trace_from_row(row)?);
        }
        Ok(traces)
    }

    async fn count_traces(&self, filter: TraceFilter) -> Result<i64, RepositoryError> {
        let (clause, binds, from, to) = pg_trace_filter_clause(&filter);
        let query = format!("SELECT COUNT(*) AS n FROM traces{clause}");
        let mut q = sqlx::query(&query);
        for b in binds {
            q = q.bind(b);
        }
        if let Some(from) = from {
            q = q.bind(to_micros(from));
        }
        if let Some(to) = to {
            q = q.bind(to_micros(to));
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
        let now = to_micros(Utc::now());
        // `note` 为 None 时保留库内旧值（COALESCE），Some("") 则显式清空。
        let rows_affected = sqlx::query(
            "UPDATE traces SET eval_status = $1, note = COALESCE($2, note), updated_at = $3
             WHERE id = $4",
        )
        .bind(status.to_string())
        .bind(note)
        .bind(now)
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
        let cutoff = to_micros(cutoff);
        // C6：保留期按 `COALESCE(updated_at, created_at)` 判定（新活跃行不因首写时间早被误删）。
        //
        // amend v1.4-3 / v1.4-8（L3A-F4 / L3A-F8）：retention 与 ingest 共用**同一把**
        // `pg_advisory_xact_lock(hashtextextended(session_id, 0))`，并严格按 id 升序取锁：
        //   1) 快照 `ORDER BY id LIMIT n` 固定候选 id 集（此时不取行锁，避免与 ingest 形成
        //      「retention: 行锁→advisory / ingest: advisory→行锁」的反向加锁序而死锁）；
        //   2) 按 id 升序逐个 session 取 advisory 锁（与 ingest 同键 ⇒ 同会话互斥）；
        //   3) 持锁后对每个 id 用 `FOR UPDATE SKIP LOCKED` 复检过期性（等待期间可能被 ingest
        //      刷新成活跃行），只删仍然过期的行；
        //   4) 先按固定 session 集删 `trace_turns`，再按固定 id 集删 `traces`，同事务提交。
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, session_id FROM traces
             WHERE COALESCE(updated_at, created_at) < $1
             ORDER BY id ASC
             LIMIT $2",
        )
        .bind(cutoff)
        .bind(batch_size)
        .fetch_all(&mut *tx)
        .await?;
        let mut candidates: Vec<(String, String)> = rows
            .iter()
            .map(|row| {
                (
                    row.get::<String, _>("id"),
                    row.get::<String, _>("session_id"),
                )
            })
            .collect();
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        if candidates.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }

        let mut deletable_ids: Vec<String> = Vec::new();
        let mut deletable_sessions: Vec<String> = Vec::new();
        for (id, session_id) in &candidates {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
            let still_stale = sqlx::query(
                "SELECT 1 AS ok FROM traces
                 WHERE id = $1 AND COALESCE(updated_at, created_at) < $2
                 FOR UPDATE SKIP LOCKED",
            )
            .bind(id)
            .bind(cutoff)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            if still_stale {
                deletable_ids.push(id.clone());
                deletable_sessions.push(session_id.clone());
            }
        }
        if deletable_ids.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }

        sqlx::query("DELETE FROM trace_turns WHERE session_id = ANY($1)")
            .bind(&deletable_sessions)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM traces WHERE id = ANY($1)")
            .bind(&deletable_ids)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(deleted)
    }
}

/// `pg_trace_filter_clause` 的返回类型：(SQL 子句, 文本 binds, 时间窗下限, 时间窗上限)。
type PgTraceFilterClause = (String, Vec<String>, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// 拼接 `traces` 过滤子句（`list_traces` / `count_traces` 共用，保证口径一致）。
///
/// 返回 `(clause, 文本 binds, from, to)`：时间窗最后以 `DateTime<Utc>` 绑定（TIMESTAMPTZ）。
/// `run_id` 语义（`amend v1.5-6` / W-04）：命中 `traces.run_id` **或**其任一轮次的
/// `trace_turns.run_id`。
fn pg_trace_filter_clause(filter: &TraceFilter) -> PgTraceFilterClause {
    let mut clause = " WHERE 1=1".to_string();
    let mut binds: Vec<String> = Vec::new();
    let mut bind_index = 1;
    // 宏而非闭包：需要同时修改 `bind_index` 与两个入参（借用检查无法用闭包表达）。
    macro_rules! push {
        ($clause:expr, $binds:expr, $value:expr, $predicate:expr) => {{
            $clause.push_str(&format!(" AND {} = ${}", $predicate, bind_index));
            $binds.push($value);
            bind_index += 1;
        }};
    }
    if let Some(session_id) = &filter.session_id {
        push!(clause, binds, session_id.clone(), "session_id");
    }
    if let Some(eval_status) = &filter.eval_status {
        push!(clause, binds, eval_status.to_string(), "eval_status");
    }
    if let Some(environment) = &filter.environment {
        push!(clause, binds, environment.clone(), "environment");
    }
    if let Some(release) = &filter.release {
        push!(clause, binds, release.clone(), "release");
    }
    if let Some(project) = &filter.project {
        push!(clause, binds, project.clone(), "project");
    }
    if let Some(run_id) = &filter.run_id {
        clause.push_str(&format!(
            " AND (run_id = ${bind_index} OR EXISTS (SELECT 1 FROM trace_turns AS tt
                 WHERE tt.session_id = traces.session_id AND tt.run_id = ${next}))",
            next = bind_index + 1
        ));
        binds.push(run_id.clone());
        binds.push(run_id.clone());
        bind_index += 2;
    }
    if let Some(turn_status) = &filter.turn_status {
        push!(clause, binds, turn_status.clone(), "turn_status");
    }
    if filter.from.is_some() {
        clause.push_str(&format!(" AND reported_at >= ${bind_index}"));
        bind_index += 1;
    }
    if filter.to.is_some() {
        clause.push_str(&format!(" AND reported_at <= ${bind_index}"));
    }
    (clause, binds, filter.from, filter.to)
}

/// `traces` 行读取列（对齐 0003 + 0005 新增的 `turn_status` / `turns_count` / `note`）。
///
/// `payload` 必须 `::text`（JSONB → TEXT），与 events 读取路径同约定。
const TRACE_COLUMNS: &str = "id, session_id, run_id, turn_id, environment, release, project, \
     eval_status, payload::text AS payload, total_input_tokens, total_output_tokens, \
     total_duration_ms, turn_status, turns_count, note, reported_at, created_at, updated_at";

/// 事务内完成一次上报写入（契约 `C2` / `C3` / `C4`）：
///
/// 1. 读旧聚合行（供 `extra`/`tags` 深合并与 `eval_status`/`note` 保留）；
/// 2. 逐 turn `merge_turn`（终态优先）→ `annotate_turn`（诊断标注）→ upsert `trace_turns`；
/// 3. 按 `seq` 读回全部 turn → `merge_top_level` + `aggregate_session`（token 单一真相）；
/// 4. upsert `traces` 聚合行并**回查库内真实行**返回（响应与库一致，修 W-08）。
async fn record_trace_locked(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    trace: TraceRecord,
) -> Result<TraceRecord, RepositoryError> {
    let now = to_micros(Utc::now());
    let session_id = trace.session_id.clone();
    // `amend v1.5-4`（I-02）：就地归一 turns + 单 tool_call 超长文本截断；warnings 写入 `payload._warnings`。
    let mut incoming_top = trace.payload.clone();
    let turn_warnings = crate::trace_v2::normalize_incoming_turns(&mut incoming_top);
    // `amend v1.6-4`：先脱敏（routes 侧）→ 再截断（此处）；截断只作用于已脱敏文本。
    crate::trace_v2::truncate_turns_tool_text(&mut incoming_top);
    let existing = fetch_trace_by_session(&mut *tx, &session_id).await?;
    let old_payload = existing
        .as_ref()
        .map(|row| row.payload.clone())
        .unwrap_or(serde_json::Value::Null);

    let incoming_turns = incoming_top
        .get("turns")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    // 覆盖度兜底（amend v1.4-1）：`trace_turns` 行数 < payload.turns 条数时按 turn_id 补齐，
    // 否则本次聚合只看得见新 turn，会把缺失的历史轮次整体丢掉。
    hydrate_trace_turns(&mut *tx, &session_id, &old_payload).await?;

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
    conn: &mut sqlx::PgConnection,
    session_id: &str,
) -> Result<Option<TraceRecord>, RepositoryError> {
    let row = sqlx::query(&format!(
        "SELECT {TRACE_COLUMNS} FROM traces WHERE session_id = $1"
    ))
    .bind(session_id)
    .fetch_optional(&mut *conn)
    .await?;
    row.map(PgIssueRepository::trace_from_row).transpose()
}

/// 按覆盖度补齐 `trace_turns`（amend v1.4-1 / L3A-F3）：
/// 仅当「已入库行数 < `payload.turns` 条数」时，用与运行时**同一套** `trace_v2::turn_keys_for`
/// （显式 turn_id 优先，缺失时内容 sha256 生成 `turn_srv_*` 并对同批内容碰撞加序号后缀）逐条补齐；
/// 已存在的键跳过（幂等，不覆盖库内更新的轮次）。
async fn hydrate_trace_turns(
    conn: &mut sqlx::PgConnection,
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
    let rows = sqlx::query("SELECT turn_id, seq FROM trace_turns WHERE session_id = $1")
        .bind(session_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut used_seqs: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for row in &rows {
        existing.insert(row.get::<String, _>("turn_id"));
        used_seqs.insert(i64::from(row.get::<i32, _>("seq")));
    }
    // 覆盖度判据：行数达到 payload.turns 条数即视为完整（不再只判"有行"）。
    if existing.len() >= legacy_turns.len() {
        return Ok(());
    }
    let keys = crate::trace_v2::turn_keys_for(&legacy_turns);
    let now = to_micros(Utc::now());
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
    conn: &mut sqlx::PgConnection,
    session_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE traces SET
             turns_count = (SELECT COUNT(*) FROM trace_turns WHERE session_id = $1),
             turn_status = COALESCE((
                 SELECT status FROM trace_turns WHERE session_id = $1
                 ORDER BY seq DESC LIMIT 1), 'unknown')
         WHERE session_id = $1",
    )
    .bind(session_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 读某轮已入库的 `(seq, payload)`；不存在返回 `None`。
async fn fetch_trace_turn(
    conn: &mut sqlx::PgConnection,
    session_id: &str,
    turn_id: &str,
) -> Result<Option<(i64, serde_json::Value)>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT seq, payload::text AS payload FROM trace_turns
         WHERE session_id = $1 AND turn_id = $2",
    )
    .bind(session_id)
    .bind(turn_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let seq: i32 = row.get("seq");
    let payload: String = row.get("payload");
    let payload = serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
    Ok(Some((i64::from(seq), payload)))
}

/// `C2.5`：新 turn 的 `seq = MAX(seq) + 1`（会话内从 0 起单调）。
async fn next_turn_seq(conn: &mut sqlx::PgConnection, session_id: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(seq) + 1, 0)::bigint AS next_seq FROM trace_turns
         WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.get("next_seq"))
}

/// 写入/合并单条 turn（`seq` 与 `created_at` 在冲突时保持不变，保证顺序稳定）。
#[allow(clippy::too_many_arguments)]
async fn store_trace_turn(
    conn: &mut sqlx::PgConnection,
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
         VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $8)
         ON CONFLICT (session_id, turn_id) DO UPDATE SET
             status = excluded.status,
             run_id = COALESCE(excluded.run_id, trace_turns.run_id),
             payload = excluded.payload,
             updated_at = excluded.updated_at",
    )
    .bind(session_id)
    .bind(turn_id)
    .bind(seq as i32)
    .bind(status)
    .bind(run_id)
    .bind(payload)
    .bind(now)
    .bind(now)
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
    conn: &mut sqlx::PgConnection,
    session_id: &str,
) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT payload::text AS payload FROM trace_turns
         WHERE session_id = $1 ORDER BY seq ASC",
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
    conn: &mut sqlx::PgConnection,
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
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14, $15, $16, $17, $18)
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
    .bind(turns_count as i32)
    .bind(&trace.note)
    .bind(to_micros(trace.reported_at))
    .bind(to_micros(trace.created_at))
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

impl PgIssueRepository {
    fn trace_from_row(row: sqlx::postgres::PgRow) -> Result<TraceRecord, RepositoryError> {
        use sqlx::Row;

        let eval_status_str: String = row.get("eval_status");
        let eval_status = eval_status_str
            .parse::<EvalStatus>()
            .unwrap_or(EvalStatus::Unreviewed);
        // 调用方的 SQL 必须写 `payload::text AS payload`（JSONB → TEXT），与 events 同约定。
        let payload_str: String = row.try_get("payload")?;
        let payload: serde_json::Value =
            serde_json::from_str(&payload_str).unwrap_or_else(|_| serde_json::json!({}));

        Ok(TraceRecord {
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
            // `turns_count` 列是 INTEGER(INT4)：必须按 i32 解码再拓宽（i64 解码会静默失败）。
            turns_count: row
                .try_get::<i32, _>("turns_count")
                .map(i64::from)
                .unwrap_or(0),
            note: row.try_get("note").unwrap_or(None),
            reported_at: row.get("reported_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
}

// ============================================================================
// 契约 `.dev-team/contract-issue-governance.md`（FROZEN **v1.2**）治理能力：PostgreSQL 实现
//
// 口径单一真相：`list_issues` / `count_issues` / stats 聚合共用
// `repository::issue_filter_clause`，因此「列表 total」与「统计 issues.total」不可能漂移。
// ============================================================================
#[async_trait]
impl IssueGovernanceRepository for PgIssueRepository {
    async fn count_issues(&self, filter: IssueFilter) -> Result<i64, RepositoryError> {
        let clause = crate::repository::issue_filter_clause(&filter, "", |index| format!("${index}"));
        pg_scalar(&self.pool, &format!("SELECT COUNT(*) AS n FROM issues{}", clause.clause), &clause.binds, None)
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
            "UPDATE issues SET archived_at = COALESCE(archived_at, $2) WHERE id = $1",
        )
        .bind(id)
        .bind(to_micros(archived_at))
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(RepositoryError::NotFound(id.to_string()));
        }
        self.issue_by_id(id).await
    }

    async fn unarchive_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        // 同语句写 `archived_at = NULL` + `archive_restored_at = now`（`amend v1.1 A-1`，
        // 恢复留痕单调前移；未归档行同样刷新，供自动 TTL 抑制项判定）。
        let affected = sqlx::query(
            "UPDATE issues SET archived_at = NULL, archive_restored_at = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(to_micros(Utc::now()))
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
        let now = to_micros(now);
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
            // ---- `ids` 模式（`amend v1.1 A-3` + `v1.2`）：ids 定候选集，statuses 仍是白名单约束 ----
            Some(ids) => {
                outcome.matched = ids.len() as i64;
                let mut tx = self.pool.begin().await?;
                let rows = sqlx::query("SELECT id, status, archived_at FROM issues WHERE id = ANY($1)")
                    .bind(&ids)
                    .fetch_all(&mut *tx)
                    .await?;
                let mut found: HashMap<String, (String, bool)> = HashMap::with_capacity(rows.len());
                for row in &rows {
                    found.insert(
                        row.get::<String, _>("id"),
                        (
                            row.get::<String, _>("status"),
                            row.get::<Option<DateTime<Utc>>, _>("archived_at").is_some(),
                        ),
                    );
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
                    let updated = sqlx::query(
                        "UPDATE issues SET archived_at = $1
                         WHERE id = ANY($2) AND status = ANY($3) AND archived_at IS NULL",
                    )
                    .bind(now)
                    .bind(&ids)
                    .bind(&statuses)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                    outcome.archived = updated as i64;
                }
                tx.commit().await?;
            }
            // ---- `statuses`/`project` 模式：`matched ≡ archived`，`skipped_*` 恒 0 ----
            None => {
                let cutoff = request
                    .older_than_days
                    .map(|days| now - chrono::Duration::days(days.max(0)));
                let project_bound = project.is_some();
                if dry_run {
                    // 只统计：占位符 $1 = statuses，$2 = project（可选），$3/$2 = cutoff（可选）。
                    let project_idx = project_bound.then_some(2usize);
                    let older_idx = cutoff.map(|_| if project_bound { 3 } else { 2 });
                    let count_sql = format!(
                        "SELECT COUNT(*) AS n FROM issues WHERE {}",
                        pg_status_filter_clause(1, project_idx, older_idx)
                    );
                    let mut query = sqlx::query(&count_sql).bind(&statuses);
                    if let Some(project) = &project {
                        query = query.bind(project.clone());
                    }
                    if let Some(cutoff) = cutoff {
                        query = query.bind(to_micros(cutoff));
                    }
                    let row = query.fetch_one(&self.pool).await?;
                    outcome.matched = row.get::<i64, _>("n");
                    outcome.archived = 0;
                } else {
                    // $1 = now，$2 = statuses，$3 = project（可选），$4/$3 = cutoff（可选）。
                    let project_idx = project_bound.then_some(3usize);
                    let older_idx = cutoff.map(|_| if project_bound { 4 } else { 3 });
                    let update_sql = format!(
                        "UPDATE issues SET archived_at = $1 WHERE {}",
                        pg_status_filter_clause(2, project_idx, older_idx)
                    );
                    let mut query = sqlx::query(&update_sql).bind(now).bind(&statuses);
                    if let Some(project) = &project {
                        query = query.bind(project.clone());
                    }
                    if let Some(cutoff) = cutoff {
                        query = query.bind(to_micros(cutoff));
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
        let updated = sqlx::query(
            "UPDATE issues SET archived_at = $1
             WHERE status = ANY($2) AND archived_at IS NULL AND last_seen_at < $3
               AND (archive_restored_at IS NULL OR archive_restored_at < last_seen_at)",
        )
        .bind(to_micros(Utc::now()))
        .bind(&statuses)
        .bind(to_micros(cutoff))
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(updated)
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
        // `amend v1.1 B-2`：游标携带库内原始 RFC3339 串；PG 侧按 `timestamptz` 原生时间比较。
        let cursor_at = cursor.as_ref().map(|payload| {
            DateTime::parse_from_rfc3339(&payload.created_at)
                .map(|value| to_micros(value.with_timezone(&Utc)))
                .unwrap_or(DateTime::<Utc>::MAX_UTC)
        });
        let mut sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE issue_id = $1");
        if cursor_at.is_some() {
            sql.push_str(&format!(
                " AND (created_at {comparator} $2 OR (created_at = $2 AND id {comparator} $3))"
            ));
        }
        sql.push_str(&format!(
            " ORDER BY created_at {direction}, id {direction} LIMIT ${}",
            if cursor_at.is_some() { 4 } else { 2 }
        ));
        let mut query = sqlx::query(&sql).bind(issue_id);
        if let (Some(at), Some(payload)) = (cursor_at, cursor.as_ref()) {
            query = query.bind(at).bind(&payload.id);
        }
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(event_from_row).collect()
    }

    async fn count_events_for_issue(&self, issue_id: &str) -> Result<i64, RepositoryError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM events WHERE issue_id = $1")
            .bind(issue_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("n"))
    }

    async fn issue_stats(&self, query: &IssueStatsQuery) -> Result<IssueStatsResponse, RepositoryError> {
        let now = to_micros(Utc::now());
        let scope = crate::repository::stats_scope(query, now);
        // ---- issues 口径（受 project/environment/level/include_archived；**不受** from/to，C5.0）----
        let clause = crate::repository::issue_filter_clause(&scope.filter, "", |index| format!("${index}"));
        let total = pg_scalar(
            &self.pool,
            &format!("SELECT COUNT(*) AS n FROM issues{}", clause.clause),
            &clause.binds,
            None,
        )
        .await?;
        let by_status = pg_grouped(
            &self.pool,
            &format!(
                "SELECT status AS k, COUNT(*) AS n FROM issues{} GROUP BY 1",
                clause.clause
            ),
            &clause.binds,
            None,
        )
        .await?;
        let by_level = pg_grouped(
            &self.pool,
            &format!(
                "SELECT level AS k, COUNT(*) AS n FROM issues{} GROUP BY 1",
                clause.clause
            ),
            &clause.binds,
            None,
        )
        .await?;
        let by_environment = pg_grouped(
            &self.pool,
            &format!(
                "SELECT environment AS k, COUNT(*) AS n FROM issues{} GROUP BY 1",
                clause.clause
            ),
            &clause.binds,
            None,
        )
        .await?;
        // `events.total` = SUM(issues.count)（聚合口径，全时段；生产 185）。
        let events_total = pg_scalar(
            &self.pool,
            &format!(
                "SELECT COALESCE(SUM(count), 0)::bigint AS n FROM issues{}",
                clause.clause
            ),
            &clause.binds,
            None,
        )
        .await?;

        // ---- events 行口径（受 from/to；include_archived=false 时只计未归档 issue 的行，C5.0）----
        let row_clause = crate::repository::issue_filter_clause(&scope.filter, "i.", |index| format!("${index}"));
        let time_where = format!(
            " AND e.created_at >= ${} AND e.created_at <= ${}",
            row_clause.binds.len() + 1,
            row_clause.binds.len() + 2
        );
        let row_from = format!(
            " FROM events e JOIN issues i ON i.id = e.issue_id{}{time_where}",
            row_clause.clause
        );
        let window = Some((scope.from, scope.to));
        let stored_rows = pg_scalar(
            &self.pool,
            &format!("SELECT COUNT(*) AS n{row_from}"),
            &row_clause.binds,
            window,
        )
        .await?;
        let ev_by_level = pg_grouped(
            &self.pool,
            &format!("SELECT COALESCE(e.level, 'unknown') AS k, COUNT(*) AS n{row_from} GROUP BY 1"),
            &row_clause.binds,
            window,
        )
        .await?;
        let ev_by_environment = pg_grouped(
            &self.pool,
            &format!(
                "SELECT COALESCE(e.environment, 'unknown') AS k, COUNT(*) AS n{row_from} GROUP BY 1"
            ),
            &row_clause.binds,
            window,
        )
        .await?;
        let ev_by_release = pg_grouped(
            &self.pool,
            &format!("SELECT COALESCE(e.release, 'unknown') AS k, COUNT(*) AS n{row_from} GROUP BY 1"),
            &row_clause.binds,
            window,
        )
        .await?;

        // ---- series（空桶补 0；`sum(series.events) == stored_rows` 由同 WHERE 保证）----
        let issue_bucket = pg_bucket_prefix("first_seen_at", scope.bucket);
        let issue_series = pg_grouped(
            &self.pool,
            &format!(
                "SELECT {issue_bucket} AS k, COUNT(*) AS n FROM issues{} \
                 AND first_seen_at >= ${} AND first_seen_at <= ${} GROUP BY 1",
                clause.clause,
                clause.binds.len() + 1,
                clause.binds.len() + 2
            ),
            &clause.binds,
            window,
        )
        .await?;
        let event_bucket = pg_bucket_prefix("e.created_at", scope.bucket);
        let event_series = pg_grouped(
            &self.pool,
            &format!("SELECT {event_bucket} AS k, COUNT(*) AS n{row_from} GROUP BY 1"),
            &row_clause.binds,
            window,
        )
        .await?;

        let mut series: Vec<StatsSeriesPoint> = Vec::new();
        let issue_series = rekey_buckets(issue_series, scope.bucket);
        let event_series = rekey_buckets(event_series, scope.bucket);
        for key in crate::repository::bucket_keys(scope.from, scope.to, scope.bucket) {
            series.push(StatsSeriesPoint {
                issues: issue_series.get(&key).copied().unwrap_or(0),
                events: event_series.get(&key).copied().unwrap_or(0),
                bucket: key,
            });
        }

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
        let now = to_micros(Utc::now());
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

        // 计数按 UTC 日期落库（`bucket_date`），故过滤按日期区间（闭区间）。
        let mut where_clause = " WHERE bucket_date >= $1 AND bucket_date <= $2".to_string();
        if project.is_some() {
            where_clause.push_str(" AND project = $3");
        }
        let from_date: NaiveDate = from.date_naive();
        let to_date: NaiveDate = to.date_naive();

        let outcome_rows = {
            let totals_sql = format!(
                "SELECT outcome AS k, COALESCE(SUM(count), 0)::bigint AS n FROM ingest_metrics{where_clause} GROUP BY 1"
            );
            let mut statement = sqlx::query(&totals_sql).bind(from_date).bind(to_date);
            if let Some(project) = &project {
                statement = statement.bind(project);
            }
            statement.fetch_all(&self.pool).await?
        };
        let mut sums: HashMap<String, i64> = HashMap::new();
        for row in &outcome_rows {
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

        let project_rows = {
            let by_project_sql = format!(
                "SELECT project AS k, \
                 COALESCE(SUM(CASE WHEN outcome = 'received' THEN count ELSE 0 END), 0)::bigint AS received, \
                 COALESCE(SUM(CASE WHEN outcome IN ('created','duplicate') THEN count ELSE 0 END), 0)::bigint AS accepted, \
                 COALESCE(SUM(CASE WHEN outcome = 'suppressed_ignored' THEN count ELSE 0 END), 0)::bigint AS suppressed \
                 FROM ingest_metrics{where_clause} GROUP BY project"
            );
            let mut statement = sqlx::query(&by_project_sql).bind(from_date).bind(to_date);
            if let Some(project) = &project {
                statement = statement.bind(project);
            }
            statement.fetch_all(&self.pool).await?
        };
        let mut by_project: Vec<IngestGovernanceProjectRow> = project_rows
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

        let series_rows = {
            let series_sql = format!(
                "SELECT bucket_date::text AS k, \
                 COALESCE(SUM(CASE WHEN outcome = 'received' THEN count ELSE 0 END), 0)::bigint AS received, \
                 COALESCE(SUM(CASE WHEN outcome IN ('created','duplicate') THEN count ELSE 0 END), 0)::bigint AS accepted, \
                 COALESCE(SUM(CASE WHEN outcome = 'suppressed_ignored' THEN count ELSE 0 END), 0)::bigint AS suppressed \
                 FROM ingest_metrics{where_clause} GROUP BY bucket_date"
            );
            let mut statement = sqlx::query(&series_sql).bind(from_date).bind(to_date);
            if let Some(project) = &project {
                statement = statement.bind(project);
            }
            statement.fetch_all(&self.pool).await?
        };
        let mut day_sums: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
        for row in &series_rows {
            day_sums.insert(
                row.get::<String, _>("k"),
                (row.get("received"), row.get("accepted"), row.get("suppressed")),
            );
        }
        let mut series: Vec<IngestGovernanceSeriesPoint> = Vec::new();
        for key in crate::repository::bucket_keys(from, to, bucket) {
            // 存储粒度为「UTC 日期」，hour 桶时计数只可能落在当日 00:00 桶（其余桶补 0）。
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
        // `C6.2`（`amend v1.1 B-3`）：单语句 UPSERT，每事件一次；失败由调用方 warn，不阻断 ingest。
        sqlx::query(
            "INSERT INTO ingest_metrics (bucket_date, project, outcome, count, updated_at)
             VALUES ($1, $2, $3, 1, now())
             ON CONFLICT (bucket_date, project, outcome)
             DO UPDATE SET count = ingest_metrics.count + 1, updated_at = now()",
        )
        .bind(bucket_date)
        .bind(project)
        .bind(crate::repository::ingest_metric_outcome_str(outcome))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// `statuses`/`project` 模式的条件子句（`UPDATE` 与 `dry_run` 计数共用同一形态，避免口径漂移）。
///
/// 占位符序号由调用方给定（`status_idx` 为 `status = ANY($n)`；project / cutoff 依次顺延）。
fn pg_status_filter_clause(
    status_idx: usize,
    project_idx: Option<usize>,
    older_idx: Option<usize>,
) -> String {
    let mut clause = format!("status = ANY(${status_idx}) AND archived_at IS NULL");
    if let Some(index) = project_idx {
        clause.push_str(&format!(" AND project = ${index}"));
    }
    if let Some(index) = older_idx {
        clause.push_str(&format!(" AND last_seen_at < ${index}"));
    }
    clause
}

/// 标量计数查询（binds 全部为文本；`window` 为可选时间窗，按 `timestamptz` 绑定）。
async fn pg_scalar(
    pool: &PgPool,
    sql: &str,
    binds: &[String],
    window: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> Result<i64, RepositoryError> {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(bind);
    }
    if let Some((from, to)) = window {
        query = query.bind(to_micros(from)).bind(to_micros(to));
    }
    let row = query.fetch_one(pool).await?;
    Ok(row.get::<i64, _>("n"))
}

/// 分组计数查询（`k` 为文本键、`n` 为计数）。
async fn pg_grouped(
    pool: &PgPool,
    sql: &str,
    binds: &[String],
    window: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> Result<BTreeMap<String, i64>, RepositoryError> {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(bind);
    }
    if let Some((from, to)) = window {
        query = query.bind(to_micros(from)).bind(to_micros(to));
    }
    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|row| (row.get::<String, _>("k"), row.get::<i64, _>("n")))
        .collect())
}

/// SQL 侧桶前缀表达式（day = `YYYY-MM-DD`；hour = `YYYY-MM-DDTHH`，均按 UTC 取整）。
fn pg_bucket_prefix(column: &str, bucket: StatsBucket) -> String {
    match bucket {
        StatsBucket::Day => format!("to_char(({column} AT TIME ZONE 'UTC'), 'YYYY-MM-DD')"),
        StatsBucket::Hour => format!(
            "to_char(({column} AT TIME ZONE 'UTC'), 'YYYY-MM-DD') || 'T' || \
             to_char(({column} AT TIME ZONE 'UTC'), 'HH24')"
        ),
    }
}

/// 把 SQL 产出的桶前缀归一为 `series[].bucket` 键（补齐 hour 桶的 `:00:00Z` 后缀）。
fn rekey_buckets(raw: BTreeMap<String, i64>, bucket: StatsBucket) -> BTreeMap<String, i64> {
    raw.into_iter()
        .map(|(prefix, count)| (crate::repository::bucket_key_from_prefix(&prefix, bucket), count))
        .collect()
}
