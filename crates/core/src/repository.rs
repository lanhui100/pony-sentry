use crate::models::{
    EvalStatus, Event, IngestGovernanceQuery, IngestGovernanceStats, IngestMetricOutcome, Issue,
    IssueArchiveMode, IssueArchiveOutcome, IssueArchiveRequest, IssueEventsCursor, IssueEventsOrder,
    IssueStatsQuery, IssueStatsResponse, IssueStatus, IssueStatusEnvironmentCount, StatsBucket,
    TraceFilter, TraceRecord,
};
use crate::reindex::FingerprintOut;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

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
    /// 归档三态筛选（契约 `C3.2`；默认 [`IssueArchiveMode::Exclude`] = 只看未归档）。
    ///
    /// 只新增字段，不改变既有字段语义：缺省值对「尚未归档」的存量数据零影响（`B2`）。
    pub archived: IssueArchiveMode,
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
pub trait IssueRepository: IssueGovernanceRepository + Send + Sync {
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

    /// 与 [`TraceRepository::list_traces`] **同过滤条件**的计数（供 `X-Total-Count`，`amend v1.5-7`）。
    ///
    /// 不受 `limit/offset` 影响。
    async fn count_traces(&self, filter: TraceFilter) -> Result<i64, RepositoryError>;

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

// ============================================================================
// issue 治理读/写能力（契约：`.dev-team/contract-issue-governance.md` FROZEN v1.2）
//
// **必需方法（无默认体）**：`IssueRepository: IssueGovernanceRepository` 为超级 trait，
// 因此两种方言（PG / SQLite）必须同时实现本 trait——缺一即编译失败（刻意的编译期强制）。
// 既有 `IssueRepository` / `TraceRepository` 的既有方法签名不得修改。
// ============================================================================
#[async_trait]
pub trait IssueGovernanceRepository: Send + Sync {
    /// `C3.3`：与 `list_issues` **同过滤条件**的总数（`X-Total-Count` / `envelope.total` 数据源），
    /// 不受 `limit/offset` 影响（先例：`TraceRepository::count_traces`）。
    async fn count_issues(&self, filter: IssueFilter) -> Result<i64, RepositoryError>;

    /// `C2.3`：单条归档（置 `archived_at`）。已归档 → 幂等返回既有行（`archived_at` 不变）；
    /// 不存在 → `RepositoryError::NotFound`（路由层映射 404）。
    ///
    /// `amend v1.1 / A-1`：**不覆盖** `archive_restored_at`（保持原值），只写 `archived_at`。
    async fn archive_issue(
        &self,
        id: &str,
        archived_at: DateTime<Utc>,
    ) -> Result<Issue, RepositoryError>;

    /// `C2.3`：单条恢复（`archived_at = NULL`）。不存在 → `NotFound`。
    ///
    /// `amend v1.1 / A-1`：恢复**必须**同语句写入 `archive_restored_at = now()`（自动 TTL 归档的抑制依据）；
    /// 对已处于未归档状态的行同样刷新 `archive_restored_at`（单调前移）。
    async fn unarchive_issue(&self, id: &str) -> Result<Issue, RepositoryError>;

    /// `C2.3` / `C2.4`：批量归档，返回 `C2.4` 结构。
    ///
    /// `ids` 非空时 `ids` 决定候选集（`project`/`older_than_days` 不参与筛选），
    /// 但 `statuses` **仍是归档白名单约束**（`amend v1.2`）；`dry_run=true` 只统计不改库
    /// （`archived == 0`）。`now` 由调用方注入（便于测试确定性）。
    ///
    /// 计数口径（`amend v1.1 / A-3`）：`statuses` 模式 `matched ≡ archived`（`skipped_*` 恒 0）；
    /// `ids` 模式 `matched = dedup(ids).len()`，并在**同一事务内**先分桶
    /// `skipped_not_archivable` / `skipped_already_archived` / `skipped_missing` 再批量 `UPDATE`。
    /// 上限（`B-5`）：`dedup(ids).len() <= 1000`、单 id `<= 128` 字符，校验归属 handler。
    async fn archive_issues(
        &self,
        request: &IssueArchiveRequest,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<IssueArchiveOutcome, RepositoryError>;

    /// `C2.2`：自动 TTL 归档一轮——把 `status IN (statuses) AND archived_at IS NULL
    /// AND last_seen_at < cutoff AND (archive_restored_at IS NULL OR archive_restored_at < last_seen_at)`
    /// 的行置为归档，返回本次归档行数。
    ///
    /// `statuses` 由调用方限定为 `["ignored","resolved"]`（`unresolved`/`in_progress`/`regression`
    /// 永不自动归档）；本方法**不删**任何 events 行、不改 `status`/`count`，
    /// 且**不覆盖** `archive_restored_at`。
    async fn archive_stale_issues(
        &self,
        cutoff: DateTime<Utc>,
        statuses: &[IssueStatus],
    ) -> Result<u64, RepositoryError>;

    /// `C4.2`：keyset 事件分页（`ORDER BY created_at DESC, id DESC`，`Asc` 时升序）。
    ///
    /// `cursor` 为解码后的 [`IssueEventsCursor`]（首页为 `None`）；issue 不存在 → 空 Vec（非错误）。
    ///
    /// `amend v1.1 / B-2`：游标的 `created_at` 是**库内原始 RFC3339 串**，比较按方言原生类型
    /// （SQLite TEXT 字典序 / PG timestamptz），**禁止**截断到微秒或重新格式化。
    async fn list_issue_events_page(
        &self,
        issue_id: &str,
        limit: i64,
        cursor: Option<IssueEventsCursor>,
        order: IssueEventsOrder,
    ) -> Result<Vec<Event>, RepositoryError>;

    /// `C4.4`：该 issue 的事件总行数（`amend v1.1 / B-4`：`X-Total-Count` 与信封 `total` 的数据源，
    /// 翻页不重不漏的断言基准）。
    async fn count_events_for_issue(&self, issue_id: &str) -> Result<i64, RepositoryError>;

    /// `C5`：`GET /api/v1/stats` 的全量统计聚合（issues 聚合口径 + events 聚合/行双口径 + series）。
    ///
    /// 作用域严格按 `C5.0` 表：issues 各聚合与 `events.total` **不受** `from`/`to` 影响；
    /// `stored_rows` / `by_*`（行口径）/ `series` **受** `from`/`to` 影响。
    async fn issue_stats(&self, query: &IssueStatsQuery) -> Result<IssueStatsResponse, RepositoryError>;

    /// `C6.1`：`GET /api/v1/ingest/stats` 的治理指标读取（读时把 `project=''` 映射为 `null`）。
    async fn ingest_governance_stats(
        &self,
        query: &IngestGovernanceQuery,
    ) -> Result<IngestGovernanceStats, RepositoryError>;

    /// `C6.2`：治理计数单事件 UPSERT（`bucket_date` = UTC 日期；无 project 维度传空串）。
    ///
    /// 失败只 `warn` 不阻断 ingest（`warn_only_no_ingest_impact`）：调用方不得 `?` 传播该错误。
    /// 单次写入超时 `ingest_metrics_write_timeout_ms = 1000`；`SQLITE_BUSY` 等丢计数允许，
    /// 但必须 `warn!` 带上 `outcome` 与丢失计数（`amend v1.1 / B-3`）。
    async fn record_ingest_metric(
        &self,
        bucket_date: NaiveDate,
        project: &str,
        outcome: IngestMetricOutcome,
    ) -> Result<(), RepositoryError>;
}

// ============================================================================
// 双方言共享的纯函数（口径单一真相：`list_issues`/`count_issues`/`stats` 共用同一 WHERE）
// ============================================================================

/// 组装 `issues` 过滤子句（`list_issues` / `count_issues` / stats 聚合共用，保证「同 WHERE」）。
///
/// - `prefix`：裸列名传 `""`；events 行口径 join `issues AS i` 时传 `"i."`；
/// - `placeholder`：按 1 起的位置序号产出占位符（PG `$n` / SQLite `?`）。
///
/// 归档三态（`C3.2`）：`exclude` → `archived_at IS NULL`；`only` → `IS NOT NULL`；`include` → 无条件。
pub(crate) struct IssueFilterClause {
    pub clause: String,
    pub binds: Vec<String>,
}

pub(crate) fn issue_filter_clause(
    filter: &IssueFilter,
    prefix: &str,
    placeholder: impl Fn(usize) -> String,
) -> IssueFilterClause {
    let mut clause = " WHERE 1=1".to_string();
    let mut binds: Vec<String> = Vec::new();
    macro_rules! push {
        ($value:expr, $column:expr) => {{
            binds.push($value);
            clause.push_str(&format!(
                " AND {prefix}{} = {}",
                $column,
                placeholder(binds.len())
            ));
        }};
    }
    if let Some(status) = &filter.status {
        push!(status.to_string(), "status");
    }
    if let Some(platform) = &filter.platform {
        push!(platform.clone(), "platform");
    }
    if let Some(release) = &filter.release {
        push!(release.clone(), "last_release");
    }
    if let Some(project) = &filter.project {
        push!(project.clone(), "project");
    }
    if let Some(environment) = &filter.environment {
        push!(environment.clone(), "environment");
    }
    if let Some(level) = &filter.level {
        push!(level.clone(), "level");
    }
    match filter.archived {
        IssueArchiveMode::Exclude => clause.push_str(&format!(" AND {prefix}archived_at IS NULL")),
        IssueArchiveMode::Only => clause.push_str(&format!(" AND {prefix}archived_at IS NOT NULL")),
        IssueArchiveMode::Include => {}
    }
    IssueFilterClause { clause, binds }
}

/// `C2.3` / `amend v1.2`：生效状态白名单。
///
/// 未给 → `["ignored","resolved"]`；给了 → 原值去重保序（白名单合法性由 handler 校验）。
pub(crate) fn effective_archive_statuses(request: &IssueArchiveRequest) -> Vec<String> {
    let mut statuses: Vec<String> = Vec::new();
    if let Some(raw) = &request.statuses {
        for status in raw {
            let status = status.trim();
            if !status.is_empty() && !statuses.iter().any(|existing| existing == status) {
                statuses.push(status.to_string());
            }
        }
    }
    if statuses.is_empty() {
        statuses.push("ignored".to_string());
        statuses.push("resolved".to_string());
    }
    statuses
}

/// `C2.3` / `amend v1.1 A-3`：`ids` 去重保序（空串忽略）。
pub(crate) fn dedup_ids(ids: &[String]) -> Vec<String> {
    let mut deduped: Vec<String> = Vec::new();
    for id in ids {
        let id = id.trim();
        if !id.is_empty() && !deduped.iter().any(|existing| existing == id) {
            deduped.push(id.to_string());
        }
    }
    deduped
}

/// 桶前缀 → `series[].bucket` 键（day `YYYY-MM-DD`；hour `YYYY-MM-DDTHH:00:00Z`，UTC）。
pub(crate) fn bucket_key_from_prefix(prefix: &str, bucket: StatsBucket) -> String {
    match bucket {
        StatsBucket::Day => prefix.chars().take(10).collect(),
        StatsBucket::Hour => format!("{}:00:00Z", prefix.chars().take(13).collect::<String>()),
    }
}

/// `C5` / `C6.1`：生成闭区间 `[from, to]` 的全部桶键（UTC 对齐到桶起点，空桶由调用方补 0）。
///
/// 点数上限由 handler 保证（`stats_series_max_points = 744`）；此处仅设防御性硬上限，
/// 避免异常区间把内存打满（handler 已 400 的请求不会走到这里）。
pub(crate) fn bucket_keys(from: DateTime<Utc>, to: DateTime<Utc>, bucket: StatsBucket) -> Vec<String> {
    use chrono::{Duration, TimeZone, Timelike};

    const MAX_POINTS: usize = 10_000;
    if to < from {
        return Vec::new();
    }
    let floor = |dt: DateTime<Utc>| -> DateTime<Utc> {
        match bucket {
            StatsBucket::Day => Utc.from_utc_datetime(
                &dt.date_naive()
                    .and_hms_opt(0, 0, 0)
                    .expect("00:00:00 恒合法"),
            ),
            StatsBucket::Hour => Utc.from_utc_datetime(
                &dt.date_naive()
                    .and_hms_opt(dt.hour(), 0, 0)
                    .expect("整点恒合法"),
            ),
        }
    };
    let step = match bucket {
        StatsBucket::Day => Duration::days(1),
        StatsBucket::Hour => Duration::hours(1),
    };
    let start = floor(from);
    let end = floor(to);
    let mut keys = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        if keys.len() >= MAX_POINTS {
            tracing::warn!(
                points = keys.len(),
                "series 桶数超过防御性上限，已截断（正常路径由 handler 以 744 点上限拦截）"
            );
            break;
        }
        keys.push(match bucket {
            StatsBucket::Day => cursor.format("%Y-%m-%d").to_string(),
            StatsBucket::Hour => cursor.format("%Y-%m-%dT%H:00:00Z").to_string(),
        });
        cursor += step;
    }
    keys
}

/// `C2.5` / `C2.1`：活跃态（`unresolved` / `in_progress` / `regression`）——写入成功后必须解档，
/// 且**永不**被自动 TTL 归档。`ignored` / `resolved` 属非活跃态（保持归档状态）。
pub(crate) fn is_active_issue_status(status: &IssueStatus) -> bool {
    matches!(
        status,
        IssueStatus::Unresolved | IssueStatus::InProgress | IssueStatus::Regression
    )
}

/// `C1.5` / `C6.2`：`ingest_metrics.outcome` 落库白名单字符串（PG 无 CHECK 约束，由此枚举约束）。
pub(crate) fn ingest_metric_outcome_str(outcome: IngestMetricOutcome) -> &'static str {
    match outcome {
        IngestMetricOutcome::Received => "received",
        IngestMetricOutcome::Created => "created",
        IngestMetricOutcome::Duplicate => "duplicate",
        IngestMetricOutcome::SuppressedIgnored => "suppressed_ignored",
        IngestMetricOutcome::RejectedInvalid => "rejected_invalid",
    }
}

/// `C5`：`GET /api/v1/stats` 的生效作用域（解析 + 默认值，双方言共用）。
///
/// 默认值（契约 C5）：`to = now`、`from = to - 30d`、`bucket = day`、`include_archived = false`。
/// 非法值由 handler 先行 400 拦截；此处对解析失败一律回落默认，**不 panic、不 500**。
pub(crate) struct StatsScope {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub bucket: StatsBucket,
    pub include_archived: bool,
    pub filter: IssueFilter,
}

pub(crate) fn stats_scope(query: &IssueStatsQuery, now: DateTime<Utc>) -> StatsScope {
    let to = query
        .to
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);
    let from = query
        .from
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| to - chrono::Duration::days(30));
    let bucket = match query.bucket.as_deref().map(str::trim) {
        Some("hour") => StatsBucket::Hour,
        _ => StatsBucket::Day,
    };
    let include_archived = matches!(
        query.include_archived.as_deref().map(str::trim),
        Some("1") | Some("true") | Some("on")
    );
    let non_empty = |value: &Option<String>| -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(str::to_string)
    };
    StatsScope {
        from,
        to,
        bucket,
        include_archived,
        filter: IssueFilter {
            project: non_empty(&query.project),
            environment: non_empty(&query.environment),
            level: non_empty(&query.level),
            archived: if include_archived {
                IssueArchiveMode::Include
            } else {
                IssueArchiveMode::Exclude
            },
            ..Default::default()
        },
    }
}
