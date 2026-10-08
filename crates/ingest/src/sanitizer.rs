use crate::parser::{Exception, Frame, RawEvent};
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
    Regex::new(concat!(
        r"(?i)",
        r"(?:bearer\s+[a-zA-Z0-9_\-\.]{10,}|basic\s+[a-zA-Z0-9+/=]{10,})|",
        r"(?:ey[a-zA-Z0-9_-]{10,}\.ey[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,})|",
        r#"(?:"?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|passwd|auth[_-]?token)"?\s*[:=]\s*"?[^\s,;"']{4,}"?)|"#,
        r"(?:-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----[\s\S]*?-----END (?:[A-Z ]+ )?PRIVATE KEY-----)"
    )).unwrap()
});

static USER_PATH_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(/home/[^/\s]+|/root|/Users/[^/\s]+|[a-zA-Z]:\\(?:Users|Documents and Settings)\\[^\\]+)").unwrap()
});

pub struct SanitizationPipeline;

impl SanitizationPipeline {
    pub fn sanitize_event(mut event: RawEvent) -> RawEvent {
        // 1. 脱敏 message
        if let Some(msg) = event.message.take() {
            event.message = Some(Self::sanitize_string(&msg));
        }

        // 2. 脱敏 exception
        if let Some(ex) = event.exception.take() {
            event.exception = Some(Self::sanitize_exception(ex));
        }

        // 3. 脱敏 tags (HashMap<String, String>)
        if let Some(tags) = event.tags.take() {
            let mut sanitized_tags = std::collections::HashMap::with_capacity(tags.len());
            for (k, v) in tags {
                if Self::is_sensitive_key(&k) {
                    sanitized_tags.insert(k, "[REDACTED_SECRET]".to_string());
                } else {
                    sanitized_tags.insert(k, Self::sanitize_string(&v));
                }
            }
            event.tags = Some(sanitized_tags);
        }

        // 4. 脱敏 extra (serde_json::Value)
        if let Some(extra) = event.extra.take() {
            event.extra = Some(Self::sanitize_json_value(extra, 0));
        }

        // 5. 脱敏 breadcrumbs (Vec<serde_json::Value>)
        if let Some(breadcrumbs) = event.breadcrumbs.take() {
            let sanitized_breadcrumbs = breadcrumbs
                .into_iter()
                .map(|b| Self::sanitize_json_value(b, 0))
                .collect();
            event.breadcrumbs = Some(sanitized_breadcrumbs);
        }

        event
    }

    fn sanitize_exception(mut ex: Exception) -> Exception {
        if let Some(val) = ex.value.take() {
            ex.value = Some(Self::sanitize_string(&val));
        }
        if let Some(frames) = ex.stacktrace.take() {
            let sanitized_frames: Vec<Frame> = frames
                .into_iter()
                .map(|mut f| {
                    if let Some(fname) = f.filename.take() {
                        f.filename = Some(Self::sanitize_path(&fname));
                    }
                    if let Some(func) = f.function.take() {
                        f.function = Some(Self::sanitize_string(&func));
                    }
                    f
                })
                .collect();
            ex.stacktrace = Some(sanitized_frames);
        }
        ex
    }

    pub fn sanitize_json_value(value: Value, depth: usize) -> Value {
        if depth > MAX_RECURSION_DEPTH {
            return Value::String("[MAX_DEPTH_EXCEEDED]".to_string());
        }

        match value {
            Value::Object(map) => {
                let mut sanitized_map = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    if Self::is_sensitive_key(&k) {
                        sanitized_map.insert(k, Value::String("[REDACTED_SECRET]".to_string()));
                    } else {
                        sanitized_map.insert(k, Self::sanitize_json_value(v, depth + 1));
                    }
                }
                Value::Object(sanitized_map)
            }
            Value::Array(arr) => {
                let sanitized_arr = arr
                    .into_iter()
                    .map(|v| Self::sanitize_json_value(v, depth + 1))
                    .collect();
                Value::Array(sanitized_arr)
            }
            Value::String(s) => Value::String(Self::sanitize_string(&s)),
            other => other,
        }
    }

    #[inline]
    fn is_sensitive_key(key: &str) -> bool {
        let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
        if SENSITIVE_KEYS.contains(normalized.as_str()) {
            return true;
        }
        normalized.contains("password")
            || normalized.contains("token")
            || normalized.contains("secret")
            || normalized.contains("apikey")
            || normalized.contains("auth")
    }

    pub fn sanitize_string(input: &str) -> String {
        let masked = SECRET_PATTERNS.replace_all(input, "[REDACTED_SECRET]");
        Self::sanitize_path(&masked)
    }

    pub fn sanitize_path(input: &str) -> String {
        USER_PATH_REGEX.replace_all(input, "[USER_HOME]").to_string()
    }
}
