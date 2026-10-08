use crate::models::{Event, Issue, IssueStatus};
use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Issue not found: {0}")]
    NotFound(String),
    #[error("Invalid state transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
}

#[derive(Debug, Clone, Default)]
pub struct IssueFilter {
    pub status: Option<IssueStatus>,
    pub platform: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[async_trait]
pub trait IssueRepository: Send + Sync {
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
    ) -> Result<(Issue, Event), RepositoryError>;

    async fn get_issue(&self, id: &str) -> Result<Issue, RepositoryError>;

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, RepositoryError>;

    async fn update_issue_status(
        &self,
        id: &str,
        status: IssueStatus,
        assigned_to: Option<String>,
    ) -> Result<Issue, RepositoryError>;

    async fn get_events_for_issue(
        &self,
        issue_id: &str,
        limit: i64,
    ) -> Result<Vec<Event>, RepositoryError>;

    /// 已出现过的项目名去重列表，供 Web 端项目筛选下拉使用（不含未注入工作区的事件）。
    async fn list_projects(&self) -> Result<Vec<String>, RepositoryError>;
}
