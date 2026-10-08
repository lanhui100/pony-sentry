use crate::models::{EvalStatus, Event, Issue, IssueStatus, TraceFilter, TraceRecord};
use crate::reindex::FingerprintOut;
use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Issue not found: {0}")]
    NotFound(String),
    #[error("Trace not found: {0}")]
    TraceNotFound(String),
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

#[derive(Debug, Clone)]
pub struct UpsertIssueResult {
    pub issue: Issue,
    pub event: Event,
    pub is_new: bool,
    pub is_regression_trigger: bool,
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
    ) -> Result<UpsertIssueResult, RepositoryError>;

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

    /// 全量指纹重索引（幂等）：按新指纹方案重建 issue 归属并持久化。
    /// `fingerprint_of` 由上层注入与实时上报完全一致的指纹计算闭包。
    /// 返回执行摘要（新建/复用/删除/事件迁移/跳过）。
    async fn reindex_fingerprints(
        &self,
        fingerprint_of: &(dyn for<'a> Fn(&'a serde_json::Value) -> Option<FingerprintOut> + Sync),
    ) -> Result<crate::reindex::ReindexSummary, RepositoryError>;
}

#[async_trait]
pub trait TraceRepository: Send + Sync {
    async fn record_trace(&self, trace: TraceRecord) -> Result<TraceRecord, RepositoryError>;

    async fn get_trace(&self, id: &str) -> Result<TraceRecord, RepositoryError>;

    async fn list_traces(&self, filter: TraceFilter) -> Result<Vec<TraceRecord>, RepositoryError>;

    async fn update_trace_eval_status(
        &self,
        id: &str,
        status: EvalStatus,
    ) -> Result<TraceRecord, RepositoryError>;
}
