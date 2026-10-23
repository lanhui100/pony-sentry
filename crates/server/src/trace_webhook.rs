//! `M-03`（契约 v1.5-10）+ `L3B-E4` 加固（契约 v1.6-9）：失败态 trace 的可选异步 Webhook 通知。
//!
//! 设计约束：
//! - 仅在配置 `TRACE_WEBHOOK_URL` 时启用；未配置时**零副作用**（不建客户端、不建任务）；
//! - 仅 `turn_status ∈ {failed,error,cancelled,timed_out}` 触发；
//! - 去重键 `project|tool_name|turn_status`，各分量归一（截断 64 + 字符集），10 分钟内只投递一次；
//! - 去重表**双上限**（条目 1024 / 总字节 256KB），满时**淘汰最旧**（禁止 `clear`，防"清表即击穿"）；
//! - 投递并发上限 8（`Semaphore`）；permit 生命周期覆盖整个请求；
//! - 复用单个 `reqwest::Client`（5s 超时）；失败只 `warn!`，**不重试**；
//! - 日志只打印 URL 的 `scheme://host`，绝不打印路径/查询串（防 token 泄漏）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

/// 单次投递硬超时（契约 v1.5-10）。
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);
/// 去重窗口：同一 `project|tool_name|turn_status` 10 分钟内只发一次。
const DEDUPE_WINDOW: Duration = Duration::from_secs(600);
/// 去重表条目上限（契约 v1.6-9）。
const DEDUPE_MAX_ENTRIES: usize = 1024;
/// 去重表总字节上限（键长度累计；契约 v1.6-9 的 256KB）。
const DEDUPE_MAX_BYTES: usize = 256 * 1024;
/// 去重键单分量最大字符数（契约 v1.6-9）。
const DEDUPE_KEY_PART_MAX_CHARS: usize = 64;
/// 投递并发上限（契约 v1.6-9）。
const WEBHOOK_MAX_IN_FLIGHT: usize = 8;
/// 触发通知的终态集合（契约 v1.5-10）。
const NOTIFIABLE_TURN_STATUSES: [&str; 4] = ["failed", "error", "cancelled", "timed_out"];

/// 去重表项：上次投递时刻 + 该键占用的字节数。
#[derive(Debug, Clone, Copy)]
struct DedupeEntry {
    sent_at: Instant,
    bytes: usize,
}

/// 去重表（条目 + 字节双上限，过载淘汰最旧）。
#[derive(Debug, Default)]
struct DedupeTable {
    entries: HashMap<String, DedupeEntry>,
    total_bytes: usize,
}

impl DedupeTable {
    /// 该键是否仍在窗口内（只读判定，不登记）。
    fn is_recent(&self, key: &str, now: Instant) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| now.duration_since(entry.sent_at) < DEDUPE_WINDOW)
    }

    /// 登记一次投递，并在超上限时淘汰最旧条目（**不做整体清空**）。
    fn insert(&mut self, key: &str, now: Instant) {
        let bytes = key.len();
        if let Some(previous) = self.entries.insert(
            key.to_string(),
            DedupeEntry {
                sent_at: now,
                bytes,
            },
        ) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.bytes);
        }
        self.total_bytes += bytes;
        while self.entries.len() > DEDUPE_MAX_ENTRIES || self.total_bytes > DEDUPE_MAX_BYTES {
            // 淘汰最旧：按 sent_at 取最小；空表则退出（防御性）。
            let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.sent_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest_key) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            } else {
                break;
            }
        }
    }
}

/// 归一单个去重键分量：截断 64 字符 + 非 `[A-Za-z0-9_.:-]` 替换为 `_`。
fn normalize_key_part(value: &str) -> String {
    let truncated: String = value
        .trim()
        .chars()
        .take(DEDUPE_KEY_PART_MAX_CHARS)
        .collect();
    let normalized: String = truncated
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}

/// 日志安全的 URL 描述：只保留 `scheme://host[:port]`，路径与查询串一律丢弃。
fn describe_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("unknown-host");
            match parsed.port() {
                Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
                None => format!("{}://{}", parsed.scheme(), host),
            }
        }
        Err(_) => "<unparsable-url>".to_string(),
    }
}

/// 进程级失败态 trace 通知器（`AppState.notifier`）。
#[derive(Debug)]
pub struct TraceNotifier {
    url: Option<String>,
    /// 日志安全的 URL 摘要（`scheme://host[:port]`），构造期算好，避免每次投递解析。
    url_digest: String,
    dedupe: Mutex<DedupeTable>,
    /// 投递并发闸门（≤8）；permit 覆盖整个 HTTP 请求生命周期。
    permits: Arc<Semaphore>,
    /// 复用的 HTTP 客户端（首次投递时惰性构建，全进程共用）。
    client: OnceLock<reqwest::Client>,
}

impl Default for TraceNotifier {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TraceNotifier {
    /// 构造；空串 / 空白 URL 视作未配置（fail-open：不通知）。
    pub fn new(url: Option<String>) -> Self {
        let url = url.filter(|value| !value.trim().is_empty());
        let url_digest = url.as_deref().map(describe_url).unwrap_or_default();
        Self {
            url,
            url_digest,
            dedupe: Mutex::new(DedupeTable::default()),
            permits: Arc::new(Semaphore::new(WEBHOOK_MAX_IN_FLIGHT)),
            client: OnceLock::new(),
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

    /// 构造去重键：各分量归一后以 `|` 连接（`project|tool_name|turn_status`）。
    pub fn dedupe_key(project: &str, tool_name: &str, turn_status: &str) -> String {
        format!(
            "{}|{}|{}",
            normalize_key_part(project),
            normalize_key_part(tool_name),
            normalize_key_part(turn_status)
        )
    }

    /// 窗口内判定 + 登记（测试与投递共用）。
    fn claim(&self, key: &str, now: Instant) -> bool {
        let Ok(mut table) = self.dedupe.lock() else {
            return false;
        };
        if table.is_recent(key, now) {
            return false;
        }
        table.insert(key, now);
        true
    }

    /// 当前去重表规模（条目数、总字节），供测试与运维观测。
    pub fn dedupe_size(&self) -> (usize, usize) {
        self.dedupe
            .lock()
            .map(|table| (table.entries.len(), table.total_bytes))
            .unwrap_or((0, 0))
    }

    /// 复用的 HTTP 客户端（惰性初始化，5s 超时）。
    fn http_client(&self) -> reqwest::Client {
        self.client
            .get_or_init(|| {
                reqwest::Client::builder()
                    .timeout(WEBHOOK_TIMEOUT)
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new())
            })
            .clone()
    }

    /// fire-and-forget 投递：并发 ≤8、5s 超时、失败只 warn、不重试、不阻塞调用方。
    ///
    /// 未配置 URL / 去重命中 / 无 tokio 运行时（同步上下文）时静默返回。
    pub fn notify(&self, dedupe_key: String, body: serde_json::Value) {
        let Some(url) = self.url.clone() else {
            return;
        };
        if !self.claim(&dedupe_key, Instant::now()) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                host = %self.url_digest,
                "trace webhook skipped: no tokio runtime in current context"
            );
            return;
        };
        let permits = self.permits.clone();
        let client = self.http_client();
        let digest = self.url_digest.clone();
        handle.spawn(async move {
            // permit 生命周期覆盖整个请求（含 body 读取），把在飞请求限制在 8 个以内。
            let _permit: OwnedSemaphorePermit = match permits.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    warn!(host = %digest, "trace webhook skipped: limiter closed");
                    return;
                }
            };
            match client.post(&url).json(&body).send().await {
                Ok(response) if response.status().is_success() => {
                    info!(host = %digest, status = %response.status(), "trace webhook delivered");
                }
                Ok(response) => {
                    warn!(
                        host = %digest,
                        status = %response.status(),
                        "trace webhook rejected (no retry)"
                    );
                }
                Err(err) => {
                    // 只记录 host + 错误类型：reqwest 错误文本可能含完整 URL（含 query/token）。
                    warn!(
                        host = %digest,
                        error = %err,
                        "trace webhook delivery failed (no retry)"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifier() -> TraceNotifier {
        TraceNotifier::new(Some("http://127.0.0.1:1/hook".to_string()))
    }

    #[test]
    fn disabled_without_url_and_empty_string_counts_as_disabled() {
        assert!(!TraceNotifier::new(None).is_enabled());
        assert!(!TraceNotifier::new(Some("   ".to_string())).is_enabled());
        assert!(!TraceNotifier::new(None).should_notify("failed"));
    }

    #[test]
    fn only_failure_terminal_states_are_notifiable() {
        let notifier = notifier();
        for status in ["failed", "error", "cancelled", "timed_out", "FAILED"] {
            assert!(notifier.should_notify(status), "`{status}` 应触发通知");
        }
        for status in ["succeeded", "completed", "running", "unknown", "other", ""] {
            assert!(!notifier.should_notify(status), "`{status}` 不应触发通知");
        }
    }

    #[test]
    fn dedupe_window_suppresses_repeats_and_expires() {
        let notifier = notifier();
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
    fn entries_cap_evicts_oldest_and_keeps_recent_dedupe() {
        let notifier = notifier();
        let start = Instant::now();
        for index in 0..=(DEDUPE_MAX_ENTRIES + 32) {
            assert!(
                notifier.claim(
                    &format!("p|tool{index}|failed"),
                    start + Duration::from_secs(index as u64)
                ),
                "顺序插入的新 key 必须放行"
            );
        }
        let (entries, _) = notifier.dedupe_size();
        assert!(
            entries <= DEDUPE_MAX_ENTRIES,
            "条目数必须有界（实际 {entries}）"
        );
        // 未被淘汰的新 key 仍在窗口内 → 必须继续被去重（不能击穿）
        let latest = DEDUPE_MAX_ENTRIES + 32;
        assert!(
            !notifier.claim(
                &format!("p|tool{latest}|failed"),
                start + Duration::from_secs(latest as u64 + 1)
            ),
            "未淘汰的新 key 仍必须在窗口内被去重（不能击穿）"
        );
        // 最旧 key 已被淘汰 → 可再次放行（证明是"淘汰最旧"而不是"清表"）
        assert!(
            notifier.claim(
                "p|tool0|failed",
                start + Duration::from_secs(latest as u64 + 1)
            ),
            "最旧 key 被淘汰后应可再次放行"
        );
    }

    #[test]
    fn bytes_cap_evicts_oldest_and_never_clears_table() {
        let notifier = notifier();
        let start = Instant::now();
        let long_part = "z".repeat(DEDUPE_KEY_PART_MAX_CHARS);
        // 每个 key 长度 ≈ 130 字节 → 约 2000 条即可越过 256KB
        for index in 0..2200 {
            notifier.claim(
                &format!("{long_part}|{long_part}|k{index:05}"),
                start + Duration::from_secs(index as u64),
            );
            let (_, bytes) = notifier.dedupe_size();
            assert!(
                bytes <= DEDUPE_MAX_BYTES,
                "总字节必须有界（越界 index={index}，实际 {bytes}）"
            );
        }
        let (entries, bytes) = notifier.dedupe_size();
        assert!(entries > 0, "禁止整体清空（条目数 {entries}）");
        assert!(
            entries <= DEDUPE_MAX_ENTRIES && bytes <= DEDUPE_MAX_BYTES,
            "双上限必须同时受控（entries={entries}, bytes={bytes}）"
        );
    }

    #[test]
    fn dedupe_key_parts_are_normalized_and_truncated() {
        let key =
            TraceNotifier::dedupe_key(&"p".repeat(200), "tool name/with spaces?x=1", "failed\n");
        let parts: Vec<&str> = key.split('|').collect();
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(
                part.chars().count() <= DEDUPE_KEY_PART_MAX_CHARS,
                "分量必须截断到 64 字符：{part}"
            );
            assert!(
                part.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-')),
                "分量字符集必须归一：{part}"
            );
        }
        assert_eq!(parts[1], "tool_name_with_spaces_x_1");
        assert_eq!(parts[2], "failed");
    }

    #[test]
    fn url_digest_never_exposes_path_or_query() {
        let digest = describe_url("https://hooks.example.com:8443/path/to/hook?token=secret123");
        assert_eq!(digest, "https://hooks.example.com:8443");
        assert!(!digest.contains("token"), "日志摘要不得含 query：{digest}");
        assert!(!digest.contains("/path"), "日志摘要不得含路径：{digest}");
        assert_eq!(
            describe_url("http://127.0.0.1:9/hook"),
            "http://127.0.0.1:9"
        );
        assert_eq!(describe_url("not a url"), "<unparsable-url>");
    }

    #[test]
    fn concurrent_claims_keep_table_bounded() {
        let notifier = Arc::new(notifier());
        let start = Instant::now();
        let mut handles = Vec::new();
        for worker in 0..100u32 {
            let notifier = notifier.clone();
            handles.push(std::thread::spawn(move || {
                for index in 0..200u32 {
                    let key = TraceNotifier::dedupe_key(
                        &format!("proj-{worker}-{index}"),
                        "tool",
                        "failed",
                    );
                    notifier.claim(&key, start);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("并发线程 panic");
        }
        let (entries, bytes) = notifier.dedupe_size();
        assert!(
            entries <= DEDUPE_MAX_ENTRIES && bytes <= DEDUPE_MAX_BYTES,
            "100×200 并发登记后仍须受控（entries={entries}, bytes={bytes}）"
        );
    }
}
