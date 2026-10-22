//! 控制台服务端会话与短期令牌。
//!
//! 浏览器只持有 HttpOnly 会话 Cookie；静态 ADMIN_TOKEN 只在登录时交换一次，
//! 不进入前端构建产物。会话记录保存在单进程内存中，重启即全部失效（部署为单副本）。

use axum::http::{header, HeaderMap};
use chrono::{DateTime, Utc};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const SESSION_COOKIE_NAME: &str = "pony_sentry_session";
pub const DEFAULT_SESSION_TTL_SECS: u64 = 28_800;
const MAX_SESSIONS: usize = 512;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_MAX_FAILURES: u32 = 5;

/// 会话签发来源：决定会话表满时的驱逐优先级。
///
/// `Manual`（静态 ADMIN_TOKEN/CLIENT_TOKEN 登录）持有写权限，优先保护；
/// `TrustedPeer`（tailnet 可信对端自动签发）可被无凭证流量触发签发，
/// 驱逐时先淘汰它，避免未认证流量挤掉管理员的手动写会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOrigin {
    Manual,
    TrustedPeer,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub expires_at: DateTime<Utc>,
    pub csrf_token: String,
    pub origin: SessionOrigin,
}

#[derive(Debug)]
pub struct CreatedSession {
    pub token: String,
    pub info: SessionInfo,
}

#[derive(Debug)]
struct SessionRecord {
    expires_at: DateTime<Utc>,
    csrf_token: String,
    origin: SessionOrigin,
}

#[derive(Debug)]
struct LoginWindowState {
    started: Instant,
    failures: u32,
}

#[derive(Debug)]
struct Inner {
    sessions: HashMap<String, SessionRecord>,
    login_windows: HashMap<String, LoginWindowState>,
}

pub struct SessionStore {
    inner: Mutex<Inner>,
    ttl: Duration,
    secure_cookie: bool,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new(Duration::from_secs(DEFAULT_SESSION_TTL_SECS), false)
    }
}

impl SessionStore {
    pub fn new(ttl: Duration, secure_cookie: bool) -> Self {
        Self {
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                login_windows: HashMap::new(),
            }),
            ttl,
            secure_cookie,
        }
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl.as_secs()
    }

    /// 手动签发的会话（静态 token 登录）：`create_with_origin(SessionOrigin::Manual)` 的薄封装。
    pub fn create(&self) -> CreatedSession {
        self.create_session(SessionOrigin::Manual)
    }

    /// 按来源签发会话，返回 `(session_token, csrf_token)`；驱逐策略见 [`Self::create_session`]。
    pub fn create_with_origin(&self, origin: SessionOrigin) -> (String, String) {
        let created = self.create_session(origin);
        (created.token, created.info.csrf_token)
    }

    /// 插入会话并在满表时驱逐。宽严有序（fail-safe 而非 fail-open）：
    /// ① 先清理已过期 → ② 仍满则只驱逐 `TrustedPeer` 中最早过期者
    /// → ③ 没有可驱逐的 `TrustedPeer`（全为手动会话）才回退驱逐最早过期者（既有兜底）。
    fn create_session(&self, origin: SessionOrigin) -> CreatedSession {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(self.ttl.as_secs() as i64);
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let csrf_token = Uuid::new_v4().simple().to_string();
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner.sessions.retain(|_, record| record.expires_at > now);
        if inner.sessions.len() >= MAX_SESSIONS {
            let evicted = inner
                .sessions
                .iter()
                .filter(|(_, record)| record.origin == SessionOrigin::TrustedPeer)
                .min_by_key(|(_, record)| record.expires_at)
                .map(|(token, _)| token.clone())
                .or_else(|| {
                    // 全为手动会话：沿用既有兜底，避免无界增长。
                    inner
                        .sessions
                        .iter()
                        .min_by_key(|(_, record)| record.expires_at)
                        .map(|(token, _)| token.clone())
                });
            if let Some(evicted) = evicted {
                inner.sessions.remove(&evicted);
            }
        }
        inner.sessions.insert(
            token.clone(),
            SessionRecord {
                expires_at,
                csrf_token: csrf_token.clone(),
                origin,
            },
        );
        CreatedSession {
            token,
            info: SessionInfo {
                expires_at,
                csrf_token,
                origin,
            },
        }
    }

    pub fn validate(&self, token: &str) -> Option<SessionInfo> {
        if !is_valid_session_token(token) {
            return None;
        }
        let now = Utc::now();
        let mut inner = self.inner.lock().expect("session store poisoned");
        let record = inner.sessions.get(token)?;
        if record.expires_at <= now {
            inner.sessions.remove(token);
            return None;
        }
        Some(SessionInfo {
            expires_at: record.expires_at,
            csrf_token: record.csrf_token.clone(),
            origin: record.origin,
        })
    }

    pub fn remove(&self, token: &str) -> bool {
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner.sessions.remove(token).is_some()
    }

    pub fn login_allowed(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner
            .login_windows
            .retain(|_, window| now.duration_since(window.started) <= LOGIN_WINDOW);
        match inner.login_windows.get(key) {
            Some(window) => window.failures < LOGIN_MAX_FAILURES,
            None => true,
        }
    }

    pub fn record_login_failure(&self, key: &str) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("session store poisoned");
        let window = inner
            .login_windows
            .entry(key.to_string())
            .or_insert(LoginWindowState {
                started: now,
                failures: 0,
            });
        if now.duration_since(window.started) > LOGIN_WINDOW {
            window.started = now;
            window.failures = 0;
        }
        window.failures = window.failures.saturating_add(1);
    }

    pub fn record_login_success(&self, key: &str) {
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner.login_windows.remove(key);
    }

    pub fn cookie_header(&self, token: &str) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!(
            "{SESSION_COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
            self.ttl.as_secs()
        )
    }

    pub fn clear_cookie_header(&self) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}")
    }

    pub fn csrf_matches(&self, expected: &str, provided: &str) -> bool {
        constant_time_eq(expected.as_bytes(), provided.as_bytes())
    }

    pub fn login_key(headers: &HeaderMap) -> String {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown")
            .to_string()
    }
}

pub fn session_token_from_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|part| {
                let part = part.trim();
                part.strip_prefix(SESSION_COOKIE_NAME)
                    .and_then(|rest| rest.strip_prefix('='))
                    .map(str::trim)
                    .filter(|token| is_valid_session_token(token))
                    .map(str::to_string)
            })
        })
}

pub fn session_token_from_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| is_valid_session_token(token))
        .map(str::to_string)
}

pub fn csrf_token_from_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn is_valid_session_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip_and_logout() {
        let store = SessionStore::default();
        let created = store.create();
        assert!(is_valid_session_token(&created.token));
        let info = store
            .validate(&created.token)
            .expect("session should validate");
        assert_eq!(info.csrf_token, created.info.csrf_token);
        assert!(store.remove(&created.token));
        assert!(store.validate(&created.token).is_none());
    }

    #[test]
    fn expired_session_is_rejected() {
        let store = SessionStore::new(Duration::from_millis(10), false);
        let created = store.create();
        std::thread::sleep(Duration::from_millis(25));
        assert!(store.validate(&created.token).is_none());
    }

    #[test]
    fn malformed_session_token_is_rejected() {
        let store = SessionStore::default();
        assert!(store.validate("not-a-session").is_none());
        assert!(store.validate("").is_none());
    }

    #[test]
    fn login_rate_limit_locks_after_five_failures() {
        let store = SessionStore::default();
        for _ in 0..LOGIN_MAX_FAILURES {
            assert!(store.login_allowed("client"));
            store.record_login_failure("client");
        }
        assert!(!store.login_allowed("client"));
        store.record_login_success("client");
        assert!(store.login_allowed("client"));
    }

    #[test]
    fn cookie_has_http_only_same_site_and_secure_flag_when_configured() {
        let store = SessionStore::new(Duration::from_secs(60), true);
        let created = store.create();
        let cookie = store.cookie_header(&created.token);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("Max-Age=60"));
    }

    // ── 会话表满时的驱逐策略（L3B：未认证流量不得挤掉手动写会话）──────────────

    /// 直接写入一条会话记录（用于确定性地铺满会话表、控制过期时间与来源）。
    fn seed_session(
        store: &SessionStore,
        token: &str,
        origin: SessionOrigin,
        expires_at: DateTime<Utc>,
    ) {
        let mut inner = store.inner.lock().expect("session store poisoned");
        inner.sessions.insert(
            token.to_string(),
            SessionRecord {
                expires_at,
                csrf_token: format!("csrf-{token}"),
                origin,
            },
        );
    }

    fn random_token() -> String {
        format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
    }

    fn session_count(store: &SessionStore) -> usize {
        store
            .inner
            .lock()
            .expect("session store poisoned")
            .sessions
            .len()
    }

    /// 满表时插入 TrustedPeer 会话：只驱逐「最早过期的 TrustedPeer」，手动会话全部保留。
    #[test]
    fn eviction_drops_oldest_trusted_peer_and_keeps_manual_sessions() {
        let store = SessionStore::new(Duration::from_secs(600), false);
        let now = Utc::now();
        let manual = random_token();
        let trusted_old = random_token();
        let trusted_new = random_token();

        seed_session(
            &store,
            &manual,
            SessionOrigin::Manual,
            now + chrono::Duration::seconds(300),
        );
        seed_session(
            &store,
            &trusted_old,
            SessionOrigin::TrustedPeer,
            now + chrono::Duration::seconds(100),
        );
        seed_session(
            &store,
            &trusted_new,
            SessionOrigin::TrustedPeer,
            now + chrono::Duration::seconds(500),
        );
        for i in 0..(MAX_SESSIONS - 3) {
            seed_session(
                &store,
                &format!("{:064x}", 0x1000 + i),
                SessionOrigin::Manual,
                now + chrono::Duration::seconds(600),
            );
        }
        assert_eq!(session_count(&store), MAX_SESSIONS);

        let (new_token, _) = store.create_with_origin(SessionOrigin::TrustedPeer);

        assert_eq!(session_count(&store), MAX_SESSIONS, "驱逐后仍受上限约束");
        assert!(
            store.validate(&manual).is_some(),
            "手动会话必须被保护，不得被未认证流量驱逐"
        );
        assert!(
            store.validate(&trusted_old).is_none(),
            "最旧过期的 TrustedPeer 会话必须被驱逐"
        );
        assert!(
            store.validate(&trusted_new).is_some(),
            "较新的 TrustedPeer 会话应保留"
        );
        assert!(store.validate(&new_token).is_some(), "新签发会话必须可用");
    }

    /// 表内全为手动会话时回退「驱逐最早过期者」，不让会话表无界增长。
    #[test]
    fn eviction_falls_back_to_oldest_session_when_no_trusted_peer_exists() {
        let store = SessionStore::new(Duration::from_secs(600), false);
        let now = Utc::now();
        let oldest = random_token();
        seed_session(
            &store,
            &oldest,
            SessionOrigin::Manual,
            now + chrono::Duration::seconds(10),
        );
        for i in 1..MAX_SESSIONS {
            seed_session(
                &store,
                &format!("{:064x}", 0x2000 + i),
                SessionOrigin::Manual,
                now + chrono::Duration::seconds(100 + i as i64),
            );
        }
        assert_eq!(session_count(&store), MAX_SESSIONS);

        let (new_token, _) = store.create_with_origin(SessionOrigin::TrustedPeer);

        assert_eq!(
            session_count(&store),
            MAX_SESSIONS,
            "回退驱逐后仍受上限约束"
        );
        assert!(
            store.validate(&oldest).is_none(),
            "无 TrustedPeer 可驱逐时必须回退驱逐最早过期会话"
        );
        assert!(store.validate(&new_token).is_some(), "新签发会话必须可用");
    }
}
