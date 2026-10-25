use crate::session::{
    csrf_token_from_header, session_token_from_bearer, session_token_from_cookie, SessionOrigin,
    SessionStore,
};
use crate::trace_ingest::TraceIngestSwitch;
use crate::trace_webhook::TraceNotifier;
use crate::trust::{is_proxied, normalize_peer, TrustConfig, TrustDecision};
use axum::{
    body::Body,
    extract::{
        rejection::JsonRejection, ConnectInfo, DefaultBodyLimit, Extension, Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use pony_sentry_core::{
    models::{
        EvalStatus, IngestGovernanceQuery, IssueArchiveRequest, IssueStatsQuery, IssueStatus,
        TraceFilter, TraceOrder, TraceRecord,
    },
    repository::{IssueFilter, IssueRepository, NewEventRecord, TraceRepository},
};
use pony_sentry_ingest::{
    compute_issue_fingerprint, guard_extra, infer_project_name, normalize_dimension,
    normalize_environment, normalize_grafana_url, normalize_level, normalize_loki_query,
    normalize_source_kind, normalize_stacktrace, normalize_tags, parse_timestamp, truncate_chars,
    validate_event_id, RawEvent, SanitizationPipeline, MESSAGE_MAX_CHARS,
};
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tracing::{error, info, warn};
use uuid::Uuid;

/// 单次 DB 探活的硬超时（契约 R11.2 / NFR `db_probe_timeout_ms`）。
const DB_PROBE_TIMEOUT: Duration = Duration::from_millis(2000);

/// `F1`：通用错误响应文案（契约 v1.4-7：DB/serde 原文只进日志，绝不外发）。
const BAD_REQUEST_BODY: &str = "Bad request";
const INTERNAL_ERROR_BODY: &str = "Internal error";

/// `L3A-E2`（契约 v1.6-2）：服务端保留载荷键——仅这三个由服务端注入；
/// 客户端传入一律先剥离（深层嵌套同剥），其余未知键（含 `_sdk` 等）全部保留到 `_unmapped`。
const RESERVED_PAYLOAD_KEYS: [&str; 3] = ["_redactions", "_warnings", "_unmapped"];
/// `L3A-E2`：递归剥离保留键的深度上限（防恶意深嵌套栈溢出）。
const RESERVED_KEY_STRIP_MAX_DEPTH: usize = 32;

/// 递归剥离任意层级中的服务端保留键（`_redactions`/`_warnings`/`_unmapped`）。
fn strip_reserved_payload_keys(value: &mut serde_json::Value, depth: usize) {
    if depth > RESERVED_KEY_STRIP_MAX_DEPTH {
        return;
    }
    match value {
        serde_json::Value::Object(object) => {
            for key in RESERVED_PAYLOAD_KEYS {
                object.remove(key);
            }
            for (_, child) in object.iter_mut() {
                strip_reserved_payload_keys(child, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                strip_reserved_payload_keys(item, depth + 1);
            }
        }
        _ => {}
    }
}

/// `W-05`（契约 v1.5-2）：`reported_at_ms` 与服务器时间的允许偏差（超过即 400）。
const REPORTED_AT_MAX_SKEW_MS: i64 = 24 * 60 * 60 * 1000;

/// `F2`：指标 label 的最大字符数（`project` 截断，防无界基数）。
const METRIC_LABEL_MAX_CHARS: usize = 64;
/// `F2`：单个指标族的 series 上限，超出折叠进 `overflow`。
const METRIC_SERIES_MAX: usize = 512;
/// `F2`：series 溢出后的折叠 label。
const METRIC_OVERFLOW_LABEL: &str = "overflow";
/// `F2`：`turn_status` 受控集合（契约 v1.4-6）；集合外一律归 `other`。
const CONTROLLED_TURN_STATUSES: [&str; 13] = [
    "completed",
    "succeeded",
    "failed",
    "error",
    "cancelled",
    "interrupted",
    "timed_out",
    "max_tokens",
    "aborted",
    "partial",
    "running",
    "unknown",
    "other",
];

/// `F2`：指标 label 归一——按字符截断到 64，非 `[A-Za-z0-9_.:-]` 一律替换为 `_`。
fn normalize_metric_label(value: &str) -> String {
    let raw = value.trim();
    let truncated: String = raw.chars().take(METRIC_LABEL_MAX_CHARS).collect();
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

/// `F2`：`turn_status` label 收敛到受控集合（大小写不敏感），集合外 → `other`。
fn normalize_turn_status_label(status: &str) -> String {
    let lowered = status.trim().to_ascii_lowercase();
    CONTROLLED_TURN_STATUSES
        .iter()
        .copied()
        .find(|candidate| *candidate == lowered)
        .unwrap_or("other")
        .to_string()
}

/// `F2`：带 series 上限的计数累加——满额后折叠进 `overflow`，且全进程只告警一次。
fn increment_capped<K: Ord + Clone>(
    map: &mut BTreeMap<K, u64>,
    key: K,
    overflow_key: K,
    warned: &std::sync::atomic::AtomicBool,
) {
    if let Some(counter) = map.get_mut(&key) {
        *counter += 1;
        return;
    }
    if map.len() >= METRIC_SERIES_MAX {
        *map.entry(overflow_key).or_insert(0) += 1;
        if !warned.swap(true, Ordering::Relaxed) {
            warn!(
                limit = METRIC_SERIES_MAX,
                "trace metric series limit reached; folding further labels into `overflow`"
            );
        }
        return;
    }
    map.insert(key, 1);
}

/// `F6`：请求体反序列化失败统一 `Bad request`（保留 413 的传输层语义）。
fn json_rejection_response(rejection: JsonRejection) -> (StatusCode, String) {
    let status = rejection.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return (status, "Payload too large".to_string());
    }
    (StatusCode::BAD_REQUEST, BAD_REQUEST_BODY.to_string())
}

/// `F1`：把仓库/序列化错误统一收口为 `Internal error`——原文只进日志，绝不进响应体。
fn internal_error<E: std::fmt::Display>(context: &'static str, err: E) -> (StatusCode, String) {
    error!(error = %err, context, "internal error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        INTERNAL_ERROR_BODY.to_string(),
    )
}

/// ingest 结果计数器（`/metrics` 的 `pony_sentry_ingest_requests_total` 数据源）。
///
/// `C7`：同一结构顺带承载 trace 维度四个新计数器（进程内、`/metrics` 单点读取，
/// 与既有 issue 计数同寿命）。label 组合用 `BTreeMap` 累加，渲染时天然按 key 升序，
/// 保证 `/metrics` 输出稳定可比对（无 HashMap 随机序）。
/// `F2`：label 归一 + series 上限，`/metrics` 渲染持锁迭代（不整表 clone）。
#[derive(Debug, Default)]
pub struct IngestMetrics {
    created: AtomicU64,
    duplicate: AtomicU64,
    degraded_level: AtomicU64,
    /// `pony_sentry_traces_total{project,turn_status}`：trace 上报成功计数。
    traces_total: Mutex<BTreeMap<(String, String), u64>>,
    /// `pony_sentry_trace_turns_total{status}`：入 turn 计数（按归一 status）。
    turns_total: Mutex<BTreeMap<String, u64>>,
    /// `pony_sentry_trace_status_conflicts_total`：`C4` status_conflict 命中的上报次数。
    status_conflicts: AtomicU64,
    /// `pony_sentry_trace_ingest_rejected_total{reason}`：trace 上报被拒计数。
    ingest_rejected: Mutex<BTreeMap<String, u64>>,
    /// `F2`：series 溢出告警「只告警一次」的闸门（跨 map 共享）。
    series_overflow_warned: std::sync::atomic::AtomicBool,
    /// `I-06`（契约 v1.5-8）：每次 ingest 累加持久化行 payload 的字节数。
    ///
    /// 合并丢失 / 文本截断两个计数由 core 进程级原子量持有（`trace_merge_lost_total()` /
    /// `trace_truncated_total()`），`/metrics` 渲染时直接读取，避免双份计数漂移。
    trace_payload_bytes_total: AtomicU64,
}

impl IngestMetrics {
    fn record_created(&self) {
        self.created.fetch_add(1, Ordering::Relaxed);
    }
    fn record_duplicate(&self) {
        self.duplicate.fetch_add(1, Ordering::Relaxed);
    }
    /// 非法 level 降级计数（契约 R2：非法值降级为 `warning` 并 WARN 日志）。
    fn record_degraded_level(&self) {
        self.degraded_level.fetch_add(1, Ordering::Relaxed);
    }
    /// `C7`：一次成功 trace 上报（聚合行 + 本次入 turn 的状态分布）。
    ///
    /// `F2`：`project` 归一（截断 64 + 字符集替换）、`turn_status` 收敛到受控集合；
    /// series 超上限折叠 `overflow`（只告警一次）。
    fn record_trace_ingested(&self, project: &str, turn_status: &str, turn_statuses: &[String]) {
        let project_label = normalize_metric_label(project);
        let status_label = normalize_turn_status_label(turn_status);
        if let Ok(mut map) = self.traces_total.lock() {
            increment_capped(
                &mut map,
                (project_label, status_label),
                (
                    METRIC_OVERFLOW_LABEL.to_string(),
                    METRIC_OVERFLOW_LABEL.to_string(),
                ),
                &self.series_overflow_warned,
            );
        }
        if let Ok(mut map) = self.turns_total.lock() {
            for status in turn_statuses {
                increment_capped(
                    &mut map,
                    normalize_turn_status_label(status),
                    METRIC_OVERFLOW_LABEL.to_string(),
                    &self.series_overflow_warned,
                );
            }
        }
    }
    /// `C7`：本次上报命中 `C4` status_conflict（成功状态 + 失败文案）记一次。
    fn record_trace_status_conflict(&self) {
        self.status_conflicts.fetch_add(1, Ordering::Relaxed);
    }
    /// `C7`：trace 上报被拒（`session_missing` / `invalid_params`）。
    fn record_trace_ingest_rejected(&self, reason: &str) {
        if let Ok(mut map) = self.ingest_rejected.lock() {
            increment_capped(
                &mut map,
                normalize_metric_label(reason),
                METRIC_OVERFLOW_LABEL.to_string(),
                &self.series_overflow_warned,
            );
        }
    }
    /// `I-06`：一次 ingest 的持久化 payload 体积。
    fn record_trace_payload_bytes(&self, payload_bytes: u64) {
        self.trace_payload_bytes_total
            .fetch_add(payload_bytes, Ordering::Relaxed);
    }

    /// `C7` + `F2`：直接把 trace 指标写入 Prometheus 文本（持锁迭代，不整表 clone）。
    ///
    /// 锁序固定为 traces → turns → rejected，三段之间不嵌套，故与写入侧不会死锁。
    fn render_trace_metrics(&self, body: &mut String) {
        body.push_str(
            "# HELP pony_sentry_traces_total trace 上报成功计数（按 project 与 turn_status 维度；每次 ingest 成功 +1）\n\
             # TYPE pony_sentry_traces_total counter\n",
        );
        if let Ok(map) = self.traces_total.lock() {
            for ((project, turn_status), count) in map.iter() {
                body.push_str(&format!(
                    "pony_sentry_traces_total{{project=\"{}\",turn_status=\"{}\"}} {}\n",
                    escape_label(project),
                    escape_label(turn_status),
                    count
                ));
            }
        }
        body.push_str(
            "# HELP pony_sentry_trace_turns_total 上报 turn 计数（按归一 status 维度；每次 ingest 按 payload.turns 逐条累加）\n\
             # TYPE pony_sentry_trace_turns_total counter\n",
        );
        if let Ok(map) = self.turns_total.lock() {
            for (status, count) in map.iter() {
                body.push_str(&format!(
                    "pony_sentry_trace_turns_total{{status=\"{}\"}} {}\n",
                    escape_label(status),
                    count
                ));
            }
        }
        body.push_str(
            "# HELP pony_sentry_trace_status_conflicts_total 「成功状态 + 失败文案」矛盾上报次数（C4 status_conflict）\n\
             # TYPE pony_sentry_trace_status_conflicts_total counter\n",
        );
        body.push_str(&format!(
            "pony_sentry_trace_status_conflicts_total {}\n",
            self.status_conflicts.load(Ordering::Relaxed)
        ));
        body.push_str(
            "# HELP pony_sentry_trace_ingest_rejected_total trace 上报被拒计数（按 reason 维度）\n\
             # TYPE pony_sentry_trace_ingest_rejected_total counter\n",
        );
        if let Ok(map) = self.ingest_rejected.lock() {
            for (reason, count) in map.iter() {
                body.push_str(&format!(
                    "pony_sentry_trace_ingest_rejected_total{{reason=\"{}\"}} {}\n",
                    escape_label(reason),
                    count
                ));
            }
        }
        // `I-06`（契约 v1.5-8）：payload 体积 / 截断 / 合并丢失三个计数器。
        body.push_str(
            "# HELP pony_sentry_trace_payload_bytes_total 上报体积（客户端内容序列化字节数，非存储体积；每次 ingest 累加）\n\
             # TYPE pony_sentry_trace_payload_bytes_total counter\n",
        );
        body.push_str(&format!(
            "pony_sentry_trace_payload_bytes_total {}\n",
            self.trace_payload_bytes_total.load(Ordering::Relaxed)
        ));
        body.push_str(
            "# HELP pony_sentry_trace_merge_lost_turns_total 合并期丢失的 turn 数（core 进程级计数）\n\
             # TYPE pony_sentry_trace_merge_lost_turns_total counter\n",
        );
        body.push_str(&format!(
            "pony_sentry_trace_merge_lost_turns_total {}\n",
            pony_sentry_core::trace_v2::trace_merge_lost_total()
        ));
        body.push_str(
            "# HELP pony_sentry_trace_truncated_total 截断字段数（tool_call 的 output/arguments_summary 超限；v1.6-4 按字段累计）\n\
             # TYPE pony_sentry_trace_truncated_total counter\n",
        );
        body.push_str(&format!(
            "pony_sentry_trace_truncated_total {}\n",
            pony_sentry_core::trace_v2::trace_truncated_total()
        ));
    }
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.created.load(Ordering::Relaxed),
            self.duplicate.load(Ordering::Relaxed),
            self.degraded_level.load(Ordering::Relaxed),
        )
    }
}

#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn IssueRepository>,
    pub trace_repo: Option<Arc<dyn TraceRepository>>,
    pub client_token: Option<String>,
    /// 管理侧只读/PATCH 令牌（契约 R10）；未配置时回退 `client_token`，两者皆无则 fail-closed 503。
    pub admin_token: Option<String>,
    pub webhook_url: Option<String>,
    /// `METRICS_TOKEN`（可选）：为空时 `/metrics` 保持免鉴权（探针/README 现状）；非空时要求
    /// `Authorization: Bearer <token>` 或 `x-metrics-token: <token>`，否则 401。
    pub metrics_token: Option<String>,
    /// `M-03`（契约 v1.5-10）：失败态 trace 的可选异步 webhook 通知器。
    pub notifier: Arc<TraceNotifier>,
    pub metrics: Arc<IngestMetrics>,
    /// 控制台服务端会话（HttpOnly Cookie / 短期 Bearer 令牌）。
    pub sessions: Arc<SessionStore>,
}

impl AppState {
    pub fn new(repo: Arc<dyn IssueRepository>) -> Self {
        Self {
            repo,
            trace_repo: None,
            client_token: None,
            admin_token: None,
            webhook_url: None,
            metrics_token: None,
            notifier: Arc::default(),
            metrics: Arc::new(IngestMetrics::default()),
            sessions: Arc::new(SessionStore::default()),
        }
    }

    pub fn with_trace_repo(mut self, trace_repo: Option<Arc<dyn TraceRepository>>) -> Self {
        self.trace_repo = trace_repo;
        self
    }

    /// 可选 `/metrics` 鉴权令牌（`METRICS_TOKEN`）。`None`/空串保持免鉴权。
    pub fn with_metrics_token(mut self, metrics_token: Option<String>) -> Self {
        self.metrics_token = metrics_token.filter(|token| !token.is_empty());
        self
    }

    /// 可选失败态 trace webhook（`TRACE_WEBHOOK_URL`，契约 v1.5-10）。`None`/空串即关闭。
    pub fn with_trace_webhook(mut self, url: Option<String>) -> Self {
        self.notifier = Arc::new(TraceNotifier::new(url));
        self
    }

    /// 写入端点（ingest / traces）的鉴权，沿用既有 CLIENT_TOKEN 语义（契约 R10.1 收窄版）：
    /// 未配置任何 token 时放行（集群内自举场景），配置后必须匹配 `client_token` 或 `admin_token`。
    fn check_write_auth(&self, headers: &HeaderMap) -> AuthOutcome {
        check_write_auth(
            self.client_token.as_deref(),
            self.admin_token.as_deref(),
            headers,
        )
    }

    /// 管理侧鉴权：静态 token（API 客户端/运维）优先；否则接受服务端短期会话。
    /// Cookie 会话执行状态变更时必须额外携带匹配的 CSRF token。
    fn authorize_admin(
        &self,
        headers: &HeaderMap,
        method: &Method,
    ) -> Result<AdminCredential, AdminAuthError> {
        if let Some(provided) = extract_provided_token(headers) {
            if static_token_matches(
                self.admin_token.as_deref(),
                self.client_token.as_deref(),
                provided,
            ) {
                return Ok(AdminCredential::Static);
            }
        }
        if self.admin_token.is_none() && self.client_token.is_none() {
            return Err(AdminAuthError::NotConfigured);
        }

        if let Some(token) = session_token_from_cookie(headers) {
            if let Some(info) = self.sessions.validate(&token) {
                let is_mutation =
                    !matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS);
                if is_mutation {
                    let Some(csrf) = csrf_token_from_header(headers) else {
                        return Err(AdminAuthError::CsrfInvalid);
                    };
                    if !self.sessions.csrf_matches(&info.csrf_token, csrf) {
                        return Err(AdminAuthError::CsrfInvalid);
                    }
                }
                return Ok(AdminCredential::SessionCookie);
            }
        }

        if let Some(token) = session_token_from_bearer(headers) {
            if self.sessions.validate(&token).is_some() {
                return Ok(AdminCredential::SessionBearer);
            }
        }

        Err(AdminAuthError::Unauthorized)
    }
}

/// 鉴权判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthOutcome {
    Allowed,
    Unauthorized,
    #[cfg_attr(not(test), allow(dead_code))]
    NotConfigured,
}

/// 管理侧请求使用的凭证类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminCredential {
    Static,
    SessionCookie,
    SessionBearer,
}

/// 管理侧鉴权失败原因（CSRF 与认证失败分开返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminAuthError {
    Unauthorized,
    NotConfigured,
    CsrfInvalid,
}

fn static_token_matches(admin: Option<&str>, client: Option<&str>, provided: &str) -> bool {
    [admin, client]
        .into_iter()
        .flatten()
        .any(|expected| token_matches(provided, expected))
}

/// 管理侧（读 + PATCH）鉴权规则（契约 R10 / R10.1）：
/// - `admin` 与 `client` 都未配置 → `NotConfigured`（503 fail-closed）；
/// - 任一配置且请求令牌与之匹配 → `Allowed`；否则 `Unauthorized`。
#[cfg(test)]
fn check_admin_auth(admin: Option<&str>, client: Option<&str>, headers: &HeaderMap) -> AuthOutcome {
    match (admin, client) {
        (None, None) => AuthOutcome::NotConfigured,
        _ => {
            let provided = extract_provided_token(headers);
            if provided.is_some_and(|token| static_token_matches(admin, client, token)) {
                AuthOutcome::Allowed
            } else {
                AuthOutcome::Unauthorized
            }
        }
    }
}

/// 写入侧（ingest / traces）鉴权规则：未配置任何令牌时沿用既有放行语义（契约 R10.1 收窄版），
/// 配置后必须匹配 `client` 或 `admin`。
fn check_write_auth(client: Option<&str>, admin: Option<&str>, headers: &HeaderMap) -> AuthOutcome {
    // 写入端点的门控键是 CLIENT_TOKEN（契约 R10.1 收窄版）：未配置即放行历史行为；
    // 配置后 ADMIN_TOKEN 亦可作为合法凭证（契约 R10 矩阵）。
    if client.is_none() {
        return AuthOutcome::Allowed;
    }
    let provided = extract_provided_token(headers);
    if provided.is_some_and(|token| static_token_matches(admin, client, token)) {
        AuthOutcome::Allowed
    } else {
        AuthOutcome::Unauthorized
    }
}

/// 从请求头取凭证：`x-admin-token` → `x-client-token` → `Authorization: Bearer`。
fn extract_provided_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-admin-token")
        .and_then(|h| h.to_str().ok())
        .or_else(|| headers.get("x-client-token").and_then(|h| h.to_str().ok()))
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 常量时间比较，避免按字节提前返回泄露 token 前缀。
fn token_matches(provided: &str, expected: &str) -> bool {
    let (a, b) = (provided.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// 422 校验拒绝（契约 R4）。
fn invalid_field(field: &'static str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"error": "invalid_field", "field": field})),
    )
}

/// 503 fail-closed（契约 R10.1）。
fn admin_token_not_configured() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "admin_token_not_configured"})),
    )
}

fn unauthorized() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
}

fn csrf_invalid() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "csrf_invalid"})),
    )
}

fn persistence_error(err: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    error!(error = %err, "ingest persistence failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "persistence_error"})),
    )
}

#[derive(Debug, Deserialize)]
pub struct IngestPayload {
    pub platform: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub message: Option<String>,
    pub exception: Option<pony_sentry_ingest::Exception>,
    pub tags: Option<std::collections::HashMap<String, String>>,
    pub extra: Option<serde_json::Value>,
    pub breadcrumbs: Option<Vec<serde_json::Value>>,
    /// 客户端确定性幂等键（≤64 可打印 ASCII），落 `events.event_key`（契约 R2 / R2.3）。
    pub event_id: Option<String>,
    /// `fatal|error|warning|info`，非法值降级 `warning`（契约 R2）。
    pub level: Option<String>,
    pub namespace: Option<String>,
    pub app: Option<String>,
    pub component: Option<String>,
    pub service_name: Option<String>,
    /// 一级错误类型，缺省取 `exception.error_type`（契约 R2）。
    pub error_type: Option<String>,
    pub source_kind: Option<String>,
    pub grafana_url: Option<String>,
    pub loki_query: Option<String>,
    /// ISO8601 事件发生时间；缺省为服务端 now()（契约 R2）。
    pub timestamp: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    /// `created` | `duplicate`（契约 R4；与 `status` 正交，后者仍是 issue 状态）。
    pub result: &'static str,
    pub status: IssueStatus,
    pub issue_id: String,
    /// 回显客户端 `event_id`（无则回退服务端事件 id）。
    pub event_id: String,
    pub fingerprint: String,
    pub count: i64,
    pub duplicate: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct IssueQuery {
    pub status: Option<String>,
    pub platform: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
    /// 环境维度筛选（契约 R10.5）。
    pub environment: Option<String>,
    /// 等级维度筛选 `fatal|error|warning|info`。
    pub level: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIssueRequest {
    pub status: Option<String>,
    pub assigned_to: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentTraceIngestRequest {
    /// 会话/线程标识：优先采用 session_id，兼容 LangGraph / LangChain 的 thread_id
    pub session_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    pub project: Option<String>,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub eval_status: Option<EvalStatus>,
    pub turns: Option<Vec<serde_json::Value>>,
    pub tags: Option<std::collections::HashMap<String, String>>,
    pub extra: Option<serde_json::Value>,
    pub total_input_tokens: Option<i64>,
    pub total_output_tokens: Option<i64>,
    pub total_duration_ms: Option<i64>,
    pub reported_at_ms: Option<u64>,
    /// `I-01`（契约 v1.5-3）：未知顶层字段不再静默丢弃，归一后写入 `payload._unmapped`。
    ///
    /// 以 `_` 开头的键属服务端保留命名空间（`_unmapped`/`_warnings`/`_redactions`），
    /// 客户端传入一律丢弃，避免伪造服务端派生字段。
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct TraceQuery {
    pub session_id: Option<String>,
    pub eval_status: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
    /// `C5`：按 run_id 精确过滤。
    pub run_id: Option<String>,
    /// `C5`：按会话级终态摘要过滤（`traces.turn_status`）。
    pub turn_status: Option<String>,
    /// `C5`：`reported_at` 时间窗下限（含），RFC3339；非法值 → 400。
    #[serde(default, deserialize_with = "deserialize_rfc3339_param")]
    pub from: Option<DateTime<Utc>>,
    /// `C5`：`reported_at` 时间窗上限（含），RFC3339；非法值 → 400。
    #[serde(default, deserialize_with = "deserialize_rfc3339_param")]
    pub to: Option<DateTime<Utc>>,
    /// `C5`：非数字 → 400（不静默丢弃）；负值在 handler 里 → 400；其余 clamp 到 `[1,100]`。
    #[serde(default, deserialize_with = "deserialize_strict_i64_param")]
    pub limit: Option<i64>,
    /// `C5`：非数字 → 400；负值 → 400。
    #[serde(default, deserialize_with = "deserialize_strict_i64_param")]
    pub offset: Option<i64>,
    /// `W-05` 后半（契约 v1.5-2）：`created_at` 可切换到按入库时间倒序；缺省 `reported_at`。
    pub order: Option<String>,
    /// `I-04`（契约 v1.5-7）：`summary` 时响应不含 `payload`；缺省返回完整记录。
    pub fields: Option<String>,
}

/// `C5`：严格整数解析——空串视作缺省，非整数字面量返回反序列化错误（axum 统一 400），
/// 绝不回显 DB 原文（错误文本由本函数生成）。
fn deserialize_strict_i64_param<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    let trimmed = raw.unwrap_or_default();
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|_| D::Error::custom("Bad request: integer query parameter expected"))
}

/// `C5`：RFC3339 严格时间解析——非法值 → 400，错误文本不含任何数据库信息。
fn deserialize_rfc3339_param<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    let trimmed = raw.unwrap_or_default();
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(trimmed)
        .map(|value| Some(value.with_timezone(&Utc)))
        .map_err(|_| D::Error::custom("Bad request: RFC3339 timestamp expected"))
}

/// `I-04`（契约 v1.5-7）：`?fields=summary` 的列表投影——除 `payload` 外与完整记录同构，
/// 便于控制台列表页在万行级场景下省掉正文传输。
#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub environment: String,
    pub release: String,
    pub project: Option<String>,
    pub eval_status: EvalStatus,
    pub turn_status: String,
    pub turns_count: i64,
    // `L3B-E6`（契约 v1.6-10）：summary 投影**不含** `note`（评审备注只走详情/PATCH 回显）。
    pub total_input_tokens: Option<i64>,
    pub total_output_tokens: Option<i64>,
    pub total_duration_ms: Option<i64>,
    pub reported_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<TraceRecord> for TraceSummary {
    fn from(record: TraceRecord) -> Self {
        Self {
            id: record.id,
            session_id: record.session_id,
            run_id: record.run_id,
            turn_id: record.turn_id,
            environment: record.environment,
            release: record.release,
            project: record.project,
            eval_status: record.eval_status,
            turn_status: record.turn_status,
            turns_count: record.turns_count,
            total_input_tokens: record.total_input_tokens,
            total_output_tokens: record.total_output_tokens,
            total_duration_ms: record.total_duration_ms,
            reported_at: record.reported_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateTraceRequest {
    pub eval_status: EvalStatus,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    token: String,
}

pub fn create_app_with_state(state: AppState) -> Router {
    // 进程级 env 接缝（契约 §1）：真实进程由 main.rs 显式调用 `create_app_with_trust`；
    // 本入口保留给既有进程内测试 —— 三个 tailnet env 键未配置时信任恒为 Disabled。
    create_app_with_trust(state, TrustConfig::from_env())
}

/// 构建应用并显式注入 tailnet 信任配置（开关取进程环境）。
///
/// 保留本入口是为了不打破既有调用方（`main.rs` 之外的进程内测试）；tracing 接收开关
/// 由 [`create_app_with_options`] 显式注入，未注入时按 [`TraceIngestSwitch::from_env`] 取值。
pub fn create_app_with_trust(state: AppState, trust: TrustConfig) -> Router {
    create_app_with_options(state, trust, TraceIngestSwitch::from_env())
}

/// 构建应用并显式注入 tailnet 信任配置 + tracing 接收开关（`main.rs` 使用，故两个
/// `from_env` 的启动日志各只打一次）。
///
/// `trust` 经 `Extension` 层下发：`admin_auth_middleware` 与 `handle_get_session` 各取一份
/// `Arc<TrustConfig>`。`trace_ingest` 以 **state 形式**注入 4 个 trace 路由的门口中间件：
/// 关闭时在处理器之前返回 `410 Gone`（不写库、不递增指标）。两者都不往 `AppState` 增字段，
/// 是因为进程内既有测试以结构体字面量构造它。
pub fn create_app_with_options(
    state: AppState,
    trust: TrustConfig,
    trace_ingest: TraceIngestSwitch,
) -> Router {
    let trust = Arc::new(trust);

    // 1. Ingest 专用 CORS：仅开放 POST，收敛跨域能力
    let ingest_cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-sentry-auth"),
            header::HeaderName::from_static("x-client-token"),
            header::HeaderName::from_static("x-admin-token"),
        ]);

    // 2. 遥测上报路由 (强制挂载 2MB 请求体硬限制与专有 CORS)
    let ingest_routes = Router::new()
        .route("/api/v1/ingest", post(handle_ingest))
        .route(
            "/api/v1/traces",
            post(handle_ingest_trace).layer(middleware::from_fn_with_state(
                trace_ingest,
                trace_ingest_gate,
            )),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(ingest_cors);

    // 3. 控制台会话路由：静态 ADMIN_TOKEN 只用于一次性交换短期会话，
    //    会话 Cookie 为 HttpOnly + SameSite=Strict，不进入前端构建产物。
    let auth_routes = Router::new()
        .route(
            "/api/v1/auth/session",
            post(handle_create_session)
                .get(handle_get_session)
                .delete(handle_delete_session),
        )
        .layer(DefaultBodyLimit::max(4 * 1024));

    // 4. 面向 Web 控制台的管理路由 (不挂载通配跨域，防止 CSRF 跨域窃取)
    //    鉴权（契约 R10）：静态 ADMIN_TOKEN / CLIENT_TOKEN，或服务端短期会话。
    let admin_routes = Router::new()
        .route("/api/v1/issues", get(handle_list_issues))
        .route("/api/v1/projects", get(handle_list_projects))
        .route("/api/v1/issues/:id", get(handle_get_issue))
        .route(
            "/api/v1/issues/:id",
            patch(handle_update_issue).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/api/v1/issues/:id/events", get(handle_get_issue_events))
        // issue 治理（契约 `.dev-team/contract-issue-governance.md` FROZEN v1）：
        // 路由注册占位；handler 均为空桩，由 Wave-9 executor 落地业务体。
        .route("/api/v1/issues/archive", post(handle_archive_issues))
        .route("/api/v1/issues/:id/archive", post(handle_archive_issue))
        .route("/api/v1/issues/:id/unarchive", post(handle_unarchive_issue))
        .route("/api/v1/stats", get(handle_get_issue_stats))
        .route("/api/v1/ingest/stats", get(handle_get_ingest_stats))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ));

    // 4b. trace 管理路由独立成组：**停收门在最外层**（先于 admin 鉴权），
    //     故关闭时无论是否携带凭证一律 410 —— 这是「入口已关闭」对调用方的确定答复，
    //     而不是 401 让人误以为补个 token 就能继续上报。
    let trace_admin_routes = Router::new()
        .route("/api/v1/traces", get(handle_list_traces))
        .route("/api/v1/traces/:id", get(handle_get_trace))
        .route(
            "/api/v1/traces/:id",
            patch(handle_update_trace).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            trace_ingest,
            trace_ingest_gate,
        ));

    let html_content = include_str!("../../../web/dist/index.html");

    Router::new()
        .route(
            "/",
            get(move || async move { axum::response::Html(html_content) }),
        )
        .route("/healthz", get(handle_healthz))
        .route("/metrics", get(handle_metrics))
        .merge(ingest_routes)
        .merge(auth_routes)
        .merge(admin_routes)
        .merge(trace_admin_routes)
        // 信任配置：Extension 层置于所有路由之外，鉴权中间件与处理器均可读取。
        .layer(Extension(trust))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .with_state(state)
}

/// 单次请求的信任判定结果与审计字段（`trust_decision` / `peer` / `host`，禁打凭证）。
struct TrustContext {
    decision: TrustDecision,
    /// 已归一化的对端地址（未接线 connect_info 时为 `None`）。
    peer: Option<IpAddr>,
    /// 原始 `Host` 头（缺失时为 `None`）。
    host: Option<String>,
}

impl TrustContext {
    fn evaluate(
        trust: &TrustConfig,
        headers: &HeaderMap,
        connect_info: Option<&ConnectInfo<SocketAddr>>,
    ) -> Self {
        let peer = connect_info.map(|info| normalize_peer(info.0.ip()));
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let decision = trust.decide(peer, is_proxied(headers), host.as_deref());
        Self {
            decision,
            peer,
            host,
        }
    }

    /// 信任只兑换**只读**：`Trusted` + `GET`/`HEAD` 才放行（契约 §2/R-T7 红线③）。
    /// 注意：`OPTIONS` 不在放行面内 —— 契约只冻结了 GET/HEAD（Lead 裁决收窄）。
    fn grants_read(&self, method: &Method) -> bool {
        matches!(self.decision, TrustDecision::Trusted)
            && matches!(method, &Method::GET | &Method::HEAD)
    }

    /// 结构化审计日志：只含 `request_id` / `trust_decision` / `peer` / `host`（R-T10 + NFR `logging.require_trace_id`）。
    ///
    /// `request_id` 由调用方生成并传入，保证同一次请求内的审计日志可关联（不在本函数内新生成）。
    fn log_grant(&self, scope: &'static str, request_id: &str) {
        let peer = self
            .peer
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "-".to_string());
        let host = self.host.as_deref().unwrap_or("-");
        info!(
            scope,
            request_id = %request_id,
            trust_decision = ?self.decision,
            peer = %peer,
            host = %host,
            "tailnet 可信对端放行只读请求"
        );
    }
}

/// tracing 停收门：只挂在 4 个 trace 路由上（`POST/GET/PATCH /api/v1/traces`、
/// `GET /api/v1/traces/:id`）。
///
/// 关闭时在**处理器之前**返回 `410 Gone` + 固定响应体：处理器与鉴权后的业务分支都不执行
/// ⇒ 不写库、不递增 `pony_sentry_trace_*` 指标、不触发 webhook。开启时原样放行（`next.run`），
/// 不改变任何既有语义。
///
/// 为什么用中间件而不是在 4 个处理器里各加一个 if：一处接线即覆盖全部方法，且「不写库」
/// 由「处理器根本不执行」从结构上保证，不依赖每个处理器的自觉。
async fn trace_ingest_gate(
    State(switch): State<TraceIngestSwitch>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if switch.enabled() {
        return next.run(request).await;
    }
    info!(
        method = %request.method(),
        path = request.uri().path(),
        "tracing 已停收：返回 410（处理器未执行，不写库、不计数）"
    );
    TraceIngestSwitch::disabled_response()
}

/// 管理侧鉴权中间件：读接口与 PATCH 一律先过 token 校验（契约 R10 / R10.1）；
/// 仅在「无任何可验证凭证」时追加 tailnet 信任分支，且只对只读方法放行（契约 R-T1/R-T7）。
async fn admin_auth_middleware(
    State(state): State<AppState>,
    trust: Option<Extension<Arc<TrustConfig>>>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    match state.authorize_admin(request.headers(), &method) {
        Ok(_) => next.run(request).await,
        Err(AdminAuthError::Unauthorized) => {
            // R-T12：静态 token / 有效会话已在上面命中；此处只处理「无可识别凭证」。
            if let Some(Extension(trust)) = trust.as_ref() {
                let context =
                    TrustContext::evaluate(trust, request.headers(), connect_info.as_ref());
                if context.grants_read(&method) {
                    // 一次请求一个关联 id（NFR `logging.require_trace_id`），随审计行落盘。
                    let request_id = Uuid::new_v4().to_string();
                    context.log_grant("admin_auth", &request_id);
                    return next.run(request).await;
                }
            }
            unauthorized().into_response()
        }
        Err(AdminAuthError::NotConfigured) => admin_token_not_configured().into_response(),
        Err(AdminAuthError::CsrfInvalid) => csrf_invalid().into_response(),
    }
}

/// `POST /api/v1/auth/session`：用 ADMIN_TOKEN / CLIENT_TOKEN 交换短期 HttpOnly 会话。
async fn handle_create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateSessionRequest>,
) -> Response {
    if state.admin_token.is_none() && state.client_token.is_none() {
        return admin_token_not_configured().into_response();
    }

    let login_key = SessionStore::login_key(&headers);
    if !state.sessions.login_allowed(&login_key) {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "too_many_attempts"})),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    }

    let token = body.token.trim();
    if !static_token_matches(
        state.admin_token.as_deref(),
        state.client_token.as_deref(),
        token,
    ) {
        state.sessions.record_login_failure(&login_key);
        return unauthorized().into_response();
    }
    state.sessions.record_login_success(&login_key);
    let created = state.sessions.create();
    session_created_response(
        &state,
        &created.token,
        &created.info.csrf_token,
        created.info.expires_at,
    )
}

/// 会话签发成功响应（登录交换与可信对端自动签发**同形**）：HttpOnly Cookie + CSRF token。
///
/// `Set-Cookie` 构造失败时**不得谎报成功**：记 WARN（不打印 token）并返回 500，
/// 否则客户端会拿到 `authenticated:true` 却无可用会话。
fn session_created_response(
    state: &AppState,
    token: &str,
    csrf_token: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Response {
    let cookie = state.sessions.cookie_header(token);
    let Ok(cookie) = HeaderValue::from_str(&cookie) else {
        warn!(
            scope = "auth_session",
            "会话 Set-Cookie 头构造失败：拒绝返回 authenticated:true 的假成功"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "session_cookie_invalid"})),
        )
            .into_response();
    };
    let mut response = (
        StatusCode::OK,
        Json(json!({
            "authenticated": true,
            "expires_at": expires_at.to_rfc3339(),
            "expires_in": state.sessions.ttl_secs(),
            "csrf_token": csrf_token,
        })),
    )
        .into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /api/v1/auth/session`：返回当前短期会话状态与 CSRF token。
///
/// 无可验证会话时，**仅** `TrustDecision::Trusted` 的对端现场签发会话（契约 R-T6）：
/// 返回与登录成功同形的 JSON + `Set-Cookie`；其余来源维持 401（R-T6b）。
async fn handle_get_session(
    State(state): State<AppState>,
    trust: Option<Extension<Arc<TrustConfig>>>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Response {
    let token = session_token_from_cookie(&headers).or_else(|| session_token_from_bearer(&headers));
    if let Some(info) = token.and_then(|token| state.sessions.validate(&token)) {
        let mut response = Json(json!({
            "authenticated": true,
            "expires_at": info.expires_at.to_rfc3339(),
            "csrf_token": info.csrf_token,
        }))
        .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    }

    let context = trust
        .as_ref()
        .map(|Extension(trust)| TrustContext::evaluate(trust, &headers, connect_info.as_ref()));
    let Some(context) = context.filter(|context| context.grants_read(&Method::GET)) else {
        return unauthorized().into_response();
    };
    let request_id = Uuid::new_v4().to_string();
    context.log_grant("auth_session", &request_id);
    // 自动签发标记为 TrustedPeer：会话表满时优先驱逐这类会话，保护手动登录的写会话。
    let (token, csrf_token) = state
        .sessions
        .create_with_origin(SessionOrigin::TrustedPeer);
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(state.sessions.ttl_secs() as i64);
    session_created_response(&state, &token, &csrf_token, expires_at)
}

/// `DELETE /api/v1/auth/session`：注销当前会话并清除 Cookie。
async fn handle_delete_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) =
        session_token_from_cookie(&headers).or_else(|| session_token_from_bearer(&headers))
    {
        state.sessions.remove(&token);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(&state.sessions.clear_cookie_header()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /healthz`：DB 探活（契约 R11.2）。DB 不可达 → 503 且 body 带 `db:"down"`。
async fn handle_healthz(State(state): State<AppState>) -> Response {
    match tokio::time::timeout(DB_PROBE_TIMEOUT, state.repo.health_check()).await {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({"status": "ok", "db": "up"}))).into_response(),
        Ok(Err(err)) => {
            warn!(error = %err, "healthz db probe failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "degraded", "db": "down"})),
            )
                .into_response()
        }
        Err(_) => {
            warn!("healthz db probe timed out");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "degraded", "db": "down"})),
            )
                .into_response()
        }
    }
}

/// `/metrics` 可选鉴权判定（L3B-F11）：
/// - 未配置令牌（`None`/空串）→ 放行（保持探针免鉴权现状，README 语义不变）；
/// - 已配置 → 接受 `x-metrics-token` 或 `Authorization: Bearer <token>`，两者都不匹配则拒绝。
fn metrics_authorized(configured: Option<&str>, headers: &HeaderMap) -> bool {
    let expected = match configured.map(str::trim).filter(|token| !token.is_empty()) {
        Some(token) => token,
        None => return true,
    };
    if let Some(value) = headers
        .get("x-metrics-token")
        .and_then(|value| value.to_str().ok())
    {
        if token_matches(expected, value.trim()) {
            return true;
        }
    }
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
        if scheme.eq_ignore_ascii_case("bearer") && token_matches(expected, token.trim()) {
            return true;
        }
    }
    false
}

/// Prometheus 文本格式的 label 值转义。
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// `GET /metrics`：最小运维指标集（契约 R11.3）。
async fn handle_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    // L3B-F11：可选鉴权。未配置 `METRICS_TOKEN` 时行为与既有完全一致（探针免鉴权）。
    if !metrics_authorized(state.metrics_token.as_deref(), &headers) {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let counts = state
        .repo
        .issue_counts_by_status_environment()
        .await
        .map_err(|err| {
            error!(error = %err, "metrics issue counts failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR_BODY.to_string(),
            )
        })?;
    let events_total = state.repo.count_events().await.map_err(|err| {
        error!(error = %err, "metrics event count failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL_ERROR_BODY.to_string(),
        )
    })?;

    let mut body = String::from(
        "# HELP pony_sentry_issues_total Issue 总数（按 status 与 environment 维度）\n\
         # TYPE pony_sentry_issues_total gauge\n",
    );
    for row in &counts {
        body.push_str(&format!(
            "pony_sentry_issues_total{{status=\"{}\",environment=\"{}\"}} {}\n",
            escape_label(&row.status),
            escape_label(&row.environment),
            row.count
        ));
    }

    body.push_str(
        "# HELP pony_sentry_events_total 事件总行数\n\
         # TYPE pony_sentry_events_total counter\n",
    );
    body.push_str(&format!("pony_sentry_events_total {events_total}\n"));

    let (created, duplicate, degraded_level) = state.metrics.snapshot();
    body.push_str(
        "# HELP pony_sentry_ingest_requests_total ingest 写入结果计数\n\
         # TYPE pony_sentry_ingest_requests_total counter\n",
    );
    body.push_str(&format!(
        "pony_sentry_ingest_requests_total{{result=\"created\"}} {created}\n\
         pony_sentry_ingest_requests_total{{result=\"duplicate\"}} {duplicate}\n"
    ));
    body.push_str(
        "# HELP pony_sentry_ingest_level_degraded_total 非法 level 降级为 warning 的次数\n\
         # TYPE pony_sentry_ingest_level_degraded_total counter\n",
    );
    body.push_str(&format!(
        "pony_sentry_ingest_level_degraded_total {degraded_level}\n"
    ));

    // `C7` + `F2`：trace 维度四个计数器（仅新增；label 已归一、series 有上限，
    // 渲染持锁迭代、不整表 clone）。
    state.metrics.render_trace_metrics(&mut body);

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
        .into_response())
}

async fn handle_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<IngestPayload>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    // 1. 写入侧鉴权：未配置 token 时沿用既有放行语义（契约 R10.1 收窄版）
    if state.check_write_auth(&headers) == AuthOutcome::Unauthorized {
        return Err(unauthorized());
    }

    // 2. 严格校验：会破坏幂等语义/时间可信度的字段直接 422（契约 R2.3 / R4）
    let event_key = validate_event_id(payload.event_id).map_err(|e| invalid_field(e.field))?;
    let occurred_at =
        parse_timestamp(payload.timestamp.as_deref()).map_err(|e| invalid_field(e.field))?;

    // 3. 归一：等级降级、维度截断、URL/查询结构校验（契约 R2 / R2.5）
    let mut warnings: Vec<String> = Vec::new();
    let (level, degraded_level) = normalize_level(payload.level.as_deref());
    if degraded_level {
        state.metrics.record_degraded_level();
        warn!(
            raw_level = ?payload.level,
            degraded_to = %level,
            "ingest level 非法，已降级"
        );
        warnings.push(format!("invalid_level_degraded_to_{level}"));
    }

    let environment = normalize_environment(payload.environment.as_deref());
    let namespace = normalize_dimension(payload.namespace.as_deref(), 128);
    let app = normalize_dimension(payload.app.as_deref(), 128);
    let component = normalize_dimension(payload.component.as_deref(), 128);
    let service_name = normalize_dimension(payload.service_name.as_deref(), 128);
    let error_type = normalize_dimension(payload.error_type.as_deref(), 256);
    let source_kind = normalize_source_kind(payload.source_kind.as_deref());
    let scope = "ingest";
    let request_id = event_key
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    info!(
        scope,
        request_id = %request_id,
        environment = %environment,
        level = %level,
        source_kind = ?source_kind,
        "ingest received"
    );

    let grafana_url = normalize_grafana_url(payload.grafana_url.clone());
    if grafana_url.is_none() && payload.grafana_url.is_some() {
        warn!(scope, request_id = %request_id, "grafana_url 结构非法，已丢弃");
        warnings.push("grafana_url_discarded".to_string());
    }
    let loki_query = normalize_loki_query(payload.loki_query.clone());
    if loki_query.is_none() && payload.loki_query.is_some() {
        warn!(scope, request_id = %request_id, "loki_query 结构非法，已丢弃");
        warnings.push("loki_query_discarded".to_string());
    }

    let (message, extra_truncated) = (
        payload
            .message
            .as_deref()
            .map(|m| truncate_chars(m, MESSAGE_MAX_CHARS)),
        false,
    );
    let (extra, extra_truncated_by_size) = guard_extra(payload.extra);
    if extra_truncated_by_size {
        warn!(scope, request_id = %request_id, "extra 超过 8KB，已替换为占位对象");
        warnings.push("extra_truncated".to_string());
    }
    let _ = extra_truncated;

    let raw_event = RawEvent {
        platform: normalize_dimension(payload.platform.as_deref(), 64)
            .unwrap_or_else(|| "other".to_string()),
        release: normalize_dimension(payload.release.as_deref(), 128),
        environment: Some(environment.clone()),
        message,
        exception: payload.exception,
        tags: normalize_tags(payload.tags),
        extra,
        breadcrumbs: payload.breadcrumbs,
        event_id: event_key.clone(),
        level: Some(level.clone()),
        namespace: namespace.clone(),
        app: app.clone(),
        component: component.clone(),
        service_name: service_name.clone(),
        error_type: error_type.clone(),
        source_kind: source_kind.clone(),
        grafana_url,
        loki_query,
        timestamp: payload.timestamp,
    };

    // 4. 栈帧双形态归一：字符串 → extra.stacktrace_text + 单帧（契约 R3）
    let raw_event = normalize_stacktrace(raw_event);

    // 5. 脱敏
    let sanitized = SanitizationPipeline::sanitize_event(raw_event);

    // 6. 计算 v3 指纹与派生维度（唯一真相源：compute_issue_fingerprint）
    let info = compute_issue_fingerprint(&sanitized);
    let title = info.title.clone();
    let culprit = info.culprit.clone();
    let fingerprint = info.fingerprint.clone();

    // 7. 项目维度：综合 tags、extra、title 与 culprit 智能识别
    let project = infer_project_name(
        sanitized.tags.as_ref(),
        sanitized.extra.as_ref(),
        Some(&title),
        culprit.as_deref(),
    );

    let payload_json = serde_json::to_value(&sanitized)
        .map_err(|e| persistence_error(format!("serialize sanitized event: {e}")))?;

    // 8. 持久化与幂等（契约 R4）
    let res = state
        .repo
        .record_event_and_upsert_issue(NewEventRecord {
            fingerprint: &fingerprint,
            title: &title,
            culprit: culprit.as_deref(),
            platform: &sanitized.platform,
            release: sanitized.release.as_deref(),
            environment: Some(info.environment.as_str()),
            project: project.as_deref(),
            app: info.app.as_deref(),
            namespace: info.namespace.as_deref(),
            component: info.component.as_deref(),
            level: &info.level,
            source_kind: info.source_kind.as_deref(),
            event_key: event_key.as_deref(),
            payload: payload_json,
            occurred_at,
        })
        .await
        .map_err(persistence_error)?;

    let issue = res.issue;
    let event = res.event;

    // 9. 契约缺陷告警（保持既有行为）
    if issue.project.is_none() {
        warnings.push(
            "missing_project: please supply tags.project, tags.service, or extra.project_path"
                .to_string(),
        );
    }
    if issue.culprit.is_none() {
        warnings.push(
            "missing_culprit: exception stacktrace or tags.command/source is absent".to_string(),
        );
    }

    if res.is_duplicate {
        state.metrics.record_duplicate();
    } else {
        state.metrics.record_created();
    }

    info!(
        scope,
        request_id = %request_id,
        issue_id = %issue.id,
        duplicate = res.is_duplicate,
        count = issue.count,
        "ingest persisted"
    );

    // 10. 仅当新问题创建 (is_new) 或首次从 Resolved 状态回归 (is_regression_trigger) 时分发 Webhook，杜绝雪崩
    //     幂等命中不再触发通知（同一事件不重复通知）。
    if !res.is_duplicate && (res.is_new || res.is_regression_trigger) {
        if let Some(ref webhook_url) = state.webhook_url {
            let webhook_url = webhook_url.clone();
            let event_type = if res.is_new {
                "issue.created"
            } else {
                "issue.regression"
            };
            let webhook_payload = serde_json::json!({
                "event_type": event_type,
                "timestamp": chrono::Utc::now().timestamp(),
                "issue": {
                    "id": issue.id,
                    "fingerprint": issue.fingerprint,
                    "title": issue.title,
                    "culprit": issue.culprit,
                    "platform": issue.platform,
                    "count": issue.count,
                    "status": issue.status,
                    "project": issue.project,
                    "environment": issue.environment,
                    "level": issue.level,
                    "project_path": sanitized.extra.as_ref().and_then(|x| x.get("project_path")).and_then(|v| v.as_str()),
                },
                "latest_event": {
                    "id": event.id,
                    "message": sanitized.message,
                    "exception": sanitized.exception,
                    "breadcrumbs": sanitized.breadcrumbs,
                    "tags": sanitized.tags,
                    "extra": sanitized.extra,
                }
            });

            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build();
                if let Ok(client) = client {
                    let _ = client
                        .post(&webhook_url)
                        .json(&webhook_payload)
                        .send()
                        .await;
                }
            });
        }
    }

    let response = IngestResponse {
        result: if res.is_duplicate {
            "duplicate"
        } else {
            "created"
        },
        status: issue.status,
        issue_id: issue.id,
        event_id: event.event_key.clone().unwrap_or_else(|| event.id.clone()),
        fingerprint,
        count: issue.count,
        duplicate: res.is_duplicate,
        warnings,
    };

    let status_code = if res.is_duplicate {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status_code, Json(response)).into_response())
}

async fn handle_list_issues(
    State(state): State<AppState>,
    Query(q): Query<IssueQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let status_enum = q.status.and_then(|s| s.parse::<IssueStatus>().ok());
    let filter = IssueFilter {
        status: status_enum,
        platform: q.platform,
        release: q.release,
        project: q.project,
        environment: q.environment,
        level: q.level,
        limit: q.limit,
        offset: q.offset,
    };

    let issues = state
        .repo
        .list_issues(filter)
        .await
        .map_err(|err| internal_error("list_issues", err))?;

    Ok(Json(issues))
}

async fn handle_list_projects(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut projects = state
        .repo
        .list_projects()
        .await
        .map_err(|err| internal_error("list_projects", err))?;

    // 聚合 trace 维度的 project（issue 之外的 Agent trace 项目也能进筛选下拉）
    if let Some(trace_repo) = &state.trace_repo {
        let trace_projects = trace_repo
            .list_projects()
            .await
            .map_err(|err| internal_error("trace_list_projects", err))?;
        projects.extend(trace_projects);
    }

    projects.sort();
    projects.dedup();

    Ok(Json(projects))
}

/// 从事件载荷派生 `error_type`（契约 R2 的一级字段，事件级属性，不落 issues 表）：
/// 优先级 `payload.error_type` → `payload.exception.error_type`。
fn derived_error_type(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("error_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            payload
                .get("exception")
                .and_then(|e| e.get("error_type"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
        })
}

async fn handle_get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let issue = state.repo.get_issue(&id).await.map_err(|err| match err {
        pony_sentry_core::RepositoryError::NotFound(_) => {
            (StatusCode::NOT_FOUND, "Issue not found".into())
        }
        other => internal_error("get_issue", other),
    })?;

    // 派生视图字段 `error_type`（事件级属性）：取该 issue 最近一条事件的载荷。
    let mut value =
        serde_json::to_value(&issue).map_err(|err| internal_error("serialize_issue", err))?;
    if let Ok(events) = state.repo.get_events_for_issue(&id, 1).await {
        if let Some(latest) = events.first() {
            if let Some(error_type) = derived_error_type(&latest.payload) {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("error_type".to_string(), json!(error_type));
                }
            }
        }
    }

    Ok(Json(value))
}

async fn handle_update_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    payload: Result<Json<UpdateIssueRequest>, JsonRejection>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // `F6`：PATCH 非法体统一 400 `Bad request`（不回显 serde 内部结构与合法枚举值）。
    let Json(body) = payload.map_err(json_rejection_response)?;
    let status = match body.status {
        Some(s) => s.parse::<IssueStatus>().map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("{BAD_REQUEST_BODY}: invalid `status`"),
            )
        })?,
        None => {
            let current = state.repo.get_issue(&id).await.map_err(|err| match err {
                pony_sentry_core::RepositoryError::NotFound(_) => {
                    (StatusCode::NOT_FOUND, "Issue not found".to_string())
                }
                other => internal_error("get_issue_for_update", other),
            })?;
            current.status
        }
    };

    let updated = state
        .repo
        .update_issue_status(&id, status, body.assigned_to)
        .await
        .map_err(|err| match err {
            pony_sentry_core::RepositoryError::NotFound(_) => {
                (StatusCode::NOT_FOUND, "Issue not found".to_string())
            }
            other => internal_error("update_issue_status", other),
        })?;

    Ok(Json(updated))
}

async fn handle_get_issue_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let events = state
        .repo
        .get_events_for_issue(&id, 20)
        .await
        .map_err(|err| internal_error("get_events_for_issue", err))?;

    Ok(Json(events))
}

// ---------------------------------------------------------------------------
// issue 治理 handler 空桩（契约 `.dev-team/contract-issue-governance.md` FROZEN v1）
//
// **仅签名骨架**：业务体一律 `unimplemented!("implemented by wave-9 executor")`。
// 返回值用具体类型 `Response`（非 `impl IntoResponse`），以便空桩在类型层面自洽。
// 参数校验（400 边界）、鉴权后处理、DB 调用均由 executor 在本区段实现。
// ---------------------------------------------------------------------------

/// `C2.3`：`POST /api/v1/issues/archive`（批量归档，body 见 [`IssueArchiveRequest`]）。
#[allow(dead_code)]
async fn handle_archive_issues(
    State(_state): State<AppState>,
    _payload: Result<Json<IssueArchiveRequest>, JsonRejection>,
) -> Result<Response, (StatusCode, String)> {
    unimplemented!("implemented by wave-9 executor")
}

/// `C2.3`：`POST /api/v1/issues/:id/archive`（单条归档，幂等）。
#[allow(dead_code)]
async fn handle_archive_issue(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
) -> Result<Response, (StatusCode, String)> {
    unimplemented!("implemented by wave-9 executor")
}

/// `C2.3`：`POST /api/v1/issues/:id/unarchive`（单条恢复，幂等）。
#[allow(dead_code)]
async fn handle_unarchive_issue(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
) -> Result<Response, (StatusCode, String)> {
    unimplemented!("implemented by wave-9 executor")
}

/// `C5`：`GET /api/v1/stats`（query 见 [`IssueStatsQuery`]）。
#[allow(dead_code)]
async fn handle_get_issue_stats(
    State(_state): State<AppState>,
    Query(_query): Query<IssueStatsQuery>,
) -> Result<Response, (StatusCode, String)> {
    unimplemented!("implemented by wave-9 executor")
}

/// `C6.1`：`GET /api/v1/ingest/stats`（query 见 [`IngestGovernanceQuery`]）。
#[allow(dead_code)]
async fn handle_get_ingest_stats(
    State(_state): State<AppState>,
    Query(_query): Query<IngestGovernanceQuery>,
) -> Result<Response, (StatusCode, String)> {
    unimplemented!("implemented by wave-9 executor")
}

async fn handle_ingest_trace(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<AgentTraceIngestRequest>, JsonRejection>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if state.check_write_auth(&headers) == AuthOutcome::Unauthorized {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid or missing client token".into(),
        ));
    }

    // `C5`：请求体不合法统一 400 `Bad request`（不回显 serde/DB 原文）；
    // 仅请求体超限保留 413（传输层限制，非上报语义错误）。
    let Json(payload) = payload.map_err(|rejection| {
        let response = json_rejection_response(rejection);
        if response.0 == StatusCode::BAD_REQUEST {
            state.metrics.record_trace_ingest_rejected("invalid_params");
        }
        response
    })?;

    let trace_repo = state.trace_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "Trace repository not configured".into(),
        )
    })?;

    let now = chrono::Utc::now();

    // `W-05`（契约 v1.5-2）：`reported_at_ms` 与服务器时间偏差超过 24h → 400（防时钟错标污染时间线）。
    if let Some(ms) = payload.reported_at_ms {
        let skew = ms as i128 - now.timestamp_millis() as i128;
        if skew.abs() > REPORTED_AT_MAX_SKEW_MS as i128 {
            state.metrics.record_trace_ingest_rejected("invalid_params");
            return Err((
                StatusCode::BAD_REQUEST,
                "Bad request: reported_at out of range".to_string(),
            ));
        }
    }

    let reported_at = payload
        .reported_at_ms
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
        .unwrap_or(now);

    let mut raw_payload = serde_json::to_value(&payload).unwrap_or_else(|_| serde_json::json!({}));
    let ingest_bytes = serde_json::to_vec(&raw_payload)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0);

    // `I-01`（v1.5-3）+ `L3A-E2`（v1.6-2）：`#[serde(flatten)]` 把未知顶层字段平铺回顶层，这里摘出来
    // 改注入 `payload._unmapped`。策略：
    // - **除三个服务端保留名外，所有未知键（含 `_sdk` 等 `_` 前缀）一律保留**；
    // - 保留名若被客户端传入 → 丢弃（随后由服务端注入真实值，防伪造服务端派生事实）；
    // - 任意深层的保留名同样剥离（递归，深度上限 32）；
    // - `_unmapped` 注入发生在脱敏之前，故其中的敏感值同样被扫。
    if let Some(object) = raw_payload.as_object_mut() {
        for key in payload.unknown.keys() {
            object.remove(key);
        }
    }
    let mut unmapped = serde_json::Map::new();
    for (key, value) in payload.unknown.iter() {
        if RESERVED_PAYLOAD_KEYS.contains(&key.as_str()) {
            continue; // 保留名：丢弃客户端伪造值，等统一注入
        }
        let mut owned = value.clone();
        strip_reserved_payload_keys(&mut owned, 0);
        unmapped.insert(key.clone(), owned);
    }
    // 已知字段/深层嵌套里的保留名同样剥离（客户端不能借 turns/extra 等通道伪造服务端派生键）。
    strip_reserved_payload_keys(&mut raw_payload, 0);
    if !unmapped.is_empty() {
        if let Some(object) = raw_payload.as_object_mut() {
            object.insert("_unmapped".to_string(), serde_json::Value::Object(unmapped));
        }
    }

    // 服务端二次脱敏（`M-04` / 契约 v1.5-5）：客户端 SDK 已脱敏一次，入库前按同一管道再扫一遍，
    // 防止绕过客户端的敏感字段（token/secret/password 等）落库；同时取回命中计数。
    // 顺序遵循 v1.6-4：**先脱敏、后截断**（先截断会把跨边界的密钥切成不匹配的碎片而漏扫）。
    let (mut sanitized_payload, redactions) =
        SanitizationPipeline::sanitize_trace_value(&raw_payload);
    if let Some(object) = sanitized_payload.as_object_mut() {
        object.insert(
            "_redactions".to_string(),
            json!({"secrets": redactions.secrets, "paths": redactions.paths}),
        );
    }

    // `I-02`（契约 v1.5-4）：`turns[]` 归一——非对象丢弃、status 非字符串→unknown、
    // tool_calls 非数组忽略、超长 tool 文本截断；warnings 回写 `payload._warnings`（不拒整条请求）。
    let turn_warnings =
        pony_sentry_core::trace_v2::normalize_incoming_turns(&mut sanitized_payload);
    if !turn_warnings.is_empty() {
        if let Some(object) = sanitized_payload.as_object_mut() {
            object.insert(
                "_warnings".to_string(),
                serde_json::Value::Array(turn_warnings),
            );
        }
    }

    // `C5`：`session_id` 优先，缺失/空白时回退 `thread_id`；两者皆空 → 400（纯文本，不含 DB 信息）。
    let session_id = match (
        payload.session_id.as_deref().map(str::trim),
        payload.thread_id.as_deref().map(str::trim),
    ) {
        (Some(value), _) if !value.is_empty() => value.to_string(),
        (_, Some(value)) if !value.is_empty() => value.to_string(),
        _ => {
            state
                .metrics
                .record_trace_ingest_rejected("session_missing");
            return Err((
                StatusCode::BAD_REQUEST,
                "Missing session_id or thread_id".to_string(),
            ));
        }
    };

    // `C7`：本次上报的 turn 状态分布 + `C4` status_conflict 探测（取归一后的 turns，只读）。
    let incoming_turns: Vec<serde_json::Value> = sanitized_payload
        .get("turns")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let turn_statuses: Vec<String> = incoming_turns
        .iter()
        .map(pony_sentry_core::trace_v2::turn_status_of)
        .collect();
    let status_conflict = incoming_turns.iter().any(|turn| {
        let mut probe = turn.clone();
        pony_sentry_core::trace_v2::annotate_turn(&mut probe)
            .is_some_and(|diagnostics| diagnostics.status_conflict)
    });

    let trace = TraceRecord {
        id: Uuid::new_v4().to_string(),
        session_id,
        run_id: payload.run_id,
        turn_id: payload.turn_id,
        environment: payload.environment.unwrap_or_else(|| "default".to_string()),
        release: payload.release.unwrap_or_else(|| "unknown".to_string()),
        project: payload.project,
        // `F10`（契约 v1.4-5）：忽略客户端 `eval_status`，一律 `unreviewed`；评审只能走 admin PATCH。
        eval_status: EvalStatus::Unreviewed,
        payload: sanitized_payload,
        total_input_tokens: payload.total_input_tokens,
        total_output_tokens: payload.total_output_tokens,
        total_duration_ms: payload.total_duration_ms,
        // `C1.2`：聚合列由 core 依据 turns 重算，入参占位为默认值；`note` 由 PATCH 写入。
        turn_status: "unknown".to_string(),
        turns_count: 0,
        note: None,
        reported_at,
        created_at: now,
        updated_at: now,
    };

    // `C5`：返回 core 同事务回查的库内持久化行（响应 id 可直接 GET，响应 total_* 与库一致）。
    let saved = trace_repo.record_trace(trace).await.map_err(|err| {
        error!(error = %err, "trace ingest failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL_ERROR_BODY.to_string(),
        )
    })?;

    state.metrics.record_trace_ingested(
        saved.project.as_deref().unwrap_or("unknown"),
        &saved.turn_status,
        &turn_statuses,
    );
    if status_conflict {
        state.metrics.record_trace_status_conflict();
    }

    // `I-06`（契约 v1.6-12）：payload_bytes 计**本次上报体积**（客户端请求序列化字节），
    // 与 HELP「上报体积（非存储体积）」一致；合并丢失/截断读 core 进程级计数。
    state.metrics.record_trace_payload_bytes(ingest_bytes);

    // `M-03`（契约 v1.5-10）：失败态 turn 触发可选异步 webhook（去重键 project|tool_name|turn_status）。
    if state.notifier.should_notify(&saved.turn_status) {
        let project = saved.project.as_deref().unwrap_or("unknown");
        let tool_name = first_tool_name(&saved.payload).unwrap_or_else(|| "unknown".to_string());
        // `E4`（v1.6-9）：去重键各分量由 notifier 侧归一（截断 64 + 非 `[A-Za-z0-9_.:-]`→`_`）。
        let dedupe_key = TraceNotifier::dedupe_key(project, &tool_name, &saved.turn_status);
        state.notifier.notify(
            dedupe_key,
            json!({
                "event": "trace_failure",
                "trace_id": saved.id,
                "session_id": saved.session_id,
                "run_id": saved.run_id,
                "project": saved.project,
                "environment": saved.environment,
                "release": saved.release,
                "turn_status": saved.turn_status,
                "turns_count": saved.turns_count,
                "tool_name": tool_name,
                "reported_at": saved.reported_at.to_rfc3339(),
            }),
        );
    }

    Ok((StatusCode::CREATED, Json(saved)))
}

/// `M-03`：取 payload 中首个 tool_call 的 `tool_name`（去重键的一部分）。
fn first_tool_name(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("turns")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find_map(|turn| {
            turn.get("tool_calls")
                .and_then(serde_json::Value::as_array)?
                .iter()
                .find_map(|call| {
                    call.get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
        })
}

async fn handle_list_traces(
    State(state): State<AppState>,
    Query(query): Query<TraceQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let trace_repo = state.trace_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "Trace repository not configured".into(),
        )
    })?;

    let eval_status = match query.eval_status {
        Some(s) => Some(s.parse::<EvalStatus>().map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Bad request: invalid `eval_status` parameter".to_string(),
            )
        })?),
        None => None,
    };

    // `C5` / `C9.4`：负值与非数字一律 400（不得静默降级）；上限 100 由 clamp 保证。
    let limit = match query.limit {
        Some(value) if value < 0 => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Bad request: `limit` must not be negative".to_string(),
            ));
        }
        Some(value) => Some(value.clamp(1, 100)),
        None => None,
    };
    let offset = match query.offset {
        Some(value) if value < 0 => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Bad request: `offset` must not be negative".to_string(),
            ));
        }
        other => other,
    };

    // `W-05` 后半（契约 v1.5-2）：排序维度白名单，非法值 → 400。
    let order_by = match query
        .order
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("reported_at") => None, // 契约默认 = ReportedAt
        Some("created_at") => Some(TraceOrder::CreatedAt),
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Bad request: invalid `order` parameter".to_string(),
            ));
        }
    };

    // `I-04`（契约 v1.5-7）：投影白名单，非法值 → 400。
    let summary_only = match query
        .fields
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("full") => false,
        Some("summary") => true,
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Bad request: invalid `fields` parameter".to_string(),
            ));
        }
    };

    let filter = TraceFilter {
        session_id: query.session_id,
        eval_status,
        environment: query.environment,
        release: query.release,
        project: query.project,
        run_id: query.run_id,
        turn_status: query.turn_status,
        from: query.from,
        to: query.to,
        order_by,
        limit,
        offset,
    };

    // `I-04`：`X-Total-Count` 必须与过滤条件一致（不随 limit/offset 变化）。
    let total = trace_repo
        .count_traces(filter.clone())
        .await
        .map_err(|err| {
            error!(error = %err, "trace count failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR_BODY.to_string(),
            )
        })?;

    let traces = trace_repo.list_traces(filter).await.map_err(|err| {
        error!(error = %err, "trace list failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL_ERROR_BODY.to_string(),
        )
    })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::HeaderName::from_static("x-total-count"),
        HeaderValue::from_str(&total.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );

    if summary_only {
        let summaries: Vec<TraceSummary> = traces.into_iter().map(TraceSummary::from).collect();
        return Ok((StatusCode::OK, headers, Json(summaries)).into_response());
    }
    Ok((StatusCode::OK, headers, Json(traces)).into_response())
}

async fn handle_get_trace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let trace_repo = state.trace_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "Trace repository not configured".into(),
        )
    })?;

    let trace = trace_repo.get_trace(&id).await.map_err(|err| match err {
        pony_sentry_core::RepositoryError::TraceNotFound(_)
        | pony_sentry_core::RepositoryError::NotFound(_) => {
            (StatusCode::NOT_FOUND, "Trace not found".into())
        }
        other => {
            error!(error = %other, "trace get failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR_BODY.to_string(),
            )
        }
    })?;

    Ok(Json(trace))
}

async fn handle_update_trace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    payload: Result<Json<UpdateTraceRequest>, JsonRejection>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // `F6`：PATCH 非法体统一 400 `Bad request`（不回显 serde 内部结构与合法枚举值）。
    let Json(body) = payload.map_err(json_rejection_response)?;

    let trace_repo = state.trace_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "Trace repository not configured".into(),
        )
    })?;

    // `C1.2` / `C9.7`：`note` 必须落库（`update_trace_review`，None 时保留库内旧值）。
    let updated = trace_repo
        .update_trace_review(&id, body.eval_status, body.note.as_deref())
        .await
        .map_err(|err| match err {
            pony_sentry_core::RepositoryError::TraceNotFound(_)
            | pony_sentry_core::RepositoryError::NotFound(_) => {
                (StatusCode::NOT_FOUND, "Trace not found".into())
            }
            other => {
                error!(error = %other, "trace review update failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    INTERNAL_ERROR_BODY.to_string(),
                )
            }
        })?;

    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_match_requires_exact_value() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "abcd"));
        assert!(!token_matches("", "abc"));
    }

    #[test]
    fn derived_error_type_prefers_top_level_field() {
        let payload = json!({
            "error_type": "ValidationError",
            "exception": {"error_type": "ValueError"}
        });
        assert_eq!(
            derived_error_type(&payload).as_deref(),
            Some("ValidationError")
        );
    }

    #[test]
    fn derived_error_type_falls_back_to_exception() {
        let payload = json!({"exception": {"error_type": "ValueError"}});
        assert_eq!(derived_error_type(&payload).as_deref(), Some("ValueError"));
        assert_eq!(derived_error_type(&json!({})), None);
        assert_eq!(
            derived_error_type(&json!({"error_type": "  "})),
            None,
            "空白等价缺失"
        );
    }

    #[test]
    fn metrics_label_values_are_escaped() {
        assert_eq!(escape_label("a\"b"), "a\\\"b");
        assert_eq!(escape_label("a\\b"), "a\\\\b");
    }

    #[test]
    fn write_auth_allows_when_no_token_configured() {
        assert_eq!(
            check_write_auth(None, None, &HeaderMap::new()),
            AuthOutcome::Allowed,
            "未配置 token 时写入沿用既有放行语义（契约 R10.1 收窄版）"
        );
        assert_eq!(
            check_admin_auth(None, None, &HeaderMap::new()),
            AuthOutcome::NotConfigured,
            "未配置 token 时读/PATCH fail-closed（契约 R10.1）"
        );
    }

    #[test]
    fn admin_auth_prefers_admin_token_and_falls_back_to_client_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-admin-token", "admin-tok".parse().unwrap());
        assert_eq!(
            check_admin_auth(Some("admin-tok"), Some("client-tok"), &headers),
            AuthOutcome::Allowed
        );

        let mut client_headers = HeaderMap::new();
        client_headers.insert("authorization", "Bearer client-tok".parse().unwrap());
        assert_eq!(
            check_admin_auth(Some("admin-tok"), Some("client-tok"), &client_headers),
            AuthOutcome::Allowed,
            "CLIENT_TOKEN 亦可通过管理侧鉴权（契约 R10 矩阵）"
        );

        let mut bad = HeaderMap::new();
        bad.insert("x-admin-token", "nope".parse().unwrap());
        assert_eq!(
            check_admin_auth(Some("admin-tok"), Some("client-tok"), &bad),
            AuthOutcome::Unauthorized
        );
    }

    #[test]
    fn admin_auth_requires_token_when_only_client_token_configured() {
        assert_eq!(
            check_admin_auth(None, Some("client-tok"), &HeaderMap::new()),
            AuthOutcome::Unauthorized
        );
        assert_eq!(
            check_write_auth(Some("client-tok"), None, &HeaderMap::new()),
            AuthOutcome::Unauthorized,
            "配置 CLIENT_TOKEN 后匿名写入必须 401"
        );
    }
}
