use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use pony_sentry_core::{
    models::IssueStatus,
    repository::{IssueFilter, IssueRepository},
};
use pony_sentry_fingerprint::FingerprintEngine;
use pony_sentry_ingest::{RawEvent, SanitizationPipeline};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;

#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn IssueRepository>,
    pub client_token: Option<String>,
    pub webhook_url: Option<String>,
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
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIssueRequest {
    pub status: Option<String>,
    pub assigned_to: Option<String>,
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
        .layer(DefaultBodyLimit::max(512 * 1024))
        .layer(ingest_cors);

    // 3. 面向 Web 控制台的管理路由 (不挂载通配跨域，防止 CSRF 跨域窃取)
    let admin_routes = Router::new()
        .route("/api/v1/issues", get(handle_list_issues))
        .route("/api/v1/issues/:id", get(handle_get_issue))
        .route("/api/v1/issues/:id", patch(handle_update_issue).layer(DefaultBodyLimit::max(64 * 1024)))
        .route("/api/v1/issues/:id/events", get(handle_get_issue_events));

    let html_content = include_str!("../../../web/dist/index.html");

    Router::new()
        .route("/", get(move || async move {
            axum::response::Html(html_content)
        }))
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
            _ => return Err((StatusCode::UNAUTHORIZED, "Invalid or missing client token".into())),
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

    // 2. 计算指纹与提取核心信息
    let (error_type, culprit, title) = if let Some(ref ex) = sanitized.exception {
        let cul = ex.stacktrace.as_ref().and_then(|frames| {
            frames.first().and_then(|f| {
                match (&f.filename, &f.function) {
                    (Some(file), Some(func)) => Some(format!("{} in {}", file, func)),
                    (Some(file), None) => Some(file.clone()),
                    (None, Some(func)) => Some(func.clone()),
                    _ => None,
                }
            })
        });
        (
            ex.error_type.clone(),
            cul,
            format!("{}: {}", ex.error_type, ex.value.as_deref().unwrap_or("")),
        )
    } else {
        (
            "GenericError".to_string(),
            None,
            sanitized.message.clone().unwrap_or_else(|| "Unknown error".to_string()),
        )
    };

    let fingerprint = FingerprintEngine::compute_fingerprint(
        &error_type,
        culprit.as_deref(),
        &sanitized.platform,
    );

    // 3. 持久化与状态机流转
    let payload_json = serde_json::to_value(&sanitized)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (issue, event) = state
        .repo
        .record_event_and_upsert_issue(
            &fingerprint,
            &title,
            culprit.as_deref(),
            &sanitized.platform,
            sanitized.release.as_deref(),
            sanitized.environment.as_deref(),
            payload_json,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 4. 若为新问题或回归复现，异步分发 Webhook
    if issue.count == 1 || issue.status == IssueStatus::Regression {
        if let Some(ref webhook_url) = state.webhook_url {
            let webhook_url = webhook_url.clone();
            let event_type = if issue.count == 1 { "issue.created" } else { "issue.regression" };
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
                    "project_path": sanitized.extra.as_ref().and_then(|x| x.get("project_path")).and_then(|v| v.as_str()),
                },
                "latest_event": {
                    "id": event.id,
                    "message": sanitized.message,
                    "exception": sanitized.exception,
                    "breadcrumbs": sanitized.breadcrumbs,
                }
            });

            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build();
                if let Ok(client) = client {
                    let _ = client.post(&webhook_url).json(&webhook_payload).send().await;
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

async fn handle_get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let issue = state
        .repo
        .get_issue(&id)
        .await
        .map_err(|e| match e {
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
        Some(s) => s.parse::<IssueStatus>().map_err(|e| (StatusCode::BAD_REQUEST, e))?,
        None => {
            let current = state.repo.get_issue(&id).await.map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
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
