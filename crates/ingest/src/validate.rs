//! ingest v1 字段级校验与归一（契约 R2.2 / R2.3 / R2.5）。
//!
//! 分工原则：
//! - **可截断**的超长字段一律截断（不拒绝上报，避免因载荷膨胀丢事件）；
//! - **会改变幂等语义**的字段（`event_id`）超长/字符集越界直接 422，不截断；
//! - **结构非法**的可选字段（`grafana_url` / `loki_query` / `timestamp`）按契约分别处理：
//!   URL/查询丢弃并计数 WARN，时间戳非法返回 422（否则事件落库时间不可信）。

use chrono::{DateTime, Utc};

/// 字段级校验失败（映射为 `422 {"error":"invalid_field","field":"<name>"}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
}

impl FieldError {
    pub const fn new(field: &'static str) -> Self {
        Self { field }
    }
}

/// 合法事件等级（契约 R2）。
pub const VALID_LEVELS: [&str; 4] = ["fatal", "error", "warning", "info"];
/// 非法等级的统一降级值（契约 R2：非法值 MUST 降级为 `warning` 并 WARN 日志）。
pub const DEGRADED_LEVEL: &str = "warning";

pub const EVENT_ID_MAX_CHARS: usize = 64;
pub const MESSAGE_MAX_CHARS: usize = 2048;
pub const DIMENSION_MAX_CHARS: usize = 128;
pub const ERROR_TYPE_MAX_CHARS: usize = 256;
pub const SOURCE_KIND_MAX_CHARS: usize = 64;
pub const URL_MAX_CHARS: usize = 1024;
pub const TAG_KEY_MAX_CHARS: usize = 64;
pub const TAG_VALUE_MAX_CHARS: usize = 512;
pub const TAGS_MAX_ENTRIES: usize = 32;
pub const EXTRA_MAX_BYTES: usize = 8 * 1024;

/// 按字符（非字节）截断，避免切裂多字节字符。
pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        value.chars().take(max_chars).collect()
    }
}

/// 等级归一：大小写无关匹配；非法/缺失分别落 `error` 与降级 `warning`。
///
/// 返回值 `degraded = true` 表示输入非空且非法（调用方据此打 WARN 并计数）。
pub fn normalize_level(raw: Option<&str>) -> (String, bool) {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return ("error".to_string(), false);
    };
    let lower = raw.to_ascii_lowercase();
    if VALID_LEVELS.contains(&lower.as_str()) {
        (lower, false)
    } else {
        (DEGRADED_LEVEL.to_string(), true)
    }
}

/// `event_id` 严格校验（契约 R2.3）：≤64 字符且仅可打印 ASCII（拒绝空白/控制字符/非 ASCII）。
///
/// 不截断——截断会静默改变幂等键，破坏 R4 语义。
pub fn validate_event_id(raw: Option<String>) -> Result<Option<String>, FieldError> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > EVENT_ID_MAX_CHARS
        || !trimmed.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(FieldError::new("event_id"));
    }
    Ok(Some(trimmed.to_string()))
}

/// `timestamp` 严格校验（契约 R2 / R4）：ISO8601（RFC3339）可解析，否则 422。
pub fn parse_timestamp(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, FieldError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| Some(dt.with_timezone(&Utc)))
        .map_err(|_| FieldError::new("timestamp"))
}

/// `environment` 归一：trim + 截断；空值表示缺失（由仓储层落 `unknown`）。
pub fn normalize_dimension(raw: Option<&str>, max_chars: usize) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, max_chars))
}

/// `grafana_url` 结构校验（契约 R2.5）：scheme ∈ {http,https}。
/// 不合法返回 `None`（调用方丢弃字段并 WARN），不拒绝事件。
pub fn normalize_grafana_url(raw: Option<String>) -> Option<String> {
    let raw = raw
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let url = truncate_chars(&raw, URL_MAX_CHARS);
        // 去掉 scheme 后必须还有主机名
        let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or("");
        if rest.trim_matches('/').is_empty() {
            None
        } else {
            Some(url)
        }
    } else {
        None
    }
}

/// `loki_query` 结构校验（契约 R2.5）：以 `{` 开头、以 `}` 结尾、不含 `|` 管道。
/// 不合法返回 `None`（调用方丢弃字段并 WARN），不拒绝事件。
pub fn normalize_loki_query(raw: Option<String>) -> Option<String> {
    let raw = raw
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    if raw.starts_with('{') && raw.ends_with('}') && !raw.contains('|') {
        Some(truncate_chars(&raw, URL_MAX_CHARS))
    } else {
        None
    }
}

/// `source_kind` 归一：仅保留契约枚举值，其余视为未提供（不报错）。
pub fn normalize_source_kind(raw: Option<&str>) -> Option<String> {
    const VALID: [&str; 5] = ["log", "agent", "backend_exception", "frontend", "alert"];
    let value = raw.map(str::trim).filter(|s| !s.is_empty())?;
    let lower = value.to_ascii_lowercase();
    if VALID.contains(&lower.as_str()) {
        Some(lower)
    } else {
        Some(truncate_chars(value, SOURCE_KIND_MAX_CHARS))
    }
}

/// `tags` 归一：键/值截断、最多 [`TAGS_MAX_ENTRIES`] 项（按 key 字典序取前 N，保证确定性）。
pub fn normalize_tags(
    tags: Option<std::collections::HashMap<String, String>>,
) -> Option<std::collections::HashMap<String, String>> {
    let tags = tags?;
    if tags.is_empty() {
        return Some(tags);
    }
    let mut entries: Vec<(String, String)> = tags
        .into_iter()
        .map(|(k, v)| {
            (
                truncate_chars(&k, TAG_KEY_MAX_CHARS),
                truncate_chars(&v, TAG_VALUE_MAX_CHARS),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.truncate(TAGS_MAX_ENTRIES);
    Some(entries.into_iter().collect())
}

/// `extra` 体量守卫：序列化超过 [`EXTRA_MAX_BYTES`] 时替换为可审计的占位对象。
/// 返回 `true` 表示发生裁剪（调用方 WARN）。
pub fn guard_extra(extra: Option<serde_json::Value>) -> (Option<serde_json::Value>, bool) {
    let Some(extra) = extra else {
        return (None, false);
    };
    let size = serde_json::to_vec(&extra)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if size <= EXTRA_MAX_BYTES {
        (Some(extra), false)
    } else {
        (
            Some(serde_json::json!({
                "_truncated": true,
                "_original_bytes": size,
                "_limit_bytes": EXTRA_MAX_BYTES,
            })),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn level_defaults_to_error_and_degrades_invalid() {
        assert_eq!(normalize_level(None), ("error".to_string(), false));
        assert_eq!(
            normalize_level(Some("  ")),
            ("error".to_string(), false),
            "空白等价缺失"
        );
        assert_eq!(
            normalize_level(Some("FATAL")),
            ("fatal".to_string(), false),
            "大小写无关"
        );
        assert_eq!(
            normalize_level(Some("banana")),
            ("warning".to_string(), true),
            "非法值降级 warning 并标记"
        );
    }

    #[test]
    fn event_id_rejects_overlong_and_non_graphic() {
        assert_eq!(validate_event_id(None).unwrap(), None);
        assert_eq!(validate_event_id(Some("  ".into())).unwrap(), None);
        assert_eq!(
            validate_event_id(Some("a".repeat(64))).unwrap(),
            Some("a".repeat(64))
        );
        assert_eq!(
            validate_event_id(Some("a".repeat(65))).unwrap_err().field,
            "event_id"
        );
        assert_eq!(
            validate_event_id(Some("has space".into()))
                .unwrap_err()
                .field,
            "event_id"
        );
        assert_eq!(
            validate_event_id(Some("中文".into())).unwrap_err().field,
            "event_id"
        );
    }

    #[test]
    fn timestamp_requires_rfc3339() {
        assert!(parse_timestamp(None).unwrap().is_none());
        let ts = parse_timestamp(Some("2026-10-21T07:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(ts.to_rfc3339(), "2026-10-21T07:00:00+00:00");
        assert_eq!(
            parse_timestamp(Some("2026-10-21 07:00")).unwrap_err().field,
            "timestamp"
        );
    }

    #[test]
    fn grafana_url_scheme_whitelist() {
        assert_eq!(
            normalize_grafana_url(Some("https://grafana.ponyjob.top/d/x".into())),
            Some("https://grafana.ponyjob.top/d/x".to_string())
        );
        assert_eq!(
            normalize_grafana_url(Some(
                "http://grafana.monitoring.svc.cluster.local/d/x".into()
            )),
            Some("http://grafana.monitoring.svc.cluster.local/d/x".to_string()),
            "集群内 http 链接不得被丢弃"
        );
        assert_eq!(
            normalize_grafana_url(Some("javascript:alert(1)".into())),
            None
        );
        assert_eq!(normalize_grafana_url(Some("https://".into())), None);
    }

    #[test]
    fn loki_query_structure() {
        assert_eq!(
            normalize_loki_query(Some("{app=\"api_v2\"}".into())),
            Some("{app=\"api_v2\"}".to_string())
        );
        assert_eq!(
            normalize_loki_query(Some("{app=\"api_v2\"} | line_format".into())),
            None
        );
        assert_eq!(normalize_loki_query(Some("app=api_v2".into())), None);
        assert_eq!(normalize_loki_query(Some("{app=\"x\"".into())), None);
    }

    #[test]
    fn tags_truncated_deterministically() {
        let mut tags = HashMap::new();
        for i in 0..40 {
            tags.insert(format!("k{i:02}"), "v".repeat(600));
        }
        let normalized = normalize_tags(Some(tags)).unwrap();
        assert_eq!(normalized.len(), TAGS_MAX_ENTRIES);
        assert!(normalized.contains_key("k00"));
        assert!(!normalized.contains_key("k39"));
        assert_eq!(normalized["k00"].chars().count(), TAG_VALUE_MAX_CHARS);
    }

    #[test]
    fn extra_over_limit_is_replaced() {
        let big = serde_json::json!({ "blob": "x".repeat(EXTRA_MAX_BYTES + 1) });
        let (guarded, truncated) = guard_extra(Some(big));
        assert!(truncated);
        assert_eq!(guarded.unwrap()["_truncated"], true);

        let small = serde_json::json!({"k": "v"});
        let (guarded, truncated) = guard_extra(Some(small.clone()));
        assert!(!truncated);
        assert_eq!(guarded.unwrap(), small);
    }

    #[test]
    fn dimensions_trim_and_truncate() {
        assert_eq!(normalize_dimension(Some("  "), 128), None);
        assert_eq!(
            normalize_dimension(Some(" production "), 128),
            Some("production".to_string())
        );
        assert_eq!(
            normalize_dimension(Some(&"x".repeat(200)), 128)
                .unwrap()
                .chars()
                .count(),
            128
        );
    }
}
