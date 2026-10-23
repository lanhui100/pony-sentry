use crate::parser::{Exception, RawEvent, Stacktrace};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::LazyLock;

const MAX_RECURSION_DEPTH: usize = 32;

static SENSITIVE_KEYS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut s = HashSet::new();
    s.insert("password");
    s.insert("passwd");
    s.insert("secret");
    s.insert("token");
    s.insert("api_key");
    s.insert("apikey");
    s.insert("access_token");
    s.insert("refresh_token");
    s.insert("authorization");
    s.insert("auth");
    s.insert("cookie");
    s.insert("set-cookie");
    s.insert("private_key");
    s.insert("privatekey");
    s.insert("credentials");
    s.insert("credential");
    s
});

static SECRET_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    // `amend v1.5-1`（M-04）：键可带单/双引号，键值分隔可用 `:` 或 `=`（两侧允许空白），
    // 值可为「带引号（≥1 字符）」或「裸值（≥4 字符）」；显式列入裸 `token`。
    // 白名单语义不变：`*tokens` / `token_count` 等计量字段不会被命中（见 `is_sensitive_key`
    // 与下方单测：`total_input_tokens=12345` 保持原样）。
    Regex::new(concat!(
        r"(?i)",
        r"(?:bearer\s+[a-zA-Z0-9_\-\.]{10,}|basic\s+[a-zA-Z0-9+/=]{10,})|",
        r"(?:ey[a-zA-Z0-9_-]{10,}\.ey[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,})|",
        // `amend v1.6-1`（L3A-E1）：值必须闭合到**同类引号**；双引号值内允许单引号、单引号值内允许
        // 双引号，转义序列（`\"`/`\'`）整体吞掉，避免 `password="ab'cd"` 提前闭合导致部分泄漏。
        r#"(?:["']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|passwd|auth[_-]?token|token)["']?\s*[:=]\s*(?:"(?:[^"\\]|\\.){1,}"|'(?:[^'\\]|\\.){1,}'|[^\s,;"']{4,}))|"#,
        r"(?:-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----[\s\S]*?-----END (?:[A-Z ]+ )?PRIVATE KEY-----)"
    )).unwrap()
});

static USER_PATH_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(/home/[^/\s]+|/root|/Users/[^/\s]+|[a-zA-Z]:\\(?:Users|Documents and Settings)\\[^\\]+)").unwrap()
});

/// 脱敏计数（`amend v1.5-5` / M-04）：供 routes 注入 `payload._redactions={"secrets":n,"paths":n}`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedactionCounts {
    /// 命中 `SECRET_PATTERNS` 的次数（含敏感键整值替换）。
    pub secrets: u64,
    /// 命中用户家目录/路径正则的次数。
    pub paths: u64,
}

impl RedactionCounts {
    pub fn merge(&mut self, other: Self) {
        self.secrets += other.secrets;
        self.paths += other.paths;
    }

    pub fn is_empty(&self) -> bool {
        self.secrets == 0 && self.paths == 0
    }
}

pub struct SanitizationPipeline;

impl SanitizationPipeline {
    pub fn sanitize_event(event: RawEvent) -> RawEvent {
        Self::sanitize_event_counted(event).0
    }

    /// `sanitize_event` 的计数版本（事件路径，`amend v1.5-5`）。
    pub fn sanitize_event_counted(mut event: RawEvent) -> (RawEvent, RedactionCounts) {
        let mut counts = RedactionCounts::default();
        // 1. 脱敏 message
        if let Some(msg) = event.message.take() {
            let (sanitized, inner) = Self::sanitize_string_counted(&msg);
            counts.merge(inner);
            event.message = Some(sanitized);
        }

        // 2. 脱敏 exception
        if let Some(ex) = event.exception.take() {
            let (sanitized, inner) = Self::sanitize_exception_counted(ex);
            counts.merge(inner);
            event.exception = Some(sanitized);
        }

        // 3. 脱敏 tags (HashMap<String, String>)
        if let Some(tags) = event.tags.take() {
            let mut sanitized_tags = std::collections::HashMap::with_capacity(tags.len());
            for (k, v) in tags {
                if Self::is_sensitive_key(&k) {
                    counts.secrets += 1;
                    sanitized_tags.insert(k, "[REDACTED_SECRET]".to_string());
                } else {
                    let (sanitized, inner) = Self::sanitize_string_counted(&v);
                    counts.merge(inner);
                    sanitized_tags.insert(k, sanitized);
                }
            }
            event.tags = Some(sanitized_tags);
        }

        // 4. 脱敏 extra (serde_json::Value)
        if let Some(extra) = event.extra.take() {
            let (sanitized, inner) = Self::sanitize_json_value_counted(extra, 0);
            counts.merge(inner);
            event.extra = Some(sanitized);
        }

        // 5. 脱敏 breadcrumbs (Vec<serde_json::Value>)
        if let Some(breadcrumbs) = event.breadcrumbs.take() {
            let mut sanitized_breadcrumbs = Vec::with_capacity(breadcrumbs.len());
            for breadcrumb in breadcrumbs {
                let (sanitized, inner) = Self::sanitize_json_value_counted(breadcrumb, 0);
                counts.merge(inner);
                sanitized_breadcrumbs.push(sanitized);
            }
            event.breadcrumbs = Some(sanitized_breadcrumbs);
        }

        (event, counts)
    }

    fn sanitize_exception_counted(mut ex: Exception) -> (Exception, RedactionCounts) {
        let mut counts = RedactionCounts::default();
        if let Some(val) = ex.value.take() {
            let (sanitized, inner) = Self::sanitize_string_counted(&val);
            counts.merge(inner);
            ex.value = Some(sanitized);
        }
        if let Some(stacktrace) = ex.stacktrace.take() {
            // 两种形态都要脱敏：结构化栈帧逐帧清理路径/函数名；
            // 字符串形态（客户端 traceback 文本）整体走字符串脱敏，保留首帧可解析性。
            ex.stacktrace = Some(match stacktrace {
                Stacktrace::Frames(frames) => Stacktrace::Frames(
                    frames
                        .into_iter()
                        .map(|mut f| {
                            if let Some(fname) = f.filename.take() {
                                let (sanitized, path_hits) = Self::sanitize_path_counted(&fname);
                                counts.paths += path_hits;
                                f.filename = Some(sanitized);
                            }
                            if let Some(func) = f.function.take() {
                                let (sanitized, inner) = Self::sanitize_string_counted(&func);
                                counts.merge(inner);
                                f.function = Some(sanitized);
                            }
                            f
                        })
                        .collect(),
                ),
                Stacktrace::Text(text) => {
                    let (sanitized, inner) = Self::sanitize_string_counted(&text);
                    counts.merge(inner);
                    Stacktrace::Text(sanitized)
                }
            });
        }
        (ex, counts)
    }

    pub fn sanitize_json_value(value: Value, depth: usize) -> Value {
        Self::sanitize_json_value_counted(value, depth).0
    }

    /// 递归脱敏并累计计数（对象敏感键计 1 次 secret；字符串命中正则按次累计）。
    pub fn sanitize_json_value_counted(value: Value, depth: usize) -> (Value, RedactionCounts) {
        if depth > MAX_RECURSION_DEPTH {
            return (
                Value::String("[MAX_DEPTH_EXCEEDED]".to_string()),
                RedactionCounts::default(),
            );
        }

        match value {
            Value::Object(map) => {
                let mut counts = RedactionCounts::default();
                let mut sanitized_map = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    if Self::is_sensitive_key(&k) {
                        counts.secrets += 1;
                        sanitized_map.insert(k, Value::String("[REDACTED_SECRET]".to_string()));
                    } else {
                        let (sanitized, inner) = Self::sanitize_json_value_counted(v, depth + 1);
                        counts.merge(inner);
                        sanitized_map.insert(k, sanitized);
                    }
                }
                (Value::Object(sanitized_map), counts)
            }
            Value::Array(arr) => {
                let mut counts = RedactionCounts::default();
                let mut sanitized_arr = Vec::with_capacity(arr.len());
                for item in arr {
                    let (sanitized, inner) = Self::sanitize_json_value_counted(item, depth + 1);
                    counts.merge(inner);
                    sanitized_arr.push(sanitized);
                }
                (Value::Array(sanitized_arr), counts)
            }
            Value::String(s) => {
                let (sanitized, counts) = Self::sanitize_string_counted(&s);
                (Value::String(sanitized), counts)
            }
            other => (other, RedactionCounts::default()),
        }
    }

    #[inline]
    fn is_sensitive_key(key: &str) -> bool {
        let lower = key.to_ascii_lowercase();
        // 计量与度量指标白名单（LLM tokens 计量字段严禁误当作鉴权 token 脱敏）
        if lower.ends_with("tokens")
            || lower.contains("token_count")
            || lower.contains("tokencount")
        {
            return false;
        }

        let normalized = lower.replace(['-', '_'], "");
        if SENSITIVE_KEYS.contains(normalized.as_str()) {
            return true;
        }
        normalized.contains("password")
            || normalized.contains("token")
            || normalized.contains("secret")
            || normalized.contains("apikey")
            || normalized.contains("auth")
    }

    /// 字符串脱敏（返回计数版本，`amend v1.5-5` / M-04）。
    pub fn sanitize_string_counted(input: &str) -> (String, RedactionCounts) {
        let mut secrets = 0u64;
        let masked = SECRET_PATTERNS
            .replace_all(input, |_: &regex::Captures| {
                secrets += 1;
                "[REDACTED_SECRET]"
            })
            .to_string();
        let mut paths = 0u64;
        let output = USER_PATH_REGEX
            .replace_all(&masked, |_: &regex::Captures| {
                paths += 1;
                "[USER_HOME]"
            })
            .to_string();
        (output, RedactionCounts { secrets, paths })
    }

    pub fn sanitize_string(input: &str) -> String {
        Self::sanitize_string_counted(input).0
    }

    pub fn sanitize_path_counted(input: &str) -> (String, u64) {
        let mut paths = 0u64;
        let output = USER_PATH_REGEX
            .replace_all(input, |_: &regex::Captures| {
                paths += 1;
                "[USER_HOME]"
            })
            .to_string();
        (output, paths)
    }

    pub fn sanitize_path(input: &str) -> String {
        Self::sanitize_path_counted(input).0
    }

    /// **HTTP 侧接口（task-18 约定）**：对 trace payload 整包脱敏并返回计数，
    /// 供 `routes` 注入 `payload._redactions={"secrets":n,"paths":n}`（`amend v1.5-5`）。
    pub fn sanitize_trace_value(value: &serde_json::Value) -> (serde_json::Value, RedactionCounts) {
        Self::sanitize_json_value_counted(value.clone(), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `amend v1.5-1`（M-04）：5 种密钥写法都必须命中。
    #[test]
    fn m04_redacts_five_secret_spellings() {
        let cases = [
            ("password=hunter2", "password=hunter2"),
            (r#""password": "hunter2""#, r#""password": "hunter2""#),
            ("'password': 'hunter2'", "'password': 'hunter2'"),
            ("token=abc123def", "token=abc123def"),
            ("'api_key' = 'sk-live-1234'", "'api_key' = 'sk-live-1234'"),
        ];
        for (input, _) in cases {
            let (output, counts) =
                SanitizationPipeline::sanitize_string_counted(input);
            assert!(
                output.contains("[REDACTED_SECRET]"),
                "写法未被脱敏：{input} → {output}"
            );
            assert!(
                !output.contains("hunter2") && !output.contains("abc123def"),
                "明文密钥残留：{input} → {output}"
            );
            assert_eq!(counts.secrets, 1, "写法 {input} 应命中 1 次：{output}");
        }
    }

    /// 白名单：计量字段（`*tokens` / `token_count`）不得被误脱敏。
    #[test]
    fn m04_token_metrics_are_not_redacted() {
        for input in [
            "total_input_tokens=12345",
            "cache_hit_tokens: 23936",
            "token_count=7",
            "input_tokens: 5000, output_tokens: 800",
        ] {
            let (output, counts) = SanitizationPipeline::sanitize_string_counted(input);
            assert_eq!(output, input, "计量字段被误改：{input} → {output}");
            assert_eq!(counts.secrets, 0);
        }
    }

    /// `amend v1.6-1`（L3A-E1）反例：值内出现另一种引号/转义引号时必须整值脱敏，不得提前闭合。
    #[test]
    fn e1_quoted_values_close_on_same_quote_only() {
        let cases = [
            (r#"password="ab'cd""#, "'cd"),
            (r#"password='ab"cd'"#, "ab"),
            (r#"password: "p@ss\"x""#, "p@ss"),
        ];
        for (input, must_not_leak) in cases {
            let (output, counts) = SanitizationPipeline::sanitize_string_counted(input);
            assert!(
                output.contains("[REDACTED_SECRET]"),
                "反例未被脱敏：{input} → {output}"
            );
            assert!(
                !output.contains(must_not_leak),
                "引号值提前闭合导致泄漏（`{must_not_leak}`）：{input} → {output}"
            );
            assert_eq!(counts.secrets, 1, "反例 {input} 应命中 1 次：{output}");
        }
    }

    /// `sanitize_trace_value`（HTTP 侧约定接口）：整包脱敏 + 计数。
    #[test]
    fn m04_sanitize_trace_value_counts_secrets_and_paths() {
        let payload = json!({
            "turns": [
                {
                    "turn_id": "t1",
                    "status": "failed",
                    "tool_calls": [{
                        "tool_call_id": "c1",
                        "output": "Traceback File \"/home/alice/app/x.py\", line 7\nRuntimeError: password=hunter2"
                    }]
                }
            ],
            "extra": {"token": "abc123def"}
        });
        let (sanitized, counts) = SanitizationPipeline::sanitize_trace_value(&payload);
        let text = sanitized.to_string();
        assert!(!text.contains("hunter2"), "trace 内密钥必须脱敏：{text}");
        assert!(!text.contains("alice"), "trace 内路径必须脱敏：{text}");
        assert!(text.contains("[REDACTED_SECRET]"));
        assert!(text.contains("[USER_HOME]"));
        assert!(counts.secrets >= 2, "secrets 计数：{counts:?}");
        assert!(counts.paths >= 1, "paths 计数：{counts:?}");
        assert!(!counts.is_empty());
    }
}
