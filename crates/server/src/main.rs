use pony_sentry_core::{create_pool, IssueRepository, PgIssueRepository, SqliteIssueRepository};
use pony_sentry_server::{create_app_with_state, AppState};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:ponysentry.db?mode=rwc".into());
    let repo: Arc<dyn IssueRepository> = if db_url.starts_with("postgres://") || db_url.starts_with("postgresql://") {
        info!("Connecting to PostgreSQL database...");
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_millis(3000))
            .connect(&db_url)
            .await?;
        let pg_repo = PgIssueRepository::new(pool);
        pg_repo.migrate().await?;
        Arc::new(pg_repo)
    } else {
        info!("Connecting to SQLite database (default)...");
        let pool = create_pool(&db_url).await?;
        let sqlite_repo = SqliteIssueRepository::new(pool);
        sqlite_repo.migrate().await?;
        Arc::new(sqlite_repo)
    };

    // 可选客户端上报鉴权 Token：若配置了 CLIENT_TOKEN，则强制校验请求头
    let client_token = std::env::var("CLIENT_TOKEN").ok().filter(|t| !t.is_empty());
    let webhook_url = std::env::var("WEBHOOK_URL").ok().filter(|t| !t.is_empty());

    let state = AppState { repo, client_token, webhook_url };
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
