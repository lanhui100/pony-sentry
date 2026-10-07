use crate::parser::{Frame, RawEvent};
use regex::Regex;
use std::sync::LazyLock;

static TOKEN_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(bearer\s+[a-zA-Z0-9_\-\.]{10,}|token[:=]\s*[a-zA-Z0-9_\-\.]{8,}|password[:=]\s*[^\s,;]+)")
        .unwrap()
});

static USER_PATH_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(/home/[^/]+|/Users/[^/]+|C:\\Users\\[^\\]+)").unwrap()
});

pub struct SanitizationPipeline;

impl SanitizationPipeline {
    pub fn sanitize_event(mut event: RawEvent) -> RawEvent {
        // 1. 脱敏 message
        if let Some(msg) = event.message.take() {
            event.message = Some(Self::sanitize_string(&msg));
        }

        // 2. 脱敏 exception
        if let Some(mut ex) = event.exception.take() {
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
                        f
                    })
                    .collect();
                ex.stacktrace = Some(sanitized_frames);
            }
            event.exception = Some(ex);
        }

        event
    }

    pub fn sanitize_string(input: &str) -> String {
        let masked = TOKEN_REGEX.replace_all(input, "[REDACTED_SECRET]");
        Self::sanitize_path(&masked)
    }

    pub fn sanitize_path(input: &str) -> String {
        USER_PATH_REGEX.replace_all(input, "[USER_HOME]").to_string()
    }
}
