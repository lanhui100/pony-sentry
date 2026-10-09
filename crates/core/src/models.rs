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
    pub reported_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceFilter {
    pub session_id: Option<String>,
    pub eval_status: Option<EvalStatus>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

