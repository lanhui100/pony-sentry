use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use pony_sentry_core::{
    models::{EvalStatus, IssueStatus, TraceFilter, TraceRecord},
    repository::{IssueFilter, IssueRepository, TraceRepository},
};
use pony_sentry_ingest::{
    compute_issue_fingerprint, infer_project_name, RawEvent, SanitizationPipeline,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn IssueRepository>,
    pub trace_repo: Option<Arc<dyn TraceRepository>>,
    pub client_token: Option<String>,
    pub webhook_url: Option<String>,
}

impl AppState {
    pub fn new(repo: Arc<dyn IssueRepository>) -> Self {
        Self {
            repo,
            trace_repo: None,
            client_token: None,
            webhook_url: None,
        }
    }

    pub fn with_trace_repo(mut self, trace_repo: Option<Arc<dyn TraceRepository>>) -> Self {
        self.trace_repo = trace_repo;
        self
    }
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
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub issue_id: String,
    pub event_id: String,
    pub fingerprint: String,
    pub status: IssueStatus,
    pub count: i64,
}

#[derive(Debug, Deserialize)]
pub struct IssueQuery {
    pub status: Option<String>,
    pub platform: Option<String>,
    pub release: Option<String>,
    pub project: Option<String>,
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
    pub session_id: String,
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
        ]);

    // 2. 遥测上报路由 (强制挂载 512KB 请求体硬限制与专有 CORS)
    let ingest_routes = Router::new()
        .route("/api/v1/ingest", post(handle_ingest))
        .route("/api/v1/traces", post(handle_ingest_trace))
        .layer(DefaultBodyLimit::max(512 * 1024))
        .layer(ingest_cors);

    // 3. 面向 Web 控制台的管理路由 (不挂载通配跨域，防止 CSRF 跨域窃取)
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
        );

    let html_content = include_str!("../../../web/dist/index.html");

    Router::new()
        .route(
            "/",
            get(move || async move { axum::response::Html(html_content) }),
        )
        .route("/healthz", get(|| async { "OK" }))
        .merge(ingest_routes)
        .merge(admin_routes)
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .with_state(state)
}

async fn handle_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<IngestPayload>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // 若配置了 CLIENT_TOKEN，验证客户端请求头
    if let Some(ref expected_token) = state.client_token {
        let provided = headers
            .get("x-client-token")
            .and_then(|h| h.to_str().ok())
            .or_else(|| {
                headers
                    .get(header::AUTHORIZATION)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.strip_prefix("Bearer "))
            });

        match provided {
            Some(t) if t.trim() == expected_token => {}
            _ => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "Invalid or missing client token".into(),
                ))
            }
        }
    }
    let raw_event = RawEvent {
        platform: payload.platform.unwrap_or_else(|| "other".into()),
        release: payload.release,
        environment: payload.environment,
        message: payload.message,
        exception: payload.exception,
        tags: payload.tags,
        extra: payload.extra,
        breadcrumbs: payload.breadcrumbs,
    };

    // 1. 脱敏
    let sanitized = SanitizationPipeline::sanitize_event(raw_event);

    // 2. 计算指纹与提取核心信息（唯一真相源：compute_issue_fingerprint）
    let info = compute_issue_fingerprint(&sanitized);
    let title = info.title.clone();
    let culprit = info.culprit.clone();
    let fingerprint = info.fingerprint.clone();

    // 3. 项目维度：使用 infer_project_name 综合 tags、extra、title 与 culprit 智能识别
    let project = infer_project_name(
        sanitized.tags.as_ref(),
        sanitized.extra.as_ref(),
        Some(&title),
        culprit.as_deref(),
    );

    // 4. 持久化与状态机流转
    let payload_json = serde_json::to_value(&sanitized)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let res = state
        .repo
        .record_event_and_upsert_issue(
            &fingerprint,
            &title,
            culprit.as_deref(),
            &sanitized.platform,
            sanitized.release.as_deref(),
            sanitized.environment.as_deref(),
            project.as_deref(),
            payload_json,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let issue = res.issue;
    let event = res.event;

    // 5. 仅当新问题创建 (is_new) 或首次从 Resolved 状态回归 (is_regression_trigger) 时分发 Webhook，杜绝雪崩
    if res.is_new || res.is_regression_trigger {
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

    Ok(Json(IngestResponse {
        issue_id: issue.id,
        event_id: event.id,
        fingerprint,
        status: issue.status,
        count: issue.count,
    }))
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
    let projects = state
        .repo
        .list_projects()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(projects))
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

    Ok(Json(issue))
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
    if let Some(ref expected_token) = state.client_token {
        let provided = headers
            .get("x-client-token")
            .and_then(|h| h.to_str().ok())
            .or_else(|| {
                headers
                    .get(header::AUTHORIZATION)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.strip_prefix("Bearer "))
            });

        match provided {
            Some(t) if t.trim() == expected_token => {}
            _ => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "Invalid or missing client token".into(),
                ))
            }
        }
    }

    let trace_repo = state
        .trace_repo
        .as_ref()
        .ok_or_else(|| (StatusCode::NOT_IMPLEMENTED, "Trace repository not configured".into()))?;

    let now = chrono::Utc::now();
    let reported_at = payload
        .reported_at_ms
        .and_then(|ms| {
            chrono::DateTime::from_timestamp_millis(ms as i64)
        })
        .unwrap_or(now);

    let raw_payload = serde_json::to_value(&payload)
        .unwrap_or_else(|_| serde_json::json!({}));

    let trace = TraceRecord {
        id: Uuid::new_v4().to_string(),
        session_id: payload.session_id,
        run_id: payload.run_id,
        turn_id: payload.turn_id,
        environment: payload.environment.unwrap_or_else(|| "default".to_string()),
        release: payload.release.unwrap_or_else(|| "unknown".to_string()),
        eval_status: payload.eval_status.unwrap_or(EvalStatus::Unreviewed),
        payload: raw_payload,
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
    let trace_repo = state
        .trace_repo
        .as_ref()
        .ok_or_else(|| (StatusCode::NOT_IMPLEMENTED, "Trace repository not configured".into()))?;

    let eval_status = match query.eval_status {
        Some(s) => Some(s.parse::<EvalStatus>().map_err(|e| (StatusCode::BAD_REQUEST, e))?),
        None => None,
    };

    let filter = TraceFilter {
        session_id: query.session_id,
        eval_status,
        environment: query.environment,
        release: query.release,
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
    let trace_repo = state
        .trace_repo
        .as_ref()
        .ok_or_else(|| (StatusCode::NOT_IMPLEMENTED, "Trace repository not configured".into()))?;

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
    let trace_repo = state
        .trace_repo
        .as_ref()
        .ok_or_else(|| (StatusCode::NOT_IMPLEMENTED, "Trace repository not configured".into()))?;

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
