use crate::{
    db::DatabasePool,
    models::{EvalStatus, Event, Issue, IssueStatus, TraceFilter, TraceRecord},
    reindex::{plan_reindex, FingerprintOut, ReindexSummary},
    repository::{IssueFilter, IssueRepository, RepositoryError, TraceRepository},
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

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
        sqlx::raw_sql(traces_sql).execute(&self.pool).await?;
        Ok(())
    }
}

#[async_trait]
impl IssueRepository for SqliteIssueRepository {
    #[allow(clippy::too_many_arguments)]
    async fn record_event_and_upsert_issue(
        &self,
        fingerprint: &str,
        title: &str,
        culprit: Option<&str>,
        platform: &str,
        release: Option<&str>,
        environment: Option<&str>,
        project: Option<&str>,
        payload: serde_json::Value,
    ) -> Result<crate::repository::UpsertIssueResult, RepositoryError> {
        let now = Utc::now();
        let existing = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at FROM issues WHERE fingerprint = ?"
        )
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await?;

        let mut is_new = false;
        let mut is_regression_trigger = false;

        let issue = match existing {
            Some(row) => {
                let id: String = row.get("id");
                let current_status_str: String = row.get("status");
                let mut status = current_status_str
                    .parse::<IssueStatus>()
                    .unwrap_or(IssueStatus::Unresolved);
                let current_count: i64 = row.get("count");
                let assigned_to: Option<String> = row.get("assigned_to");
                let stored_project: Option<String> = row.get("project");
                let first_seen_str: String = row.get("first_seen_at");
                let first_seen = chrono::DateTime::parse_from_rfc3339(&first_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now);

                // 核心状态机：仅当首次处于 Resolved 且新事件再次出现时，才触发一次 Regression 状态跃迁
                if status == IssueStatus::Resolved {
                    status = IssueStatus::Regression;
                    is_regression_trigger = true;
                }

                let new_count = current_count + 1;
                sqlx::query(
                    "UPDATE issues SET count = ?, status = ?, last_release = ?, last_seen_at = ? WHERE id = ?"
                )
                .bind(new_count)
                .bind(status.to_string())
                .bind(release)
                .bind(now.to_rfc3339())
                .bind(&id)
                .execute(&self.pool)
                .await?;

                // 项目名以首报为准：issue 的指纹/标题/出错位置都诞生于首次上报的那个工作区，
                // 若让后续上报覆盖，同一 issue 的「项目」标签会随最后一次写入漂移，筛选随之失真。
                let project = stored_project.or_else(|| project.map(|s| s.to_string()));

                Issue {
                    id,
                    fingerprint: fingerprint.to_string(),
                    title: title.to_string(),
                    culprit: culprit.map(|s| s.to_string()),
                    platform: platform.to_string(),
                    status,
                    assigned_to,
                    count: new_count,
                    last_release: release.map(|s| s.to_string()),
                    project,
                    first_seen_at: first_seen,
                    last_seen_at: now,
                }
            }
            None => {
                is_new = true;
                let id = Uuid::new_v4().to_string();
                let status = IssueStatus::Unresolved;
                sqlx::query(
                    "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, count, last_release, project, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                )
                .bind(&id)
                .bind(fingerprint)
                .bind(title)
                .bind(culprit)
                .bind(platform)
                .bind(status.to_string())
                .bind(1i64)
                .bind(release)
                .bind(project)
                .bind(now.to_rfc3339())
                .bind(now.to_rfc3339())
                .execute(&self.pool)
                .await?;

                Issue {
                    id,
                    fingerprint: fingerprint.to_string(),
                    title: title.to_string(),
                    culprit: culprit.map(|s| s.to_string()),
                    platform: platform.to_string(),
                    status,
                    assigned_to: None,
                    count: 1,
                    last_release: release.map(|s| s.to_string()),
                    project: project.map(|s| s.to_string()),
                    first_seen_at: now,
                    last_seen_at: now,
                }
            }
        };

        let event_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO events (id, issue_id, payload, release, environment, created_at) VALUES (?, ?, ?, ?, ?, ?)"
        )
        .bind(&event_id)
        .bind(&issue.id)
        .bind(serde_json::to_string(&payload).unwrap_or_default())
        .bind(release)
        .bind(environment)
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;

        let event = Event {
            id: event_id,
            issue_id: issue.id.clone(),
            payload,
            release: release.map(|s| s.to_string()),
            environment: environment.map(|s| s.to_string()),
            created_at: now,
        };

        Ok(crate::repository::UpsertIssueResult {
            issue,
            event,
            is_new,
            is_regression_trigger,
        })
    }

    async fn get_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at FROM issues WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| RepositoryError::NotFound(id.to_string()))?;

        let status_str: String = row.get("status");
        let first_seen_str: String = row.get("first_seen_at");
        let last_seen_str: String = row.get("last_seen_at");

        Ok(Issue {
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
            first_seen_at: chrono::DateTime::parse_from_rfc3339(&first_seen_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            last_seen_at: chrono::DateTime::parse_from_rfc3339(&last_seen_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, RepositoryError> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at FROM issues WHERE 1=1"
        );

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
        builder.push(" ORDER BY last_seen_at DESC");

        let limit = filter.limit.unwrap_or(50).clamp(1, 100);
        let offset = filter.offset.unwrap_or(0).max(0);
        builder
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let mut issues = Vec::new();
        for row in rows {
            let status_str: String = row.get("status");
            let first_seen_str: String = row.get("first_seen_at");
            let last_seen_str: String = row.get("last_seen_at");
            issues.push(Issue {
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
                first_seen_at: chrono::DateTime::parse_from_rfc3339(&first_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                last_seen_at: chrono::DateTime::parse_from_rfc3339(&last_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            });
        }
        Ok(issues)
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
        let rows = sqlx::query("SELECT id, issue_id, payload, release, environment, created_at FROM events WHERE issue_id = ? ORDER BY created_at DESC LIMIT ?")
            .bind(issue_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let mut events = Vec::new();
        for row in rows {
            let created_str: String = row.get("created_at");
            let payload_str: String = row.get("payload");
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).unwrap_or(json!({}));
            events.push(Event {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                payload,
                release: row.get("release"),
                environment: row.get("environment"),
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            });
        }
        Ok(events)
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

    async fn reindex_fingerprints(
        &self,
        fingerprint_of: &(dyn for<'a> Fn(&'a serde_json::Value) -> Option<FingerprintOut> + Sync),
    ) -> Result<ReindexSummary, RepositoryError> {
        // 1. 全量读取 events 与 issues
        let event_rows = sqlx::query(
            "SELECT id, issue_id, payload, release, environment, created_at FROM events",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(event_rows.len());
        for row in event_rows {
            let created_str: String = row.get("created_at");
            let payload_str: String = row.get("payload");
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).unwrap_or(json!({}));
            events.push(Event {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                payload,
                release: row.get("release"),
                environment: row.get("environment"),
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            });
        }

        let issue_rows = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at FROM issues",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut issues = Vec::with_capacity(issue_rows.len());
        for row in issue_rows {
            let status_str: String = row.get("status");
            let first_str: String = row.get("first_seen_at");
            let last_str: String = row.get("last_seen_at");
            issues.push(Issue {
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
                first_seen_at: chrono::DateTime::parse_from_rfc3339(&first_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                last_seen_at: chrono::DateTime::parse_from_rfc3339(&last_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            });
        }

        // 2. 规划（纯函数）
        let plan = plan_reindex(&events, &issues, fingerprint_of);

        // 3. 事务持久化
        let mut tx = self.pool.begin().await?;
        for row in &plan.issue_rows {
            if plan.reused_ids.contains(&row.id) {
                sqlx::query(
                    "UPDATE issues SET fingerprint = ?, title = ?, culprit = ?, platform = ?, status = ?, assigned_to = ?, count = ?, last_release = ?, project = ?, first_seen_at = ?, last_seen_at = ? WHERE id = ?",
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
                .bind(row.first_seen_at.to_rfc3339())
                .bind(row.last_seen_at.to_rfc3339())
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        let payload_str = serde_json::to_string(&trace.payload).unwrap_or_else(|_| "{}".to_string());
        sqlx::query(
            "INSERT INTO traces (
                id, session_id, run_id, turn_id, environment, release,
                eval_status, payload, total_input_tokens, total_output_tokens,
                total_duration_ms, reported_at, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&trace.id)
        .bind(&trace.session_id)
        .bind(&trace.run_id)
        .bind(&trace.turn_id)
        .bind(&trace.environment)
        .bind(&trace.release)
        .bind(trace.eval_status.to_string())
        .bind(payload_str)
        .bind(trace.total_input_tokens)
        .bind(trace.total_output_tokens)
        .bind(trace.total_duration_ms)
        .bind(trace.reported_at.to_rfc3339())
        .bind(trace.created_at.to_rfc3339())
        .bind(trace.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(trace)
    }

    async fn get_trace(&self, id: &str) -> Result<TraceRecord, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, session_id, run_id, turn_id, environment, release,
                    eval_status, payload, total_input_tokens, total_output_tokens,
                    total_duration_ms, reported_at, created_at, updated_at
             FROM traces WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| RepositoryError::TraceNotFound(id.to_string()))?;

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

        Ok(TraceRecord {
            id: row.get("id"),
            session_id: row.get("session_id"),
            run_id: row.get("run_id"),
            turn_id: row.get("turn_id"),
            environment: row.get("environment"),
            release: row.get("release"),
            eval_status,
            payload,
            total_input_tokens: row.get("total_input_tokens"),
            total_output_tokens: row.get("total_output_tokens"),
            total_duration_ms: row.get("total_duration_ms"),
            reported_at: chrono::DateTime::parse_from_rfc3339(&reported_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    async fn list_traces(&self, filter: TraceFilter) -> Result<Vec<TraceRecord>, RepositoryError> {
        let mut query = "SELECT id, session_id, run_id, turn_id, environment, release,
                                eval_status, payload, total_input_tokens, total_output_tokens,
                                total_duration_ms, reported_at, created_at, updated_at
                         FROM traces WHERE 1=1".to_string();
        let mut binds: Vec<String> = Vec::new();

        if let Some(session_id) = &filter.session_id {
            query.push_str(" AND session_id = ?");
            binds.push(session_id.clone());
        }
        if let Some(eval_status) = &filter.eval_status {
            query.push_str(" AND eval_status = ?");
            binds.push(eval_status.to_string());
        }
        if let Some(environment) = &filter.environment {
            query.push_str(" AND environment = ?");
            binds.push(environment.clone());
        }
        if let Some(release) = &filter.release {
            query.push_str(" AND release = ?");
            binds.push(release.clone());
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

            traces.push(TraceRecord {
                id: row.get("id"),
                session_id: row.get("session_id"),
                run_id: row.get("run_id"),
                turn_id: row.get("turn_id"),
                environment: row.get("environment"),
                release: row.get("release"),
                eval_status,
                payload,
                total_input_tokens: row.get("total_input_tokens"),
                total_output_tokens: row.get("total_output_tokens"),
                total_duration_ms: row.get("total_duration_ms"),
                reported_at: chrono::DateTime::parse_from_rfc3339(&reported_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            });
        }

        Ok(traces)
    }

    async fn update_trace_eval_status(
        &self,
        id: &str,
        status: EvalStatus,
    ) -> Result<TraceRecord, RepositoryError> {
        let now = Utc::now();
        let rows_affected = sqlx::query("UPDATE traces SET eval_status = ?, updated_at = ? WHERE id = ?")
            .bind(status.to_string())
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
}
