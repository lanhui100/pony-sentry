use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
