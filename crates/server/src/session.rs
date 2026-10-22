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

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub expires_at: DateTime<Utc>,
    pub csrf_token: String,
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

    pub fn create(&self) -> CreatedSession {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(self.ttl.as_secs() as i64);
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let csrf_token = Uuid::new_v4().simple().to_string();
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner.sessions.retain(|_, record| record.expires_at > now);
        if inner.sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = inner
                .sessions
                .iter()
                .min_by_key(|(_, record)| record.expires_at)
                .map(|(token, _)| token.clone())
            {
                inner.sessions.remove(&oldest);
            }
        }
        inner.sessions.insert(
            token.clone(),
            SessionRecord {
                expires_at,
                csrf_token: csrf_token.clone(),
            },
        );
        CreatedSession {
            token,
            info: SessionInfo {
                expires_at,
                csrf_token,
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
}
