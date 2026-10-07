use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::time::Duration;

pub type DatabasePool = SqlitePool;

pub async fn create_pool(database_url: &str) -> Result<DatabasePool, sqlx::Error> {
    SqlitePoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_millis(3000))
        .idle_timeout(Duration::from_secs(60))
        .connect(database_url)
        .await
}
