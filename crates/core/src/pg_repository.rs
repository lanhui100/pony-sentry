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
    async fn record_trace(&self, mut trace: TraceRecord) -> Result<TraceRecord, RepositoryError> {
        // TIMESTAMPTZ 微秒归一：保证写回的时间与再次读取一致（同 events 路径）。
        trace.reported_at = to_micros(trace.reported_at);
        trace.created_at = to_micros(trace.created_at);
        trace.updated_at = to_micros(trace.updated_at);
        // 同 session_id 增量上报：先取旧 payload，按 turns 轮次去重合并（旧轮保留、新轮按序追加）。
        // 此处读的是 JSONB 列，解码目标必须同样是 `serde_json::Value`（Value ↔ JSONB）；
        // 与读取接口"`payload::text` → String → serde_json"两条路径各自配对，不得交叉。
        let existing_payload: Option<serde_json::Value> =
            match sqlx::query("SELECT payload FROM traces WHERE session_id = $1")
                .bind(&trace.session_id)
                .fetch_optional(&self.pool)
                .await?
            {
                Some(row) => row.try_get("payload")?,
                None => None,
            };

        let merged_payload = match existing_payload {
            Some(old) => crate::trace_merge::merge_trace_payloads(&old, &trace.payload),
            None => trace.payload.clone(),
        };
        let payload_str =
            serde_json::to_string(&merged_payload).unwrap_or_else(|_| "{}".to_string());

        let mut effective_in_tokens = trace.total_input_tokens;
        let mut effective_out_tokens = trace.total_output_tokens;
        let mut effective_duration = trace.total_duration_ms;

        if let Some(t) = merged_payload
            .get("total_input_tokens")
            .and_then(|v| v.as_i64())
        {
            effective_in_tokens = Some(t);
        }
        if let Some(t) = merged_payload
            .get("total_output_tokens")
            .and_then(|v| v.as_i64())
        {
            effective_out_tokens = Some(t);
        }
        if let Some(t) = merged_payload
            .get("total_duration_ms")
            .and_then(|v| v.as_i64())
        {
            effective_duration = Some(t);
        }

        sqlx::query(
            "INSERT INTO traces (
                id, session_id, run_id, turn_id, environment, release, project,
                eval_status, payload, total_input_tokens, total_output_tokens,
                total_duration_ms, reported_at, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14, $15)
            ON CONFLICT (session_id) DO UPDATE SET
                run_id = EXCLUDED.run_id,
                turn_id = EXCLUDED.turn_id,
                environment = EXCLUDED.environment,
                release = EXCLUDED.release,
                project = EXCLUDED.project,
                eval_status = CASE WHEN traces.eval_status != 'unreviewed' THEN traces.eval_status ELSE EXCLUDED.eval_status END,
                payload = EXCLUDED.payload,
                total_input_tokens = EXCLUDED.total_input_tokens,
                total_output_tokens = EXCLUDED.total_output_tokens,
                total_duration_ms = EXCLUDED.total_duration_ms,
                reported_at = EXCLUDED.reported_at,
                updated_at = EXCLUDED.updated_at"
        )
        .bind(&trace.id)
        .bind(&trace.session_id)
        .bind(&trace.run_id)
        .bind(&trace.turn_id)
        .bind(&trace.environment)
        .bind(&trace.release)
        .bind(&trace.project)
        .bind(trace.eval_status.to_string())
        .bind(payload_str)
        .bind(effective_in_tokens)
        .bind(effective_out_tokens)
        .bind(effective_duration)
        .bind(trace.reported_at)
        .bind(trace.created_at)
        .bind(trace.updated_at)
        .execute(&self.pool)
        .await?;

        let mut saved = trace;
        saved.payload = merged_payload;
        Ok(saved)
    }

    async fn get_trace(&self, id: &str) -> Result<TraceRecord, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, session_id, run_id, turn_id, environment, release, project,
                    eval_status, payload::text AS payload, total_input_tokens, total_output_tokens,
                    total_duration_ms, reported_at, created_at, updated_at
             FROM traces WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| RepositoryError::TraceNotFound(id.to_string()))?;

        Self::trace_from_row(row)
    }

    async fn list_traces(&self, filter: TraceFilter) -> Result<Vec<TraceRecord>, RepositoryError> {
        let mut query = "SELECT id, session_id, run_id, turn_id, environment, release, project,
                                eval_status, payload::text AS payload, total_input_tokens, total_output_tokens,
                                total_duration_ms, reported_at, created_at, updated_at
                         FROM traces WHERE 1=1"
            .to_string();
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
        }

        query.push_str(" ORDER BY created_at DESC");

        let limit = filter.limit.unwrap_or(50);
        let offset = filter.offset.unwrap_or(0);
        query.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));

        let mut q = sqlx::query(&query);
        for b in binds {
            q = q.bind(b);
        }

        let rows = q.fetch_all(&self.pool).await?;
        let mut traces = Vec::new();
        for row in rows {
            traces.push(Self::trace_from_row(row)?);
        }

        Ok(traces)
    }

    async fn update_trace_eval_status(
        &self,
        id: &str,
        status: EvalStatus,
    ) -> Result<TraceRecord, RepositoryError> {
        let now = Utc::now();
        let rows_affected =
            sqlx::query("UPDATE traces SET eval_status = $1, updated_at = $2 WHERE id = $3")
                .bind(status.to_string())
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
        let deleted = sqlx::query(
            "DELETE FROM traces WHERE id IN (SELECT id FROM traces WHERE created_at < $1 LIMIT $2)",
        )
        .bind(cutoff)
        .bind(batch_size)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(deleted)
    }
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
            reported_at: row.get("reported_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
}
