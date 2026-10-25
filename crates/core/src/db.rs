use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use std::time::Duration;

pub type DatabasePool = SqlitePool;

/// 建立 SQLite 连接池。
///
/// wave-9 issue 治理并发修复：**连接级 `busy_timeout = 5s`**——
/// 写锁被占用时排队等待，而不是立刻返回 `SQLITE_BUSY (code 5) database is locked`。
///
/// 注：仅追加 PRAGMA，不改变任何既有连接语义（`?mode=rwc`、`sqlite::memory:` 等 URL 语义
/// 由 `SqliteConnectOptions::from_str` 按既有默认解析；`journal_mode` 等其它参数保持 sqlx 默认，
/// 与改造前的 `connect(url)` 完全一致）。
/// 连接级 busy_timeout 只能覆盖「等锁」，无法覆盖 deferred 事务的锁升级死锁——
/// 那由 `sqlite_repository` 的 `BEGIN IMMEDIATE` + 整事务有界重试解决。
pub async fn create_pool(database_url: &str) -> Result<DatabasePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .busy_timeout(Duration::from_secs(5));
    SqlitePoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_millis(3000))
        .idle_timeout(Duration::from_secs(60))
        .connect_with(options)
        .await
}
