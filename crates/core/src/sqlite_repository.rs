use crate::{
    db::{create_pool, DatabasePool},
    models::{Event, Issue, IssueStatus, Platform},
    repository::{IssueFilter, IssueRepository, RepositoryError},
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
        Ok(())
    }
}

#[async_trait]
impl IssueRepository for SqliteIssueRepository {
    async fn record_event_and_upsert_issue(
        &self,
        fingerprint: &str,
        title: &str,
        culprit: Option<&str>,
        platform: &str,
        release: Option<&str>,
        environment: Option<&str>,
        payload: serde_json::Value,
    ) -> Result<(Issue, Event), RepositoryError> {
        let now = Utc::now();
        let existing = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, first_seen_at, last_seen_at FROM issues WHERE fingerprint = ?"
        )
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await?;

        let issue = match existing {
            Some(row) => {
                let id: String = row.get("id");
                let current_status_str: String = row.get("status");
                let mut status = current_status_str.parse::<IssueStatus>().unwrap_or(IssueStatus::Unresolved);
                let current_count: i64 = row.get("count");
                let assigned_to: Option<String> = row.get("assigned_to");
                let first_seen_str: String = row.get("first_seen_at");
                let first_seen = chrono::DateTime::parse_from_rfc3339(&first_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now);

                // 核心状态机：若处于 Resolved，新事件触发 Regression 重开
                if status == IssueStatus::Resolved {
                    status = IssueStatus::Regression;
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
                    first_seen_at: first_seen,
                    last_seen_at: now,
                }
            }
            None => {
                let id = Uuid::new_v4().to_string();
                let status = IssueStatus::Unresolved;
                sqlx::query(
                    "INSERT INTO issues (id, fingerprint, title, culprit, platform, status, count, last_release, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                )
                .bind(&id)
                .bind(fingerprint)
                .bind(title)
                .bind(culprit)
                .bind(platform)
                .bind(status.to_string())
                .bind(1i64)
                .bind(release)
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

        Ok((issue, event))
    }

    async fn get_issue(&self, id: &str) -> Result<Issue, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, first_seen_at, last_seen_at FROM issues WHERE id = ?"
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
            first_seen_at: chrono::DateTime::parse_from_rfc3339(&first_seen_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            last_seen_at: chrono::DateTime::parse_from_rfc3339(&last_seen_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, RepositoryError> {
        let mut query = "SELECT id, fingerprint, title, culprit, platform, status, assigned_to, count, last_release, first_seen_at, last_seen_at FROM issues WHERE 1=1".to_string();

        if let Some(ref st) = filter.status {
            query.push_str(&format!(" AND status = '{}'", st));
        }
        if let Some(ref pf) = filter.platform {
            query.push_str(&format!(" AND platform = '{}'", pf));
        }
        if let Some(ref rel) = filter.release {
            query.push_str(&format!(" AND last_release = '{}'", rel));
        }
        query.push_str(" ORDER BY last_seen_at DESC");

        let limit = filter.limit.unwrap_or(50);
        let offset = filter.offset.unwrap_or(0);
        query.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

        let rows = sqlx::query(&query).fetch_all(&self.pool).await?;
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

    async fn get_events_for_issue(&self, issue_id: &str, limit: i64) -> Result<Vec<Event>, RepositoryError> {
        let rows = sqlx::query("SELECT id, issue_id, payload, release, environment, created_at FROM events WHERE issue_id = ? ORDER BY created_at DESC LIMIT ?")
            .bind(issue_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let mut events = Vec::new();
        for row in rows {
            let created_str: String = row.get("created_at");
            let payload_str: String = row.get("payload");
            let payload: serde_json::Value = serde_json::from_str(&payload_str).unwrap_or(json!({}));
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
}
