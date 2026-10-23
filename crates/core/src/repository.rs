use crate::models::{
    EvalStatus, Event, Issue, IssueStatus, IssueStatusEnvironmentCount, TraceFilter, TraceRecord,
};
use crate::reindex::FingerprintOut;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

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
    /// 环境维度筛选（`GET /api/v1/issues?environment=<env>`）。
    pub environment: Option<String>,
    /// 等级维度筛选（`fatal|error|warning|info`）。
    pub level: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// 一次事件写入所需的全部维度（避免超长位置参数；`event_key` 为客户端幂等键）。
///
/// 语义：`event_key` 为 `Some` 且该键已存在时，整次写入按幂等命中处理——
/// 不新增 events 行、不递增 `issues.count`、不推进 `issues.last_seen_at`（契约 R4.1）。
#[derive(Debug, Clone)]
pub struct NewEventRecord<'a> {
    pub fingerprint: &'a str,
    pub title: &'a str,
    pub culprit: Option<&'a str>,
    pub platform: &'a str,
    pub release: Option<&'a str>,
    /// 环境维度；`None` 落库为 `unknown`。
    pub environment: Option<&'a str>,
    pub project: Option<&'a str>,
    pub app: Option<&'a str>,
    pub namespace: Option<&'a str>,
    pub component: Option<&'a str>,
    /// 已校验并完成降级的事件等级（调用方负责非法值 → `warning`）。
    pub level: &'a str,
    pub source_kind: Option<&'a str>,
    /// 客户端确定性幂等键（上报一级 `event_id`）。
    pub event_key: Option<&'a str>,
    pub payload: serde_json::Value,
    /// 事件发生时间（上报 `timestamp` 或服务端 now）；`None` 取写入时刻。
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpsertIssueResult {
    pub issue: Issue,
    pub event: Event,
    pub is_new: bool,
    pub is_regression_trigger: bool,
    /// `event_key` 幂等命中：本次写入未产生新 events 行，issue 计数与时间戳均未变更。
    pub is_duplicate: bool,
}

#[async_trait]
pub trait IssueRepository: Send + Sync {
    async fn record_event_and_upsert_issue(
        &self,
        record: NewEventRecord<'_>,
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

    /// 存活探针（`/healthz`）：只做一次最轻量的 DB 往返，失败即视为不可用。
    async fn health_check(&self) -> Result<(), RepositoryError>;

    /// 按 `(status, environment)` 聚合的 issue 计数（`/metrics`）。
    async fn issue_counts_by_status_environment(
        &self,
    ) -> Result<Vec<IssueStatusEnvironmentCount>, RepositoryError>;

    /// events 总行数（`/metrics`）。
    async fn count_events(&self) -> Result<i64, RepositoryError>;

    /// 分批删除 `created_at < cutoff` 的 events，返回本批删除行数（retention 任务）。
    async fn delete_events_older_than(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: i64,
    ) -> Result<u64, RepositoryError>;
}

#[async_trait]
pub trait TraceRepository: Send + Sync {
    async fn record_trace(&self, trace: TraceRecord) -> Result<TraceRecord, RepositoryError>;

    async fn get_trace(&self, id: &str) -> Result<TraceRecord, RepositoryError>;

    async fn list_traces(&self, filter: TraceFilter) -> Result<Vec<TraceRecord>, RepositoryError>;

    async fn update_trace_review(
        &self,
        id: &str,
        status: EvalStatus,
        note: Option<&str>,
    ) -> Result<TraceRecord, RepositoryError>;

    /// 兼容入口（既有调用方只改状态）：等价于 `update_trace_review(id, status, None)`。
    async fn update_trace_eval_status(
        &self,
        id: &str,
        status: EvalStatus,
    ) -> Result<TraceRecord, RepositoryError> {
        self.update_trace_review(id, status, None).await
    }

    /// Trace 维度已出现过的项目名去重排序列表（不含 NULL 与空串），
    /// 供 Web 端项目筛选下拉与 `/api/v1/projects` 聚合使用。
    async fn list_projects(&self) -> Result<Vec<String>, RepositoryError>;

    /// 分批删除 `COALESCE(updated_at, created_at) < cutoff` 的 traces（`C6`），返回本批删除行数。
    async fn delete_traces_older_than(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: i64,
    ) -> Result<u64, RepositoryError>;
}
