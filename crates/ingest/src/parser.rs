use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub filename: Option<String>,
    pub function: Option<String>,
    pub lineno: Option<u32>,
    pub colno: Option<u32>,
    pub in_app: Option<bool>,
}

/// `exception.stacktrace` 的两种合法形态（契约 R3.0 / R3.2）：
/// - `Frame[]`：结构化栈帧（历史形态）；
/// - 字符串：客户端已有的 traceback 文本（消费侧 `stacktrace_redacted`），
///   归一后文本落 `extra.stacktrace_text`，并尽力解析首帧用于指纹与 culprit。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Stacktrace {
    Frames(Vec<Frame>),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exception {
    pub error_type: String,
    pub value: Option<String>,
    pub stacktrace: Option<Stacktrace>,
}

/// 上报事件的完整语义面（ingest v1）。
///
/// 一级扩展字段（`event_id`/`level`/`namespace`/`app`/`component`/`service_name`/
/// `error_type`/`source_kind`/`grafana_url`/`loki_query`/`timestamp`）全部可选，
/// 缺省与历史行为一致；`RawEvent` 会原样序列化进 `events.payload`，
/// 因此这些字段随事件载荷可追溯（契约 R2 / R3.2）。
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
    /// 客户端确定性幂等键（≤64，落 `events.event_key`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// 事件等级 `fatal|error|warning|info`（非法值已由上报侧降级为 `warning`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// 一级错误类型（缺省取 `exception.error_type`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// 来源通道 `log|agent|backend_exception|frontend|alert`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    /// 仅当通过 R2.5 结构校验时保留。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grafana_url: Option<String>,
    /// 仅当通过 R2.5 结构校验时保留。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loki_query: Option<String>,
    /// 事件发生时间（ISO8601 原文，解析在 `validate` 层完成）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// 关联 `trace_id`（一级形态）。历史上报只把它放在 `extra.trace_id`，由
    /// [`normalize_trace_id`] 回填，因此下游（含读取侧）只需认这一个来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// 字符串栈帧文本的最大留存长度（契约 R3.0：`extra.stacktrace_text` ≤5000 字符）。
pub const STACKTRACE_TEXT_MAX_CHARS: usize = 5000;

/// 首帧解析正则（契约 R3.3）：
/// 1. `path:line[:col] in func`（Rust/Python 通用写法）；
/// 2. Python traceback `File "path", line N, in func`。
static FRAME_COLON_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<file>[\w./-]+):(?P<line>\d+)(:\d+)? in (?P<func>\S+)").expect("valid regex")
});
static FRAME_PYTHON_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"File "(?P<file>[^"]+)", line (?P<line>\d+), in (?P<func>\S+)"#)
        .expect("valid regex")
});

/// 从字符串栈帧文本尽力解析首帧；解析失败返回 `None`（契约 R3.0 要求不得报错）。
pub fn parse_first_frame(text: &str) -> Option<Frame> {
    let caps = FRAME_COLON_IN
        .captures(text)
        .or_else(|| FRAME_PYTHON_FILE.captures(text))?;
    let filename = caps.name("file").map(|m| m.as_str().to_string());
    let function = caps.name("func").map(|m| m.as_str().to_string());
    let lineno = caps
        .name("line")
        .and_then(|m| m.as_str().parse::<u32>().ok());
    if filename.is_none() && function.is_none() {
        return None;
    }
    Some(Frame {
        filename,
        function,
        lineno,
        colno: None,
        in_app: None,
    })
}

/// 把字符串形态栈帧归一为 `Frame[]` 形态，并把原文本存入 `extra.stacktrace_text`
/// （契约 R3.0 / R3.2）：
/// - 文本截断到 [`STACKTRACE_TEXT_MAX_CHARS`]；
/// - 首帧解析成功则构造单元素 `Frame[]` 用于 culprit 与指纹；失败则为空栈帧；
/// - 不修改已结构化（`Frame[]`）的上报，保持原始形态可追溯。
pub fn normalize_stacktrace(mut event: RawEvent) -> RawEvent {
    let Some(exception) = event.exception.take() else {
        return event;
    };
    let Exception {
        error_type,
        value,
        stacktrace,
    } = exception;
    let normalized = match stacktrace {
        Some(Stacktrace::Text(text)) => {
            let stored: String = text.chars().take(STACKTRACE_TEXT_MAX_CHARS).collect();
            let extra = event
                .extra
                .take()
                .map(|value| match value {
                    serde_json::Value::Object(map) => serde_json::Value::Object(map),
                    other => serde_json::json!({ "original_extra": other }),
                })
                .unwrap_or_else(|| serde_json::json!({}));
            let extra = match extra {
                serde_json::Value::Object(mut map) => {
                    map.insert(
                        "stacktrace_text".to_string(),
                        serde_json::Value::String(stored.clone()),
                    );
                    serde_json::Value::Object(map)
                }
                other => other,
            };
            event.extra = Some(extra);
            let frames = parse_first_frame(&stored)
                .into_iter()
                .collect::<Vec<Frame>>();
            Stacktrace::Frames(frames)
        }
        other => other.unwrap_or(Stacktrace::Frames(Vec::new())),
    };
    event.exception = Some(Exception {
        error_type,
        value,
        stacktrace: Some(normalized),
    });
    event
}

/// 兼容路径：历史上报把 `trace_id` 放在 `extra.trace_id`（发送侧契约），这里回填到一级字段，
/// 使后续处理与读取侧只需要认 `trace_id` 一个来源；一级字段已有非空值时**不覆盖**（新形态优先）。
pub fn normalize_trace_id(mut event: RawEvent) -> RawEvent {
    let top_level = event
        .trace_id
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let from_extra = event
        .extra
        .as_ref()
        .and_then(|extra| extra.get("trace_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    event.trace_id = top_level.or(from_extra);
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colon_in_form() {
        let frame = parse_first_frame("at crates/core/src/main.rs:42:7 in handle")
            .expect("must parse colon form");
        assert_eq!(frame.filename.as_deref(), Some("crates/core/src/main.rs"));
        assert_eq!(frame.function.as_deref(), Some("handle"));
        assert_eq!(frame.lineno, Some(42));
    }

    #[test]
    fn parses_python_traceback_form() {
        let text = "Traceback (most recent call last):\n  File \"api_v2/src/x.py\", line 12, in run\nValueError: boom";
        let frame = parse_first_frame(text).expect("must parse python form");
        assert_eq!(frame.filename.as_deref(), Some("api_v2/src/x.py"));
        assert_eq!(frame.function.as_deref(), Some("run"));
        assert_eq!(frame.lineno, Some(12));
    }

    #[test]
    fn unparsable_text_yields_no_frame_without_error() {
        assert!(parse_first_frame("no frame info here").is_none());
    }

    #[test]
    fn string_stacktrace_becomes_extra_text_and_single_frame() {
        let event = RawEvent {
            platform: "python".into(),
            release: None,
            environment: Some("dev".into()),
            message: None,
            exception: Some(Exception {
                error_type: "RuntimeError".into(),
                value: Some("boom".into()),
                stacktrace: Some(Stacktrace::Text(
                    "File \"api_v2/src/x.py\", line 7, in run\nRuntimeError: boom".into(),
                )),
            }),
            tags: None,
            extra: None,
            breadcrumbs: None,
            event_id: Some("evt-1".into()),
            level: Some("error".into()),
            namespace: None,
            app: None,
            component: None,
            service_name: None,
            error_type: None,
            source_kind: None,
            grafana_url: None,
            loki_query: None,
            timestamp: None,
            trace_id: None,
        };

        let normalized = normalize_stacktrace(event);
        let extra = normalized.extra.expect("extra must be created");
        assert_eq!(
            extra["stacktrace_text"].as_str(),
            Some("File \"api_v2/src/x.py\", line 7, in run\nRuntimeError: boom")
        );
        match normalized.exception.unwrap().stacktrace.unwrap() {
            Stacktrace::Frames(frames) => {
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0].filename.as_deref(), Some("api_v2/src/x.py"));
                assert_eq!(frames[0].function.as_deref(), Some("run"));
            }
            Stacktrace::Text(_) => panic!("must normalize to frames"),
        }
    }

    #[test]
    fn frame_array_shape_is_preserved() {
        let event = RawEvent {
            platform: "rust".into(),
            release: None,
            environment: None,
            message: None,
            exception: Some(Exception {
                error_type: "Panic".into(),
                value: None,
                stacktrace: Some(Stacktrace::Frames(vec![Frame {
                    filename: Some("src/main.rs".into()),
                    function: Some("main".into()),
                    lineno: Some(1),
                    colno: None,
                    in_app: Some(true),
                }])),
            }),
            tags: None,
            extra: None,
            breadcrumbs: None,
            event_id: None,
            level: None,
            namespace: None,
            app: None,
            component: None,
            service_name: None,
            error_type: None,
            source_kind: None,
            grafana_url: None,
            loki_query: None,
            timestamp: None,
            trace_id: None,
        };
        let normalized = normalize_stacktrace(event);
        assert!(
            normalized.extra.is_none(),
            "结构化形态不得注入 stacktrace_text"
        );
        match normalized.exception.unwrap().stacktrace.unwrap() {
            Stacktrace::Frames(frames) => assert_eq!(frames.len(), 1),
            Stacktrace::Text(_) => panic!("frame array must stay frames"),
        }
    }

    /// 新上报形态：一级 `trace_id` 必须能被解析进 `RawEvent`（读取侧透出的前提）。
    #[test]
    fn parses_top_level_trace_id() {
        let event: RawEvent = serde_json::from_value(serde_json::json!({
            "platform": "python",
            "trace_id": "0123456789abcdef0123456789abcdef"
        }))
        .expect("top-level trace_id must parse");
        let normalized = normalize_trace_id(event);
        assert_eq!(
            normalized.trace_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    /// 历史形态兼容：只放 `extra.trace_id` 上报时，归一必须回填一级字段并 trim。
    #[test]
    fn backfills_trace_id_from_extra() {
        let event: RawEvent = serde_json::from_value(serde_json::json!({
            "platform": "python",
            "extra": { "trace_id": " fedcba9876543210fedcba9876543210 " }
        }))
        .expect("extra-only trace_id must parse");
        assert!(
            event.trace_id.is_none(),
            "extra 形态不得在解析期凭空产生一级字段"
        );
        let normalized = normalize_trace_id(event);
        assert_eq!(
            normalized.trace_id.as_deref(),
            Some("fedcba9876543210fedcba9876543210"),
            "extra.trace_id 必须回填到一级字段（去空白后）"
        );
    }

    /// 两种形态同时存在时以一级字段为准：新上报不得被历史 extra 覆盖。
    #[test]
    fn top_level_trace_id_wins_over_extra() {
        let event: RawEvent = serde_json::from_value(serde_json::json!({
            "platform": "python",
            "trace_id": "0123456789abcdef0123456789abcdef",
            "extra": { "trace_id": "fedcba9876543210fedcba9876543210" }
        }))
        .expect("both forms must parse");
        let normalized = normalize_trace_id(event);
        assert_eq!(
            normalized.trace_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    /// 无 trace_id 时不得落键（payload 保持既有形态，读取侧按「键存在」判断能力）。
    #[test]
    fn missing_trace_id_leaves_no_payload_key() {
        let event: RawEvent = serde_json::from_value(serde_json::json!({ "platform": "rust" }))
            .expect("minimal event must parse");
        let normalized = normalize_trace_id(event);
        assert!(normalized.trace_id.is_none());
        let payload = serde_json::to_value(&normalized).expect("event must serialize");
        assert!(
            payload.get("trace_id").is_none(),
            "无 trace_id 时 payload 不得出现空键：{payload}"
        );
    }
}
