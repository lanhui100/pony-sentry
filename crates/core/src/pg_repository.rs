use crate::{
    models::{Event, Issue, IssueStatus},
    repository::{IssueFilter, IssueRepository, RepositoryError},
};
use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Row};
use uuid::Uuid;

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
        Ok(())
    }
}

#[async_trait]
impl IssueRepository for PgIssueRepository {
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
    ) -> Result<(Issue, Event), RepositoryError> {
        let now = Utc::now();
        let existing = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at FROM issues WHERE fingerprint = $1"
        )
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await?;

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
                let first_seen: chrono::DateTime<Utc> = row.get("first_seen_at");

                if status == IssueStatus::Resolved {
                    status = IssueStatus::Regression;
                }

                let new_count = current_count + 1;
                sqlx::query(
                    "UPDATE issues SET count = $1, status = $2, last_release = $3, last_seen_at = $4 WHERE id = $5"
                )
                .bind(new_count)
                .bind(status.to_string())
                .bind(release)
                .bind(now)
                .bind(&id)
                .execute(&self.pool)
                .await?;

                // 项目名以首报为准，理由同 SQLite 实现。
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
                let id = Uuid::new_v4().to_string();
                let status = IssueStatus::Unresolved;
                sqlx::query(
                    "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, count, last_release, project, first_seen_at, last_seen_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
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
                .bind(now)
                .bind(now)
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
            "INSERT INTO events (id, issue_id, payload, release, environment, created_at) VALUES ($1, $2, $3, $4, $5, $6)"
        )
        .bind(&event_id)
        .bind(&issue.id)
        .bind(&payload)
        .bind(release)
        .bind(environment)
        .bind(now)
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

        Ok((issue, event))
    }

    async fn get_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, project, first_seen_at, last_seen_at FROM issues WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| RepositoryError::NotFound(id.to_string()))?;

        let status_str: String = row.get("status");

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
            first_seen_at: row.get("first_seen_at"),
            last_seen_at: row.get("last_seen_at"),
        })
    }

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, RepositoryError> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
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
                first_seen_at: row.get("first_seen_at"),
                last_seen_at: row.get("last_seen_at"),
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
        let rows = sqlx::query("SELECT id, issue_id, payload, release, environment, created_at FROM events WHERE issue_id = $1 ORDER BY created_at DESC LIMIT $2")
            .bind(issue_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let mut events = Vec::new();
        for row in rows {
            let payload: serde_json::Value = row.get("payload");
            events.push(Event {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                payload,
                release: row.get("release"),
                environment: row.get("environment"),
                created_at: row.get("created_at"),
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
}
