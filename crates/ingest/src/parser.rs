use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub filename: Option<String>,
    pub function: Option<String>,
    pub lineno: Option<u32>,
    pub colno: Option<u32>,
    pub in_app: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exception {
    pub error_type: String,
    pub value: Option<String>,
    pub stacktrace: Option<Vec<Frame>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub platform: String,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub message: Option<String>,
    pub exception: Option<Exception>,
    pub tags: Option<std::collections::HashMap<String, String>>,
    pub extra: Option<serde_json::Value>,
    pub breadcrumbs: Option<Vec<serde_json::Value>>,
}
