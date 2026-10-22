//! 契约级黑盒测试夹具：以真实进程 + 真实 HTTP 驱动 `pony-sentry-server`。
//!
//! 契约真源：`.dev-team/contracts-pony-sentry-migration.md`（冻结 v1.1）。
//! 断言依据：R2（ingest v1 字段）、R3（stacktrace 双形态）、R4（201/200 duplicate）、
//! R6（指纹 v3 环境作用域）、R10（鉴权矩阵）、R11.2（/healthz db 探活）。
//!
//! 设计约束：
//! - `crates/**` 属实施者写域，本夹具零 crate 内部类型依赖，只依赖进程级契约面；
//! - 进程级接缝（Lead 冻结口径）：`DATABASE_URL` / `PORT` / `CLIENT_TOKEN` / `ADMIN_TOKEN`；
//!   扩展（`.dev-team/contract-tailnet-trust.md` ver 2）：`CONSOLE_TAILNET_TRUST` /
//!   `CONSOLE_TAILNET_CIDRS` / `CONSOLE_TRUSTED_HOSTS`。
//! - 三个 tailnet 接缝键在 spawn 前一律 `env_remove`，防止开发者 shell 导出的同名变量
//!   污染用例（假绿/假红）。用例只能通过 `extra_env` 显式注入。
//! - 每个用例独立 sqlite 文件库 + 独立端口 + 独立临时目录，互不污染，绝不复用生产库。

// 本模块被多个 `[[test]]` target 以 `#[path]` 共享，任何单个 target 都只用到其中一部分接缝；
// 逐 target 的 `dead_code` 告警属噪声，统一抑制（不影响任何断言强度）。
#![allow(dead_code)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// 契约 R10 的测试用虚拟凭证（严禁真实 token 入库）。
pub const TEST_CLIENT_TOKEN: &str = "contract-test-client-token";
pub const TEST_ADMIN_TOKEN: &str = "contract-test-admin-token";

// ── 响应封装 ────────────────────────────────────────────────────────────────

/// 单次 HTTP 调用的原始收据：状态码 + 原文 + JSON 视图。
pub struct Resp {
    pub status: u16,
    pub raw: String,
    pub json: serde_json::Value,
    pub headers: reqwest::header::HeaderMap,
}

impl Resp {
    pub fn field(&self, key: &str) -> &serde_json::Value {
        &self.json[key]
    }

    pub fn str_field(&self, key: &str) -> String {
        self.json[key]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "响应缺少字符串字段 `{key}`：status={} body={}",
                    self.status, self.raw
                )
            })
            .to_string()
    }

    pub fn i64_field(&self, key: &str) -> i64 {
        self.json[key].as_i64().unwrap_or_else(|| {
            panic!(
                "响应缺少整数字段 `{key}`：status={} body={}",
                self.status, self.raw
            )
        })
    }

    pub fn describe(&self) -> String {
        format!("status={} body={}", self.status, self.raw)
    }
}

// ── 进程级服务夹具 ──────────────────────────────────────────────────────────

pub struct Server {
    child: Child,
    pub base_url: String,
    workdir: PathBuf,
    /// `Some` 时子进程 stdout 被重定向到该文件（用于机械断言启动 WARN / 审计日志脱敏）。
    log_path: Option<PathBuf>,
}

/// 契约 R-T4/R-T5b/R-T8b 的进程级 env 接缝：spawn 前必须从继承环境里剔除，
/// 否则开发机导出的同名变量会让「默认关闭」用例假绿。
const TAILNET_ENV_KEYS: [&str; 3] = [
    "CONSOLE_TAILNET_TRUST",
    "CONSOLE_TAILNET_CIDRS",
    "CONSOLE_TRUSTED_HOSTS",
];

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .unwrap_or_else(|| {
            panic!(
                "契约测试包必须位于仓库 tests/ 目录：{}",
                manifest_dir.display()
            )
        })
        .to_path_buf()
}

/// 确保被测二进制为当前源码的最新产物（每个测试进程首次调用执行一次）。
fn server_binary() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(explicit) = std::env::var("PONY_SENTRY_BIN") {
            let path = PathBuf::from(explicit);
            assert!(
                path.is_file(),
                "PONY_SENTRY_BIN 指向的二进制不存在：{}",
                path.display()
            );
            return path;
        }
        let root = repo_root();
        let status = Command::new("cargo")
            .args(["build", "--offline", "-p", "pony-sentry-server"])
            .current_dir(&root)
            .status()
            .expect("无法执行 cargo build 构建 pony-sentry-server");
        assert!(
            status.success(),
            "构建 pony-sentry-server 失败（cargo build 退出码非 0）"
        );
        let candidate = root.join("target/debug/pony-sentry-server");
        assert!(
            candidate.is_file(),
            "构建后未找到二进制：{}",
            candidate.display()
        );
        candidate
    })
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("无法分配空闲端口");
    let port = listener.local_addr().expect("无法读取本地监听地址").port();
    drop(listener);
    port
}

fn unique_workdir() -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "pony-sentry-contract-{}-{}-{}",
        std::process::id(),
        seq,
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("无法创建测试工作目录");
    dir
}

pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .no_proxy()
        .build()
        .expect("无法构建 HTTP 客户端")
}

impl Server {
    /// 默认实例：CLIENT_TOKEN 与 ADMIN_TOKEN 均已配置（R10 鉴权矩阵前置条件）。
    pub fn start() -> Self {
        Self::start_with(&[
            ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
            ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ])
    }

    /// 定制 env 实例（额外 env 覆盖默认值）。
    pub fn start_with(extra_env: &[(&str, &str)]) -> Self {
        Self::spawn(extra_env, false)
    }

    /// 定制 env 实例 + 捕获子进程 stdout 到临时文件（供启动 WARN / 日志脱敏断言读取）。
    ///
    /// stdout 用文件重定向而非管道：避免长时间运行的服务写满管道缓冲区阻塞（防死锁）。
    pub fn start_with_log_capture(extra_env: &[(&str, &str)]) -> Self {
        Self::spawn(extra_env, true)
    }

    fn spawn(extra_env: &[(&str, &str)], capture_logs: bool) -> Self {
        let binary = server_binary().clone();
        let port = free_port();
        let workdir = unique_workdir();
        let db_path = workdir.join("ponysentry.db");
        let base_url = format!("http://127.0.0.1:{port}");
        let log_path = workdir.join("server-stdout.log");

        let mut cmd = Command::new(&binary);
        cmd.current_dir(&workdir)
            .env("PORT", port.to_string())
            .env(
                "DATABASE_URL",
                format!("sqlite:{}?mode=rwc", db_path.display()),
            )
            .env("RUST_LOG", "info")
            .env_remove("PONY_REINDEX_FINGERPRINTS")
            .stderr(Stdio::piped());
        // 契约接缝：先无条件剔除继承来的 tailnet 配置，再由 extra_env 显式注入。
        for key in TAILNET_ENV_KEYS {
            cmd.env_remove(key);
        }
        if capture_logs {
            let file = std::fs::File::create(&log_path).expect("无法创建子进程 stdout 日志文件");
            cmd.stdout(Stdio::from(file));
        } else {
            cmd.stdout(Stdio::null());
        }
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let child = cmd
            .spawn()
            .unwrap_or_else(|err| panic!("无法启动 {}：{err}", binary.display()));
        let mut server = Self {
            child,
            base_url,
            workdir,
            log_path: capture_logs.then_some(log_path),
        };
        server.wait_ready();
        server
    }

    /// 读取子进程至今写入的 stdout（未启用捕获时为空串）。
    pub fn captured_log(&self) -> String {
        match self.log_path.as_ref() {
            Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
            None => String::new(),
        }
    }

    fn wait_ready(&mut self) {
        let client = http_client();
        let deadline = Instant::now() + READY_TIMEOUT;
        let health_url = format!("{}/healthz", self.base_url);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                let stderr = self.drain_stderr();
                panic!("pony-sentry-server 启动即退出（{status}）：\n{stderr}");
            }
            if let Ok(resp) = client.get(&health_url).send() {
                if resp.status().is_success() {
                    return;
                }
            }
            if Instant::now() >= deadline {
                let stderr = self.drain_stderr();
                panic!("pony-sentry-server 未在 {READY_TIMEOUT:?} 内就绪：{health_url}\n{stderr}");
            }
            std::thread::sleep(READY_POLL_INTERVAL);
        }
    }

    fn drain_stderr(&mut self) -> String {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(stderr) = self.child.stderr.as_mut() {
            let _ = stderr.read_to_string(&mut buf);
        }
        buf
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// `host:port` 形态的 TCP 地址：供必须逐字节控制请求头（如伪造 `Host`）的原始 HTTP 用例。
    pub fn tcp_addr(&self) -> String {
        self.base_url
            .trim_start_matches("http://")
            .to_string()
    }

    // ── 契约动词（显式区分 token 形态，避免「双头同时发送」掩盖鉴权语义）──────

    pub fn post_ingest_client(&self, body: &serde_json::Value) -> Resp {
        self.send(
            reqwest::Method::POST,
            "/api/v1/ingest",
            &[("x-client-token", TEST_CLIENT_TOKEN)],
            Some(body),
        )
    }

    pub fn post_ingest_anon(&self, body: &serde_json::Value) -> Resp {
        self.send(reqwest::Method::POST, "/api/v1/ingest", &[], Some(body))
    }

    pub fn get_anon(&self, path: &str) -> Resp {
        self.send(reqwest::Method::GET, path, &[], None)
    }

    pub fn get_admin(&self, path: &str) -> Resp {
        self.send(
            reqwest::Method::GET,
            path,
            &[("x-admin-token", TEST_ADMIN_TOKEN)],
            None,
        )
    }

    pub fn get_client(&self, path: &str) -> Resp {
        self.send(
            reqwest::Method::GET,
            path,
            &[("x-client-token", TEST_CLIENT_TOKEN)],
            None,
        )
    }

    pub fn patch_anon(&self, path: &str, body: &serde_json::Value) -> Resp {
        self.send(reqwest::Method::PATCH, path, &[], Some(body))
    }

    pub fn patch_admin(&self, path: &str, body: &serde_json::Value) -> Resp {
        self.send(
            reqwest::Method::PATCH,
            path,
            &[("x-admin-token", TEST_ADMIN_TOKEN)],
            Some(body),
        )
    }

    pub fn healthz(&self) -> Resp {
        self.send(reqwest::Method::GET, "/healthz", &[], None)
    }

    pub fn login_admin(&self) -> Resp {
        self.send(
            reqwest::Method::POST,
            "/api/v1/auth/session",
            &[],
            Some(&serde_json::json!({"token": TEST_ADMIN_TOKEN})),
        )
    }

    pub fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Resp {
        let client = http_client();
        let mut request = client.request(method, self.url(path));
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .unwrap_or_else(|err| panic!("请求 {path} 失败：{err}"));
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let raw = response.text().unwrap_or_default();
        let json = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        Resp { status, raw, json, headers }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
}

// ── 报文构造（严格按 R2 字段清单）───────────────────────────────────────────

/// R2 规定的 ingest v1 扩展字段全集（缺一不可）。
pub fn full_v1_ingest_payload(
    event_id: &str,
    environment: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "platform": "python",
        "release": "0.24.35",
        "environment": environment,
        "message": message,
        "event_id": event_id,
        "level": "error",
        "namespace": environment,
        "app": "api_v2",
        "component": "issue_reporting",
        "service_name": "langgraph",
        "error_type": "ValidationError",
        "source_kind": "agent",
        "grafana_url": "https://grafana.ponyjob.top/d/contract/issue-reporting",
        "loki_query": "{app=\"api_v2\"}",
        "timestamp": "2026-10-21T07:00:00Z",
        "exception": {
            "error_type": "ValidationError",
            "value": message,
            "stacktrace": [
                {
                    "filename": "api_v2/src/modules/issue_reporting/worker.py",
                    "function": "build_pony_sentry_payload",
                    "lineno": 88,
                    "colno": 5,
                    "in_app": true
                }
            ]
        },
        "tags": {"project": "job_copilot", "alertname": "contract-probe"},
        "extra": {"project_path": "api_v2/src/modules/issue_reporting/worker.py"}
    })
}
