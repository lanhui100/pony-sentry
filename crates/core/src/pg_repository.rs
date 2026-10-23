use crate::{
    models::{
        EvalStatus, Event, Issue, IssueStatus, IssueStatusEnvironmentCount, TraceFilter,
        TraceRecord,
    },
    reindex::{plan_reindex, FingerprintOut, ReindexSummary},
    repository::{IssueFilter, IssueRepository, NewEventRecord, RepositoryError, TraceRepository},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// issues 表全量列（对齐 `migrations/postgres/0001` + `0002` + `0004`）。
const ISSUE_COLUMNS: &str =
    "id, fingerprint, title, culprit, platform, status, assigned_to, count, \
     last_release, project, environment, app, namespace, component, level, source_kind, \
     first_seen_at, last_seen_at";

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
        // C1.3 / amend v1.4-1：回填在 Rust 侧执行——turn_id 必须与运行时
        // `trace_v2::turn_key_of`（sha256）同构，SQL 方言无 sha256 故不写在迁移文件里。
        self.backfill_trace_turns().await?;
        Ok(())
    }

    /// 落实 0005 schema（幂等、逐条 autocommit；`amend v1.4-6` / L3B-F4）。
    ///
    /// 与「整段 `raw_sql`」的区别：每条语句独立提交，长 DDL 不会合并成一次超长 ACCESS EXCLUSIVE；
    /// 已存在的列/索引直接跳过（不再每次启动重放 `ADD COLUMN IF NOT EXISTS` 取表锁）；
    /// 索引用 `CREATE INDEX CONCURRENTLY`（不在事务内，允许写入并行）。
    /// 返回逐类计数，供启动日志与测试断言（`TraceSchemaReport`）。
    pub async fn ensure_trace_v2_schema(&self) -> Result<TraceSchemaReport, sqlx::Error> {
        let sql = include_str!("../../../migrations/postgres/0005_trace_turns.sql");
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
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(format!(
            "SELECT {ISSUE_COLUMNS} FROM issues WHERE 1=1"
        ));

        if let Some(ref st) = filter.status {
            builder.push(" AND status = ").push_bind(st.to_string());
        }
        if let Some(ref pf) = filter.platform {
            builder.push(" AND platform = ").push_bind(pf);
        }
        if let Some(ref rel) = filter.release {
            builder.push(" AND last_release = ").push_bind(rel);
        }
        if let Some(ref proj) = filter.project {
            builder.push(" AND project = ").push_bind(proj);
        }
        if let Some(ref env) = filter.environment {
            builder.push(" AND environment = ").push_bind(env);
        }
        if let Some(ref level) = filter.level {
            builder.push(" AND level = ").push_bind(level);
        }
        builder.push(" ORDER BY last_seen_at DESC");

        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let offset = filter.offset.unwrap_or(0).max(0);
        builder
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);

        let rows = builder.build().fetch_all(&self.pool).await?;
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
        let mut query = format!("SELECT {TRACE_COLUMNS} FROM traces WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let mut bind_index = 1;

        if let Some(session_id) = &filter.session_id {
            query.push_str(&format!(" AND session_id = ${bind_index}"));
            binds.push(session_id.clone());
            bind_index += 1;
        }
        if let Some(eval_status) = &filter.eval_status {
            query.push_str(&format!(" AND eval_status = ${bind_index}"));
            binds.push(eval_status.to_string());
            bind_index += 1;
        }
        if let Some(environment) = &filter.environment {
            query.push_str(&format!(" AND environment = ${bind_index}"));
            binds.push(environment.clone());
            bind_index += 1;
        }
        if let Some(release) = &filter.release {
            query.push_str(&format!(" AND release = ${bind_index}"));
            binds.push(release.clone());
            bind_index += 1;
        }
        if let Some(project) = &filter.project {
            query.push_str(&format!(" AND project = ${bind_index}"));
            binds.push(project.clone());
            bind_index += 1;
        }
        // C5：新增 run_id / turn_status / 时间窗（按 reported_at，含端点）筛选。
        if let Some(run_id) = &filter.run_id {
            query.push_str(&format!(" AND run_id = ${bind_index}"));
            binds.push(run_id.clone());
            bind_index += 1;
        }
        if let Some(turn_status) = &filter.turn_status {
            query.push_str(&format!(" AND turn_status = ${bind_index}"));
            binds.push(turn_status.clone());
            bind_index += 1;
        }
        if filter.from.is_some() {
            query.push_str(&format!(" AND reported_at >= ${bind_index}"));
            bind_index += 1;
        }
        if filter.to.is_some() {
            query.push_str(&format!(" AND reported_at <= ${bind_index}"));
        }

        query.push_str(" ORDER BY created_at DESC");

        // C5：limit clamp 到 [1,100]；offset 不得为负（HTTP 层负责 400，仓储层兜底）。
        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let offset = filter.offset.unwrap_or(0).max(0);
        query.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));

        let mut q = sqlx::query(&query);
        for b in binds {
            q = q.bind(b);
        }
        // 时间窗按 `DateTime<Utc>` 绑定（TIMESTAMPTZ 语义，避免文本比较）。
        if let Some(from) = filter.from {
            q = q.bind(to_micros(from));
        }
        if let Some(to) = filter.to {
            q = q.bind(to_micros(to));
        }

        let rows = q.fetch_all(&self.pool).await?;
        let mut traces = Vec::new();
        for row in rows {
            traces.push(Self::trace_from_row(row)?);
        }

        Ok(traces)
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
    let incoming_top = trace.payload.clone();
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

    for raw_turn in incoming_turns {
        let incoming = crate::trace_v2::normalize_incoming_turn(&raw_turn);
        let turn_id = crate::trace_v2::turn_key_of(&incoming);
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
        store_trace_turn(&mut *tx, &session_id, &turn_id, seq, &status, &merged, now).await?;
    }

    let turns = list_turn_payloads(&mut *tx, &session_id).await?;
    let mut incoming_top = incoming_top;
    if let Some(object) = incoming_top.as_object_mut() {
        object.insert(
            "session_id_source".to_string(),
            serde_json::Value::String(crate::trace_v2::session_id_source_of(&trace.payload)),
        );
    }
    let top = crate::trace_v2::merge_top_level(&old_payload, &incoming_top);
    let payload = crate::trace_v2::aggregate_session(&turns, &top);
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
        store_trace_turn(conn, session_id, turn_id, seq, &status, &turn, now).await?;
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
async fn store_trace_turn(
    conn: &mut sqlx::PgConnection,
    session_id: &str,
    turn_id: &str,
    seq: i64,
    status: &str,
    payload: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let payload = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    sqlx::query(
        "INSERT INTO trace_turns (session_id, turn_id, seq, status, payload, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7)
         ON CONFLICT (session_id, turn_id) DO UPDATE SET
             status = excluded.status,
             payload = excluded.payload,
             updated_at = excluded.updated_at",
    )
    .bind(session_id)
    .bind(turn_id)
    .bind(seq as i32)
    .bind(status)
    .bind(payload)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
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
