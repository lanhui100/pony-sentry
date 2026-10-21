use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use pony_sentry_core::{
    models::{EvalStatus, IssueStatus, TraceFilter, TraceRecord},
    repository::{IssueFilter, IssueRepository, NewEventRecord, TraceRepository},
};
use pony_sentry_ingest::{
    compute_issue_fingerprint, guard_extra, infer_project_name, normalize_dimension,
    normalize_environment, normalize_grafana_url, normalize_level, normalize_loki_query,
    normalize_source_kind, normalize_stacktrace, normalize_tags, parse_timestamp, truncate_chars,
    validate_event_id, RawEvent, SanitizationPipeline, MESSAGE_MAX_CHARS,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tracing::{error, info, warn};
use uuid::Uuid;

/// 单次 DB 探活的硬超时（契约 R11.2 / NFR `db_probe_timeout_ms`）。
const DB_PROBE_TIMEOUT: Duration = Duration::from_millis(2000);

/// ingest 结果计数器（`/metrics` 的 `pony_sentry_ingest_requests_total` 数据源）。
#[derive(Debug, Default)]
pub struct IngestMetrics {
    created: AtomicU64,
    duplicate: AtomicU64,
    degraded_level: AtomicU64,
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
    pub metrics: Arc<IngestMetrics>,
}

impl AppState {
    pub fn new(repo: Arc<dyn IssueRepository>) -> Self {
        Self {
            repo,
            trace_repo: None,
            client_token: None,
            admin_token: None,
            webhook_url: None,
            metrics: Arc::new(IngestMetrics::default()),
        }
    }

    pub fn with_trace_repo(mut self, trace_repo: Option<Arc<dyn TraceRepository>>) -> Self {
        self.trace_repo = trace_repo;
        self
    }

    /// 管理侧请求的失败策略（契约 R10 / R10.1）：
    /// - 两个 token 都没配置 → `NotConfigured`（503 fail-closed）；
    /// - 提供了 `admin_token` 或 `client_token` 之一且匹配 → `Allowed`；
    /// - 其余 → `Unauthorized`。
    fn check_admin_auth(&self, headers: &HeaderMap) -> AuthOutcome {
        check_admin_auth(
            self.admin_token.as_deref(),
            self.client_token.as_deref(),
            headers,
        )
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
}

/// 鉴权判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthOutcome {
    Allowed,
    Unauthorized,
    NotConfigured,
}

/// 管理侧（读 + PATCH）鉴权规则（契约 R10 / R10.1）：
/// - `admin` 与 `client` 都未配置 → `NotConfigured`（503 fail-closed）；
/// - 任一配置且请求令牌与之匹配 → `Allowed`；否则 `Unauthorized`。
fn check_admin_auth(admin: Option<&str>, client: Option<&str>, headers: &HeaderMap) -> AuthOutcome {
    match (admin, client) {
        (None, None) => AuthOutcome::NotConfigured,
        _ => {
            let provided = extract_provided_token(headers);
            let matched = provided.is_some_and(|token| {
                [admin, client]
                    .into_iter()
                    .flatten()
                    .any(|expected| token_matches(token, expected))
            });
            if matched {
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
    let matched = provided.is_some_and(|token| {
        [client, admin]
            .into_iter()
            .flatten()
            .any(|expected| token_matches(token, expected))
    });
    if matched {
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
}

#[derive(Debug, Deserialize)]
pub struct TraceQuery {
    pub session_id: Option<String>,
    pub eval_status: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTraceRequest {
    pub eval_status: EvalStatus,
    pub note: Option<String>,
}

pub fn create_app_with_state(state: AppState) -> Router {
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
        .route("/api/v1/traces", post(handle_ingest_trace))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(ingest_cors);

    // 3. 面向 Web 控制台的管理路由 (不挂载通配跨域，防止 CSRF 跨域窃取)
    //    鉴权（契约 R10）：ADMIN_TOKEN 优先，回退 CLIENT_TOKEN；两者皆无 → 503 fail-closed。
    let admin_routes = Router::new()
        .route("/api/v1/issues", get(handle_list_issues))
        .route("/api/v1/projects", get(handle_list_projects))
        .route("/api/v1/issues/:id", get(handle_get_issue))
        .route(
            "/api/v1/issues/:id",
            patch(handle_update_issue).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/api/v1/issues/:id/events", get(handle_get_issue_events))
        .route("/api/v1/traces", get(handle_list_traces))
        .route("/api/v1/traces/:id", get(handle_get_trace))
        .route(
            "/api/v1/traces/:id",
            patch(handle_update_trace).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
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
        .merge(admin_routes)
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .with_state(state)
}

/// 管理侧鉴权中间件：读接口与 PATCH 一律先过 token 校验（契约 R10 / R10.1）。
async fn admin_auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match state.check_admin_auth(request.headers()) {
        AuthOutcome::Allowed => next.run(request).await,
        AuthOutcome::Unauthorized => unauthorized().into_response(),
        AuthOutcome::NotConfigured => admin_token_not_configured().into_response(),
    }
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

/// Prometheus 文本格式的 label 值转义。
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// `GET /metrics`：最小运维指标集（契约 R11.3）。
async fn handle_metrics(State(state): State<AppState>) -> Result<Response, (StatusCode, String)> {
    let counts = state
        .repo
        .issue_counts_by_status_environment()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let events_total = state
        .repo
        .count_events()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(issues))
}

async fn handle_list_projects(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut projects = state
        .repo
        .list_projects()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 聚合 trace 维度的 project（issue 之外的 Agent trace 项目也能进筛选下拉）
    if let Some(trace_repo) = &state.trace_repo {
        let trace_projects = trace_repo
            .list_projects()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    let issue = state.repo.get_issue(&id).await.map_err(|e| match e {
        pony_sentry_core::RepositoryError::NotFound(_) => {
            (StatusCode::NOT_FOUND, "Issue not found".into())
        }
        other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    })?;

    // 派生视图字段 `error_type`（事件级属性）：取该 issue 最近一条事件的载荷。
    let mut value = serde_json::to_value(&issue)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    Json(body): Json<UpdateIssueRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let status = match body.status {
        Some(s) => s
            .parse::<IssueStatus>()
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?,
        None => {
            let current = state
                .repo
                .get_issue(&id)
                .await
                .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
            current.status
        }
    };

    let updated = state
        .repo
        .update_issue_status(&id, status, body.assigned_to)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(events))
}

async fn handle_ingest_trace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AgentTraceIngestRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if state.check_write_auth(&headers) == AuthOutcome::Unauthorized {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid or missing client token".into(),
        ));
    }

    let trace_repo = state.trace_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "Trace repository not configured".into(),
        )
    })?;

    let now = chrono::Utc::now();
    let reported_at = payload
        .reported_at_ms
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
        .unwrap_or(now);

    let raw_payload = serde_json::to_value(&payload).unwrap_or_else(|_| serde_json::json!({}));

    // 服务端二次脱敏：客户端 SDK 已脱敏一次，入库前按同一管道再扫一遍，
    // 防止绕过客户端的敏感字段（token/secret/password 等）落库。
    let sanitized_payload = SanitizationPipeline::sanitize_json_value(raw_payload, 0);

    let session_id = payload
        .session_id
        .clone()
        .or_else(|| payload.thread_id.clone())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Missing session_id or thread_id".to_string(),
            )
        })?;

    if session_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "session_id cannot be empty".to_string(),
        ));
    }

    let trace = TraceRecord {
        id: Uuid::new_v4().to_string(),
        session_id,
        run_id: payload.run_id,
        turn_id: payload.turn_id,
        environment: payload.environment.unwrap_or_else(|| "default".to_string()),
        release: payload.release.unwrap_or_else(|| "unknown".to_string()),
        project: payload.project,
        eval_status: payload.eval_status.unwrap_or(EvalStatus::Unreviewed),
        payload: sanitized_payload,
        total_input_tokens: payload.total_input_tokens,
        total_output_tokens: payload.total_output_tokens,
        total_duration_ms: payload.total_duration_ms,
        reported_at,
        created_at: now,
        updated_at: now,
    };

    let saved = trace_repo
        .record_trace(trace)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((StatusCode::CREATED, Json(saved)))
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
        Some(s) => Some(
            s.parse::<EvalStatus>()
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?,
        ),
        None => None,
    };

    let filter = TraceFilter {
        session_id: query.session_id,
        eval_status,
        environment: query.environment,
        release: query.release,
        project: query.project,
        limit: query.limit,
        offset: query.offset,
    };

    let traces = trace_repo
        .list_traces(filter)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(traces))
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

    let trace = trace_repo.get_trace(&id).await.map_err(|e| match e {
        pony_sentry_core::RepositoryError::TraceNotFound(_)
        | pony_sentry_core::RepositoryError::NotFound(_) => {
            (StatusCode::NOT_FOUND, "Trace not found".into())
        }
        other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    })?;

    Ok(Json(trace))
}

async fn handle_update_trace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateTraceRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let trace_repo = state.trace_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            "Trace repository not configured".into(),
        )
    })?;

    let updated = trace_repo
        .update_trace_eval_status(&id, body.eval_status)
        .await
        .map_err(|e| match e {
            pony_sentry_core::RepositoryError::TraceNotFound(_)
            | pony_sentry_core::RepositoryError::NotFound(_) => {
                (StatusCode::NOT_FOUND, "Trace not found".into())
            }
            other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
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
