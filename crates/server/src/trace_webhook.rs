//! `M-03`（契约 v1.5-10）：失败态 trace 的可选异步 Webhook 通知。
//!
//! 设计约束（契约原文）：
//! - 仅在配置 `TRACE_WEBHOOK_URL` 时启用；未配置时**零副作用**（不建客户端、不建任务）；
//! - 仅 `turn_status ∈ {failed,error,cancelled,timed_out}` 触发；
//! - 去重键 `project|tool_name|turn_status`，10 分钟内只投递一次（进程内 `Mutex<HashMap>`）；
//! - 单次投递 5s 超时；失败只 `warn!`，**不重试**（旁路通知，绝不影响 ingest 主链路）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// 单次投递硬超时（契约 v1.5-10）。
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);
/// 去重窗口：同一 `project|tool_name|turn_status` 10 分钟内只发一次。
const DEDUPE_WINDOW: Duration = Duration::from_secs(600);
/// 去重表容量上限（防无界增长；超限先按窗口清理，仍超限则整体清空）。
const DEDUPE_MAX_ENTRIES: usize = 1024;
/// 触发通知的终态集合（契约 v1.5-10）。
const NOTIFIABLE_TURN_STATUSES: [&str; 4] = ["failed", "error", "cancelled", "timed_out"];

/// 进程级失败态 trace 通知器（`AppState.notifier`）。
#[derive(Debug, Default)]
pub struct TraceNotifier {
    url: Option<String>,
    /// 去重表：key = `project|tool_name|turn_status`，value = 上次投递时刻。
    last_sent: Mutex<HashMap<String, Instant>>,
}

impl TraceNotifier {
    /// 构造；空串 / 空白 URL 视作未配置（fail-open：不通知）。
    pub fn new(url: Option<String>) -> Self {
        Self {
            url: url.filter(|value| !value.trim().is_empty()),
            last_sent: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.url.is_some()
    }

    /// 该终态是否需要通知（未配置 URL 时恒 false）。
    pub fn should_notify(&self, turn_status: &str) -> bool {
        self.is_enabled()
            && NOTIFIABLE_TURN_STATUSES
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(turn_status.trim()))
    }

    /// 去重窗口内是否可投递（并登记本次投递时刻）。
    fn claim(&self, key: &str, now: Instant) -> bool {
        let Ok(mut table) = self.last_sent.lock() else {
            return false;
        };
        if let Some(previous) = table.get(key) {
            if now.duration_since(*previous) < DEDUPE_WINDOW {
                return false;
            }
        }
        if table.len() >= DEDUPE_MAX_ENTRIES && !table.contains_key(key) {
            table.retain(|_, sent_at| now.duration_since(*sent_at) < DEDUPE_WINDOW);
            if table.len() >= DEDUPE_MAX_ENTRIES {
                table.clear();
            }
        }
        table.insert(key.to_string(), now);
        true
    }

    /// fire-and-forget 投递：5s 超时、失败只 warn、不重试、不阻塞调用方。
    ///
    /// 未配置 URL / 去重命中 / 无运行时（同步上下文）时静默返回。
    pub fn notify(&self, dedupe_key: String, body: serde_json::Value) {
        let Some(url) = self.url.clone() else {
            return;
        };
        if !self.claim(&dedupe_key, Instant::now()) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("trace webhook skipped: no tokio runtime in current context");
            return;
        };
        handle.spawn(async move {
            let client = match reqwest::Client::builder().timeout(WEBHOOK_TIMEOUT).build() {
                Ok(client) => client,
                Err(err) => {
                    warn!(error = %err, "trace webhook client build failed");
                    return;
                }
            };
            match client.post(&url).json(&body).send().await {
                Ok(response) if response.status().is_success() => {
                    info!(status = %response.status(), "trace webhook delivered");
                }
                Ok(response) => {
                    warn!(status = %response.status(), "trace webhook rejected (no retry)");
                }
                Err(err) => {
                    warn!(error = %err, "trace webhook delivery failed (no retry)");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_without_url_and_empty_string_counts_as_disabled() {
        let none = TraceNotifier::new(None);
        assert!(!none.is_enabled());
        assert!(!none.should_notify("failed"));
        let blank = TraceNotifier::new(Some("   ".to_string()));
        assert!(!blank.is_enabled());
    }

    #[test]
    fn only_failure_terminal_states_are_notifiable() {
        let notifier = TraceNotifier::new(Some("http://127.0.0.1:1/hook".to_string()));
        for status in ["failed", "error", "cancelled", "timed_out", "FAILED"] {
            assert!(notifier.should_notify(status), "`{status}` 应触发通知");
        }
        for status in ["succeeded", "completed", "running", "unknown", "other", ""] {
            assert!(!notifier.should_notify(status), "`{status}` 不应触发通知");
        }
    }

    #[test]
    fn dedupe_window_suppresses_repeats_and_expires() {
        let notifier = TraceNotifier::new(Some("http://127.0.0.1:1/hook".to_string()));
        let key = "job_copilot|format_jd_to_json|failed";
        let start = Instant::now();
        assert!(notifier.claim(key, start), "首次应放行");
        assert!(
            !notifier.claim(key, start + Duration::from_secs(599)),
            "10 分钟内应被去重"
        );
        assert!(
            notifier.claim(key, start + DEDUPE_WINDOW),
            "超过 10 分钟窗口后应再次放行"
        );
        assert!(
            notifier.claim("other|tool|failed", start + DEDUPE_WINDOW),
            "不同 key 互不影响"
        );
    }

    #[test]
    fn dedupe_table_is_bounded() {
        let notifier = TraceNotifier::new(Some("http://127.0.0.1:1/hook".to_string()));
        let now = Instant::now();
        for index in 0..(DEDUPE_MAX_ENTRIES + 32) {
            notifier.claim(&format!("p|t{index}|failed"), now);
        }
        let table = notifier.last_sent.lock().unwrap();
        assert!(
            table.len() <= DEDUPE_MAX_ENTRIES,
            "去重表必须有界（实际 {}）",
            table.len()
        );
    }
}
