use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Unresolved,
    InProgress,
    Resolved,
    Ignored,
    Regression,
}

impl std::fmt::Display for IssueStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueStatus::Unresolved => write!(f, "unresolved"),
            IssueStatus::InProgress => write!(f, "in_progress"),
            IssueStatus::Resolved => write!(f, "resolved"),
            IssueStatus::Ignored => write!(f, "ignored"),
            IssueStatus::Regression => write!(f, "regression"),
        }
    }
}

impl std::str::FromStr for IssueStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unresolved" => Ok(IssueStatus::Unresolved),
            "in_progress" => Ok(IssueStatus::InProgress),
            "resolved" => Ok(IssueStatus::Resolved),
            "ignored" => Ok(IssueStatus::Ignored),
            "regression" => Ok(IssueStatus::Regression),
            _ => Err(format!("Unknown issue status: {}", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Rust,
    Tauri,
    Vue,
    Python,
    Other(String),
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Rust => write!(f, "rust"),
            Platform::Tauri => write!(f, "tauri"),
            Platform::Vue => write!(f, "vue"),
            Platform::Python => write!(f, "python"),
            Platform::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rust" => Ok(Platform::Rust),
            "tauri" => Ok(Platform::Tauri),
            "vue" => Ok(Platform::Vue),
            "python" => Ok(Platform::Python),
            other => Ok(Platform::Other(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub fingerprint: String,
    pub title: String,
    pub culprit: Option<String>,
    pub platform: String,
    pub status: IssueStatus,
    pub assigned_to: Option<String>,
    pub count: i64,
    pub last_release: Option<String>,
    /// 项目名（上报事件 `extra.project_path` 的末段），见 pony_sentry_ingest::project_name_from_extra。
    pub project: Option<String>,
    /// 部署环境维度（`dev|development|preprod|production|<自由串>`），参与 v3 指纹作用域；
    /// 历史 issue 由 0004 迁移从其最近一条 events.environment 回填，缺失为 `unknown`。
    pub environment: String,
    /// 应用名（上报一级字段 `app`，如 `api_v2`）。
    pub app: Option<String>,
    /// 命名空间（上报一级字段 `namespace`，通常与 environment 同值）。
    pub namespace: Option<String>,
    /// 组件名（上报一级字段 `component`，如 `agent`）。
    pub component: Option<String>,
    /// 事件等级 `fatal|error|warning|info`，非法上报值降级为 `warning`。
    pub level: String,
    /// 来源通道 `log|agent|backend_exception|frontend|alert`。
    pub source_kind: Option<String>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// 归档时刻（契约 `C2.1`）：`Some` 即已归档；`None` = 未归档。
    ///
    /// `serde(default)` 保证旧形态 JSON（无该字段）仍可反序列化为 `None`，
    /// 且序列化时缺失值显式落 `null`（不得省略字段——Web/验收侧按字段存在性判断）。
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    /// 最近一次**手动恢复**时刻（契约 `C2.1` / `amend v1.1 A-1`）：自动 TTL 归档的抑制依据；
    /// `None` = 从未手动恢复。
    #[serde(default)]
    pub archive_restored_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub issue_id: String,
    pub payload: serde_json::Value,
    pub release: Option<String>,
    pub environment: Option<String>,
    /// 客户端确定性幂等键（上报一级字段 `event_id`），全局唯一；历史事件为 NULL。
    pub event_key: Option<String>,
    /// 事件等级快照（与 issue.level 同源，供审计与回填留痕，读取接口不依赖）。
    pub level: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvalStatus {
    #[default]
    Unreviewed,
    TriageGood,
    TriageBad,
    EvalDataset,
    Optimized,
    Wontfix,
}

impl std::fmt::Display for EvalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreviewed => write!(f, "unreviewed"),
            Self::TriageGood => write!(f, "triage_good"),
            Self::TriageBad => write!(f, "triage_bad"),
            Self::EvalDataset => write!(f, "eval_dataset"),
            Self::Optimized => write!(f, "optimized"),
            Self::Wontfix => write!(f, "wontfix"),
        }
    }
}

impl std::str::FromStr for EvalStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unreviewed" => Ok(EvalStatus::Unreviewed),
            "triage_good" => Ok(EvalStatus::TriageGood),
            "triage_bad" => Ok(EvalStatus::TriageBad),
            "eval_dataset" => Ok(EvalStatus::EvalDataset),
            "optimized" => Ok(EvalStatus::Optimized),
            "wontfix" => Ok(EvalStatus::Wontfix),
            _ => Err(format!("Unknown eval status: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    pub id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub environment: String,
    pub release: String,
    /// 项目名（上报 `project` 字段），可空；用于项目维度聚合与筛选。
    #[serde(default)]
    pub project: Option<String>,
    pub eval_status: EvalStatus,
    pub payload: serde_json::Value,
    pub total_input_tokens: Option<i64>,
    pub total_output_tokens: Option<i64>,
    pub total_duration_ms: Option<i64>,
    /// 会话级终态摘要：该 session 最后一个 turn 的 status（`C1.2`）；缺失默认 `unknown`。
    #[serde(default = "default_turn_status")]
    pub turn_status: String,
    /// 该 session 已入库的 turn 数（`C1.2`）。
    #[serde(default)]
    pub turns_count: i64,
    /// 评审备注（PATCH 落库，`C1.2` / `C9.7`）。
    #[serde(default)]
    pub note: Option<String>,
    pub reported_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `C1.2`：`turn_status` 非空默认 `unknown`。
fn default_turn_status() -> String {
    "unknown".to_string()
}

/// 环境维度的状态分布（`/metrics` 的 `pony_sentry_issues_total{status,environment}` 数据源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueStatusEnvironmentCount {
    pub status: String,
    pub environment: String,
    pub count: i64,
}

/// `amend v1.5-2`（I-04/W-05）：列表排序维度；`None` 时按 `ReportedAt`（契约默认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOrder {
    /// 按上报时间倒序（契约默认）。
    #[default]
    ReportedAt,
    /// 按入库时间倒序（历史行为）。
    CreatedAt,
}

impl TraceOrder {
    /// SQL 排序片段（两列均为 DESC，避免调用方再拼）。
    pub fn order_clause(&self) -> &'static str {
        match self {
            TraceOrder::ReportedAt => "ORDER BY reported_at DESC, created_at DESC",
            TraceOrder::CreatedAt => "ORDER BY created_at DESC",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceFilter {
    pub session_id: Option<String>,
    pub eval_status: Option<EvalStatus>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
    /// `C5`：按 run_id 过滤。
    pub run_id: Option<String>,
    /// `C5`：按会话级终态摘要过滤。
    pub turn_status: Option<String>,
    /// `C5`：按 `reported_at` 时间窗下限（含）过滤。
    pub from: Option<DateTime<Utc>>,
    /// `C5`：按 `reported_at` 时间窗上限（含）过滤。
    pub to: Option<DateTime<Utc>>,
    /// `amend v1.5-2`：排序维度（`None` ⇒ `ReportedAt`）。
    #[serde(default)]
    pub order_by: Option<TraceOrder>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// ============================================================================
// issue 治理 DTO / 枚举（契约：`.dev-team/contract-issue-governance.md` FROZEN v1）
//
// **纯空桩**：本区段只有类型定义与文档注释，无任何实现逻辑。
// 业务落库、参数校验、序列化细节由 Wave-9 executor 在 repository/routes 实现。
// ============================================================================

/// `C3.2`：issue 列表归档过滤三态，默认 [`IssueArchiveMode::Exclude`]。
///
/// 序列化名：`exclude` / `include` / `only`（同时作为响应头 `X-Archived-Mode` 的取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueArchiveMode {
    /// 只看未归档（`archived_at IS NULL`）—— 默认。
    #[default]
    Exclude,
    /// 归档与未归档都包含（不加归档条件）。
    Include,
    /// 只看已归档（`archived_at IS NOT NULL`）。
    Only,
}

/// `C2.3`：批量归档请求体（`POST /api/v1/issues/archive`）。
///
/// 优先级（写死）：`ids` 非空时只按 `ids` 处理，`statuses` 与 `project` 被忽略。
/// `statuses` 白名单仅 `ignored` / `resolved`（默认 `["ignored","resolved"]`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueArchiveRequest {
    /// 项目名；缺省或空串 == 全项目。
    #[serde(default)]
    pub project: Option<String>,
    /// 归档目标状态白名单；缺省 `["ignored","resolved"]`，越界（含 `unresolved`/`in_progress`/`regression`）→ 400。
    ///
    /// 上限（`amend v1.1 / B-5`）：最多 2 项且不得重复，否则 → 400 `Bad request: invalid statuses`。
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    /// 仅归档 `last_seen_at < now - older_than_days` 的行；缺省不限。
    #[serde(default)]
    pub older_than_days: Option<i64>,
    /// 显式 id 列表；给出即优先于 `statuses`/`project`。
    ///
    /// 上限（`amend v1.1 / B-5`）：`dedup(ids).len() <= 1000`（超出 → 400 `Bad request: too many ids`），
    /// 单 id 长度 `<= 128` 字符。
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// `true` 只统计不改库（`archived` 恒为 0）。
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// `C2.4`：批量归档回显的生效条件。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueArchiveCriteria {
    /// 生效 project（空串归一为 `null`）。
    pub project: Option<String>,
    /// 生效状态白名单（缺省回显 `["ignored","resolved"]`）。
    pub statuses: Vec<String>,
    pub older_than_days: Option<i64>,
    pub ids: Option<Vec<String>>,
    /// `ids` 非空即 `true`（此时 `statuses`/`project` 被忽略）。
    pub ids_take_priority: bool,
}

/// `C2.4`：批量归档结果（字段名与类型写死）。
///
/// 计数口径（`amend v1.1 / A-3`）：
/// - `statuses` 模式：`matched ≡ archived`，`skipped_*` 恒 0；
/// - `ids` 模式：`matched = dedup(ids).len()`，其余三项为分桶结果。
///
/// 不变式（单写者/无并发）：`archived + skipped_not_archivable + skipped_already_archived
/// + skipped_missing == matched`；并发下只保证 `archived <= matched` 且 `archived + skipped_* <= matched`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueArchiveOutcome {
    pub dry_run: bool,
    /// 按模式的命中总数（见类型级文档）。
    pub matched: i64,
    /// 本次实际置 `archived_at` 的行数；`dry_run=true` 时恒为 0。
    pub archived: i64,
    pub skipped_already_archived: i64,
    /// 命中但 `status ∉ 生效白名单`（含 `unresolved`/`in_progress`/`regression`）。
    pub skipped_not_archivable: i64,
    /// `ids` 在库中不存在（`amend v1.1 / A-3`；仅 `ids` 模式非零）。
    pub skipped_missing: i64,
    pub criteria: IssueArchiveCriteria,
}

/// `C4.1`：单 issue 事件分页排序，默认 [`IssueEventsOrder::Desc`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueEventsOrder {
    /// `ORDER BY created_at DESC, id DESC`（默认）。
    #[default]
    Desc,
    /// `ORDER BY created_at ASC, id ASC`。
    Asc,
}

/// `C4.2`：事件分页游标载荷（解码后形态）。
///
/// 编码（`amend v1.1 / B-2`）：UTF-8 JSON `{"created_at": "<原始 RFC3339 串>", "id": "<string>"}`
/// → base64url（无 padding），即为不透明 `cursor` 串；
/// 解码失败/字段不符/长度 > 512 字符 → 400 `Bad request: invalid cursor`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueEventsCursor {
    /// `created_at` 的**原始 RFC3339 序列化串**（库内该行原值，禁止截断到微秒）。
    ///
    /// 方言比较：SQLite 对 TEXT 做字典序比较、PG 对 `timestamptz` 做时间比较，故必须原样携带。
    pub created_at: String,
    /// 事件 id（keyset 第二键，破同 `created_at` 平局）。
    pub id: String,
}

/// `C4.1`：`GET /api/v1/issues/:id/events` 的 Query 参数。
///
/// 校验归属 handler：`limit` 默认 20（`0`/负数 → 400，`>100` clamp 到 100）；
/// `order` 仅 `desc|asc`；`envelope` 仅 `0|1`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IssueEventsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub envelope: Option<String>,
}

/// `C4.3`：`?envelope=1` 时的事件分页信封。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueEventsEnvelope {
    pub items: Vec<Event>,
    /// 无下一页时为 `null`（此时不携带响应头 `X-Next-Cursor`）。
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub limit: i64,
    /// 该 issue 的事件**总行数**，与 `cursor`/`limit` 无关（`amend v1.1 / B-4`），
    /// 与响应头 `X-Total-Count` 相等。
    pub total: i64,
}

/// `C3.4`：`?envelope=1` 时回显的生效过滤条件（缺省 `null`，空串归一 `null`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueListFiltersEcho {
    pub project: Option<String>,
    pub status: Option<String>,
    pub level: Option<String>,
    pub environment: Option<String>,
    pub platform: Option<String>,
    pub release: Option<String>,
}

/// `C3.4`：`?envelope=1` 时的列表信封（默认响应体仍是 bare array，见 C3.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueListEnvelope {
    pub items: Vec<Issue>,
    /// 与响应头 `X-Total-Count` 相等，不随 `limit/offset` 变化。
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    /// 与响应头 `X-Has-More` 相等（`offset + items.len() < total`）。
    pub has_more: bool,
    /// 与响应头 `X-Archived-Mode` 相等。
    pub archived: IssueArchiveMode,
    pub filters: IssueListFiltersEcho,
}

/// `C5`：统计落桶粒度，默认 `day`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsBucket {
    #[default]
    Day,
    Hour,
}

/// `C5`：`/api/v1/stats` 的生效区间与粒度。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub bucket: StatsBucket,
}

/// `C5`：issue 维度统计（`by_*` 木桶之和必须等于 `total`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueStatsCounts {
    /// 过滤条件下 issues 行数（默认排除 archived）。
    pub total: i64,
    pub by_status: BTreeMap<String, i64>,
    pub by_level: BTreeMap<String, i64>,
    pub by_environment: BTreeMap<String, i64>,
}

/// `C5`：events 维度统计（**聚合口径与行口径并存，禁止混用**）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventStatsCounts {
    /// `SUM(issues.count)` —— 与生产验收值 185 一致。
    pub total: i64,
    /// 恒为 `"sum_issues_count"`。
    pub basis: String,
    /// `COUNT(*) FROM events`（同过滤条件）—— 与生产值 196 一致。
    pub stored_rows: i64,
    pub by_level: BTreeMap<String, i64>,
    pub by_environment: BTreeMap<String, i64>,
    pub by_release: BTreeMap<String, i64>,
}

/// `C5`：`series` 单点。`bucket` 为 `YYYY-MM-DD`（day）或 `YYYY-MM-DDTHH:00:00Z`（hour）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSeriesPoint {
    pub bucket: String,
    /// 按 `issues.first_seen_at` 落桶计数。
    pub issues: i64,
    /// 按 `events.created_at` 落桶计数（行口径）。
    pub events: i64,
}

/// `C5`：`GET /api/v1/stats` 的 200 响应（字段名与类型写死）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueStatsResponse {
    pub project: Option<String>,
    pub range: StatsRange,
    pub include_archived: bool,
    pub issues: IssueStatsCounts,
    pub events: EventStatsCounts,
    pub series: Vec<StatsSeriesPoint>,
}

/// `C5`：`GET /api/v1/stats` 的 Query 参数（校验归属 handler）。
///
/// `from`/`to` 为 RFC3339；`bucket` 仅 `day|hour`；`include_archived` 仅 `0|1`（默认 `0`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IssueStatsQuery {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub include_archived: Option<String>,
}

/// `C6.1`：治理指标时间窗，默认 `24h`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum IngestGovernanceWindow {
    #[default]
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "7d")]
    D7,
    #[serde(rename = "30d")]
    D30,
}

/// `C6.1`：`GET /api/v1/ingest/stats` 的 Query 参数（校验归属 handler）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IngestGovernanceQuery {
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
}

/// `C1.5`：`ingest_metrics.outcome` 白名单（PG 不加 CHECK 约束，由此枚举约束）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestMetricOutcome {
    /// 通过鉴权后的上报请求（401 不计）。
    Received,
    /// 新建事件（`is_duplicate == false`）。
    Created,
    /// `event_key` 幂等命中（`is_duplicate == true`）。
    Duplicate,
    /// 写入成功但该 issue 当时 `status == 'ignored'`（旁路分类，不改 ingest 行为）。
    SuppressedIgnored,
    /// 请求体不合法导致的 400/413。
    RejectedInvalid,
}

/// `C6.1`：治理指标总计（`accepted = created + duplicate`，派生不落库）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestGovernanceTotals {
    pub received: i64,
    pub accepted: i64,
    pub created: i64,
    pub duplicate: i64,
    pub suppressed_ignored: i64,
    pub rejected_invalid: i64,
}

/// `C6.1`：按 project 分组的治理计数（库内 `''` 映射为 `null`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestGovernanceProjectRow {
    pub project: Option<String>,
    pub received: i64,
    pub accepted: i64,
    pub suppressed_ignored: i64,
}

/// `C6.1`：治理指标时间序列表。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestGovernanceSeriesPoint {
    pub bucket: String,
    pub received: i64,
    pub accepted: i64,
    pub suppressed_ignored: i64,
}

/// `C6.1`：`GET /api/v1/ingest/stats` 的 200 响应（字段名与类型写死）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestGovernanceStats {
    /// `24h` | `7d` | `30d`。
    pub window: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub bucket: String,
    pub totals: IngestGovernanceTotals,
    /// 按 `project` 升序，`null` 排最后。
    pub by_project: Vec<IngestGovernanceProjectRow>,
    /// 按时间升序，空桶补 0。
    pub series: Vec<IngestGovernanceSeriesPoint>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_trace_json() -> serde_json::Value {
        serde_json::json!({
            "id": "trc-1",
            "session_id": "sess-1",
            "run_id": null,
            "turn_id": null,
            "environment": "production",
            "release": "dev",
            "eval_status": "unreviewed",
            "payload": {"turns": [{"turn_id": "t1", "status": "succeeded"}]},
            "total_input_tokens": 10,
            "total_output_tokens": 5,
            "total_duration_ms": 100,
            "reported_at": "2026-10-01T00:00:00Z",
            "created_at": "2026-10-01T00:00:00Z",
            "updated_at": "2026-10-01T00:00:00Z"
        })
    }

    #[test]
    fn trace_record_serde_defaults_keep_legacy_rows_decodable() {
        let record: TraceRecord =
            serde_json::from_value(legacy_trace_json()).expect("旧形态 JSON 必须仍可反序列化");
        assert_eq!(record.turn_status, "unknown", "C1.2：缺省 turn_status 必须是 unknown");
        assert_eq!(record.turns_count, 0);
        assert!(record.note.is_none());
    }

    #[test]
    fn trace_record_serializes_new_columns() {
        let record: TraceRecord = serde_json::from_value(legacy_trace_json()).expect("反序列化");
        let encoded = serde_json::to_value(&record).expect("序列化");
        assert_eq!(encoded["turn_status"], "unknown");
        assert_eq!(encoded["turns_count"], 0);
        assert!(encoded["note"].is_null());
    }

    #[test]
    fn trace_filter_new_dimensions_default_to_none() {
        let filter = TraceFilter::default();
        assert!(filter.run_id.is_none());
        assert!(filter.turn_status.is_none());
        assert!(filter.from.is_none());
        assert!(filter.to.is_none());
    }
}
