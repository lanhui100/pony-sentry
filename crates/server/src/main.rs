use pony_sentry_core::{create_pool, IssueRepository, PgIssueRepository, SqliteIssueRepository, TraceRepository};
use pony_sentry_server::{create_app_with_state, AppState};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let db_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:ponysentry.db?mode=rwc".into());
    let (repo, trace_repo): (Arc<dyn IssueRepository>, Option<Arc<dyn TraceRepository>>) =
        if db_url.starts_with("postgres://") || db_url.starts_with("postgresql://") {
            info!("Connecting to PostgreSQL database...");
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(Duration::from_millis(3000))
                .connect(&db_url)
                .await?;
            let pg_repo = PgIssueRepository::new(pool);
            pg_repo.migrate().await?;
            let shared = Arc::new(pg_repo);
            let repo: Arc<dyn IssueRepository> = shared.clone();
            let trace_repo: Arc<dyn TraceRepository> = shared;
            (repo, Some(trace_repo))
        } else {
            info!("Connecting to SQLite database (default)...");
            let pool = create_pool(&db_url).await?;
            let sqlite_repo = SqliteIssueRepository::new(pool);
            sqlite_repo.migrate().await?;
            let shared = Arc::new(sqlite_repo);
            let repo: Arc<dyn IssueRepository> = shared.clone();
            let trace_repo: Arc<dyn TraceRepository> = shared;
            (repo, Some(trace_repo))
        };

    // 可选客户端上报鉴权 Token：若配置了 CLIENT_TOKEN，则强制校验请求头
    let client_token = std::env::var("CLIENT_TOKEN").ok().filter(|t| !t.is_empty());
    let webhook_url = std::env::var("WEBHOOK_URL").ok().filter(|t| !t.is_empty());

    // 运维子命令：全量指纹重索引（幂等）。
    // 用法：PONY_REINDEX_FINGERPRINTS=1 pony-sentry-server
    // 重索引会按 v2 指纹方案重建 issue 归属，随后进程退出（不启动 HTTP 服务）。
    if std::env::var("PONY_REINDEX_FINGERPRINTS").as_deref() == Ok("1") {
        info!("Running fingerprint reindex (PONY_REINDEX_FINGERPRINTS=1)...");
        let summary = repo
            .reindex_fingerprints(&|payload: &serde_json::Value| {
                serde_json::from_value::<pony_sentry_ingest::RawEvent>(payload.clone())
                    .ok()
                    .map(|ev| pony_sentry_ingest::compute_issue_fingerprint(&ev))
            })
            .await?;
        info!(
            "Reindex complete: created={} reused={} deleted={} moved_events={} skipped={}",
            summary.issues_created,
            summary.issues_reused,
            summary.issues_deleted,
            summary.events_moved,
            summary.events_skipped
        );
        return Ok(());
    }

    let state = AppState {
        repo,
        trace_repo,
        client_token,
        webhook_url,
    };
    let app = create_app_with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("PonySentry server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
