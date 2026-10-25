use chrono::Utc;
use pony_sentry_core::{
    create_pool, models::IssueStatus, IssueRepository, PgIssueRepository, SqliteIssueRepository,
    TraceRepository,
};
use pony_sentry_server::{
    create_app_with_options, session::SessionStore, trace_ingest::TraceIngestSwitch,
    trust::TrustConfig, AppState,
};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// retention 配置（契约 R11.1 / NFR `retention`）。
#[derive(Debug, Clone, Copy)]
struct RetentionConfig {
    events_days: i64,
    traces_days: i64,
    dry_run: bool,
}

/// retention 单批删除行数（契约 R11.1：≤10000 行/批；NFR 定 5000）。
const RETENTION_BATCH_ROWS: i64 = 5000;
/// 启动后首轮延迟（契约 R11.1：≤60s，错开启动流量高峰）。
const RETENTION_INITIAL_DELAY: Duration = Duration::from_secs(30);
/// 每日一次。
const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// 单次 DB 操作硬超时，避免慢库拖住清理任务。
const RETENTION_OP_TIMEOUT: Duration = Duration::from_secs(30);

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// `RETENTION_DISABLED=1` 关闭清理；`RETENTION_DRY_RUN=1` 只打印计划不删除。
fn retention_config() -> Option<RetentionConfig> {
    if std::env::var("RETENTION_DISABLED").as_deref() == Ok("1") {
        return None;
    }
    Some(RetentionConfig {
        events_days: env_i64("RETENTION_DAYS_EVENTS", 180),
        traces_days: env_i64("RETENTION_DAYS_TRACES", 90),
        dry_run: std::env::var("RETENTION_DRY_RUN").as_deref() == Ok("1"),
    })
}

/// 分批删除直到没有更多过期行（单批独立语句，批间释放事务，避免长事务阻塞 ingest）。
async fn purge_events(repo: &Arc<dyn IssueRepository>, cutoff: chrono::DateTime<Utc>) -> u64 {
    let mut total = 0u64;
    loop {
        match tokio::time::timeout(
            RETENTION_OP_TIMEOUT,
            repo.delete_events_older_than(cutoff, RETENTION_BATCH_ROWS),
        )
        .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(deleted)) => total += deleted,
            Ok(Err(err)) => {
                warn!(error = %err, "retention.events 删除失败，本轮中止");
                break;
            }
            Err(_) => {
                warn!("retention.events 删除超时，本轮中止");
                break;
            }
        }
    }
    total
}

async fn purge_traces(trace_repo: &Arc<dyn TraceRepository>, cutoff: chrono::DateTime<Utc>) -> u64 {
    let mut total = 0u64;
    loop {
        match tokio::time::timeout(
            RETENTION_OP_TIMEOUT,
            trace_repo.delete_traces_older_than(cutoff, RETENTION_BATCH_ROWS),
        )
        .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(deleted)) => total += deleted,
            Ok(Err(err)) => {
                warn!(error = %err, "retention.traces 删除失败，本轮中止");
                break;
            }
            Err(_) => {
                warn!("retention.traces 删除超时，本轮中止");
                break;
            }
        }
    }
    total
}

/// 内置 retention 每日任务（契约 R11.1）。
fn spawn_retention(
    repo: Arc<dyn IssueRepository>,
    trace_repo: Option<Arc<dyn TraceRepository>>,
    config: RetentionConfig,
) {
    tokio::spawn(async move {
        tokio::time::sleep(RETENTION_INITIAL_DELAY).await;
        loop {
            let events_cutoff = Utc::now() - chrono::Duration::days(config.events_days);
            let traces_cutoff = Utc::now() - chrono::Duration::days(config.traces_days);
            if config.dry_run {
                // 干跑：只输出计划（用于验收与首次上线观察），不执行删除。
                info!(
                    dry_run = true,
                    events_older_than_days = config.events_days,
                    traces_older_than_days = config.traces_days,
                    events_cutoff = %events_cutoff,
                    traces_cutoff = %traces_cutoff,
                    batch_rows = RETENTION_BATCH_ROWS,
                    "retention 干跑：跳过删除"
                );
            } else {
                let events_deleted = purge_events(&repo, events_cutoff).await;
                let traces_deleted = match trace_repo.as_ref() {
                    Some(tracer) => purge_traces(tracer, traces_cutoff).await,
                    None => 0,
                };
                info!(
                    events_deleted,
                    traces_deleted,
                    events_older_than_days = config.events_days,
                    traces_older_than_days = config.traces_days,
                    "retention 完成"
                );
            }
            tokio::time::sleep(RETENTION_INTERVAL).await;
        }
    });
}

/// `C2.2`：issue 归档 TTL 作业配置（环境变量解析结果）。
#[derive(Debug, Clone, Copy)]
struct IssueArchiveConfig {
    ttl_secs: u64,
    interval_secs: u64,
}

/// `C2.2`：`PONY_ISSUE_ARCHIVE_TTL_DAYS` 缺省 = 30 天。
const ISSUE_ARCHIVE_DEFAULT_TTL_DAYS: u64 = 30;
/// `C2.2`：`PONY_ISSUE_ARCHIVE_INTERVAL_SECS` 缺省 = 3600（亦为生产值）。
const ISSUE_ARCHIVE_DEFAULT_INTERVAL_SECS: u64 = 3600;
/// `C2.2` / `amend v1.1 B-10`：单轮失败后的重试间隔上限 = `min(INTERVAL_SECS, 60)`。
const ISSUE_ARCHIVE_FAILURE_BACKOFF_CAP_SECS: u64 = 60;
/// 单轮归档 DB 操作硬超时（慢库不得拖住作业循环）。
const ISSUE_ARCHIVE_OP_TIMEOUT: Duration = Duration::from_secs(30);

/// `C2.2`：读取正 u64 环境变量；缺失/非法/0 一律 `None`（由调用方回落默认值 + `warn!`）。
fn env_positive_u64(name: &'static str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match raw.parse::<u64>() {
        Ok(value) if value > 0 => Some(value),
        Ok(_) => {
            warn!(env = name, value = raw, "环境变量为 0，回落默认值");
            None
        }
        Err(_) => {
            warn!(env = name, value = raw, "环境变量非整数，回落默认值");
            None
        }
    }
}

/// `C2.2`：`PONY_ISSUE_ARCHIVE_*` 解析（非法值只 `warn!` 回落默认，**不得 panic**）。
///
/// 返回 `None` 表示自动作业关闭（手动归档接口不受影响）。
fn issue_archive_config() -> Option<IssueArchiveConfig> {
    let enabled = match std::env::var("PONY_ISSUE_ARCHIVE_ENABLED") {
        Err(_) => true,
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "true" | "on" => true,
            "0" | "false" | "off" => false,
            other => {
                warn!(
                    value = other,
                    "PONY_ISSUE_ARCHIVE_ENABLED 非法值，按启用处理"
                );
                true
            }
        },
    };
    if !enabled {
        return None;
    }

    // TTL 解析顺序写死：`TTL_SECS`(>0) > `TTL_DAYS`(>0) > 30 天。
    let ttl_secs = match env_positive_u64("PONY_ISSUE_ARCHIVE_TTL_SECS") {
        Some(secs) => secs,
        None => env_positive_u64("PONY_ISSUE_ARCHIVE_TTL_DAYS")
            .unwrap_or(ISSUE_ARCHIVE_DEFAULT_TTL_DAYS)
            .saturating_mul(24 * 60 * 60),
    };
    // `INTERVAL_SECS` 最小 1（`<1` → 取 1 + `warn!`；非法字面量 → 回落默认 3600 + `warn!`）。
    let interval_secs = match std::env::var("PONY_ISSUE_ARCHIVE_INTERVAL_SECS") {
        Err(_) => ISSUE_ARCHIVE_DEFAULT_INTERVAL_SECS,
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => {
                warn!(
                    env = "PONY_ISSUE_ARCHIVE_INTERVAL_SECS",
                    value = raw.trim(),
                    "周期 < 1，按最小 1 秒执行"
                );
                1
            }
            Ok(secs) => secs,
            Err(_) => {
                warn!(
                    env = "PONY_ISSUE_ARCHIVE_INTERVAL_SECS",
                    value = raw.trim(),
                    "环境变量非整数，回落默认值"
                );
                ISSUE_ARCHIVE_DEFAULT_INTERVAL_SECS
            }
        },
    };
    Some(IssueArchiveConfig {
        ttl_secs,
        interval_secs,
    })
}

/// `C2.2`：自动 TTL 归档作业——首轮立即执行，之后固定周期；单轮失败只 `warn!` 并按
/// `min(interval, 60)` 秒退避重试（`amend v1.1 B-10`），永不终止循环、永不退出进程。
///
/// 调用方必须保证：本函数在 schema 迁移**成功返回之后**才被调用（首轮时序要求）。
fn spawn_issue_archive_job(repo: Arc<dyn IssueRepository>, config: IssueArchiveConfig) {
    tokio::spawn(async move {
        let mut next_delay = Duration::ZERO;
        loop {
            if !next_delay.is_zero() {
                tokio::time::sleep(next_delay).await;
            }
            let cutoff = Utc::now() - chrono::Duration::seconds(config.ttl_secs as i64);
            // `C2.1`：只归档 `ignored`/`resolved`；`unresolved`/`in_progress`/`regression` 永不自动归档。
            let outcome = tokio::time::timeout(
                ISSUE_ARCHIVE_OP_TIMEOUT,
                repo.archive_stale_issues(
                    cutoff,
                    &[IssueStatus::Ignored, IssueStatus::Resolved],
                ),
            )
            .await;
            let succeeded = match outcome {
                Ok(Ok(archived)) => {
                    info!(
                        scope = "issue_archive",
                        archived,
                        cutoff = %cutoff,
                        ttl_secs = config.ttl_secs,
                        interval_secs = config.interval_secs,
                        "issue archive round completed"
                    );
                    true
                }
                Ok(Err(err)) => {
                    warn!(
                        scope = "issue_archive",
                        error = %err,
                        cutoff = %cutoff,
                        ttl_secs = config.ttl_secs,
                        interval_secs = config.interval_secs,
                        "issue archive round failed"
                    );
                    false
                }
                Err(_) => {
                    warn!(
                        scope = "issue_archive",
                        cutoff = %cutoff,
                        ttl_secs = config.ttl_secs,
                        interval_secs = config.interval_secs,
                        "issue archive round timed out"
                    );
                    false
                }
            };
            next_delay = if succeeded {
                Duration::from_secs(config.interval_secs)
            } else {
                Duration::from_secs(config.interval_secs.min(ISSUE_ARCHIVE_FAILURE_BACKOFF_CAP_SECS))
            };
        }
    });
}

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
                // 控制台前端 API_TIMEOUT_MS（web/src/lib/apiTimeout.js）必须严格大于本值 + 2000ms，见 .dev-team/contract-console-timeout.md
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
    // 管理侧令牌：读接口与 PATCH 需要；未配置时回退 CLIENT_TOKEN，两者皆无则 fail-closed 503。
    let admin_token = std::env::var("ADMIN_TOKEN").ok().filter(|t| !t.is_empty());
    let webhook_url = std::env::var("WEBHOOK_URL").ok().filter(|t| !t.is_empty());
    // 可选 `/metrics` 鉴权令牌（L3B-F11）：未配置时保持免鉴权（探针/README 现状）。
    let metrics_token = std::env::var("METRICS_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    // 可选失败态 trace webhook（M-03 / 契约 v1.5-10）：未配置时零副作用。
    let trace_webhook_url = std::env::var("TRACE_WEBHOOK_URL")
        .ok()
        .filter(|t| !t.is_empty());
    if admin_token.is_none() && client_token.is_none() {
        warn!(
            "未配置 ADMIN_TOKEN / CLIENT_TOKEN：读接口与 PATCH 将按 fail-closed 返回 503 \
             admin_token_not_configured（写入端点沿用既有放行语义）"
        );
    }

    // 运维子命令：全量指纹重索引（幂等）。
    // 用法：PONY_REINDEX_FINGERPRINTS=1 pony-sentry-server
    // 警告：会按 v3 指纹方案重塑 issue 分组，仅运维手动执行。
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

    // 内置 retention：每日分批清理 events / traces 过期行（issues 聚合永久保留）。
    match retention_config() {
        Some(config) => {
            info!(
                events_days = config.events_days,
                traces_days = config.traces_days,
                dry_run = config.dry_run,
                "retention 任务已启用"
            );
            spawn_retention(repo.clone(), trace_repo.clone(), config);
        }
        None => info!("retention 任务已通过 RETENTION_DISABLED=1 关闭"),
    }

    // 内置 issue 归档 TTL 作业（契约 C2.2）：**schema 迁移已成功返回之后**启动
    //（`repo.migrate()` 在构造仓储时 await 完成），`tokio::spawn` 不阻塞监听；首轮立即执行。
    match issue_archive_config() {
        Some(config) => {
            info!(
                ttl_secs = config.ttl_secs,
                interval_secs = config.interval_secs,
                "issue 归档 TTL 作业已启用（首轮立即执行）"
            );
            spawn_issue_archive_job(repo.clone(), config);
        }
        None => info!("issue 归档 TTL 作业已通过 PONY_ISSUE_ARCHIVE_ENABLED=0 关闭（手动接口仍可用）"),
    }

    let session_ttl_secs = std::env::var("CONSOLE_SESSION_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 60)
        .unwrap_or(pony_sentry_server::session::DEFAULT_SESSION_TTL_SECS)
        .min(86_400);
    let cookie_secure = std::env::var("CONSOLE_COOKIE_SECURE").as_deref() == Ok("1");
    info!(
        session_ttl_secs,
        cookie_secure, "控制台服务端会话已启用（HttpOnly + SameSite=Strict）"
    );
    let state = AppState {
        repo,
        trace_repo,
        client_token,
        admin_token,
        webhook_url,
        metrics_token,
        notifier: std::sync::Arc::new(pony_sentry_server::TraceNotifier::new(trace_webhook_url)),
        metrics: Arc::default(),
        sessions: Arc::new(SessionStore::new(
            Duration::from_secs(session_ttl_secs),
            cookie_secure,
        )),
    };
    // Tailnet 信任配置（契约 §1）：默认关闭，仅由部署 env 显式开启；非法配置 fail-closed + WARN。
    let trust = TrustConfig::from_env();
    info!(
        trust_enabled = trust.enabled,
        "tailnet 信任判定已接入管理读路由与会话自动签发（变更请求仍需会话 Cookie + CSRF）"
    );
    // tracing 接收开关（`PONY_TRACE_INGEST_ENABLED`，默认开启）：停收时 4 个 trace 路由
    // 返回 410，Issue 采集 / 探针 / 指标语义不变。启动日志在本函数内只打一次。
    let trace_ingest = TraceIngestSwitch::from_env();
    let app = create_app_with_options(state, trust, trace_ingest);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("PonySentry server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    // 必须取得 TCP 对端地址：`TrustConfig::decide` 的 peer 来源，缺省即 PeerUnknown（绝不 fail-open）。
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
