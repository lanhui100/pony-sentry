//! L2-AT 对抗破坏测试（adversarial / destructive）：Tailnet 可信对端免 admin token。
//!
//! 契约真源：`.dev-team/contract-tailnet-trust.md`（FROZEN ver 2，R-T1..R-T13，只读）。
//! 本文件只做**黑盒破坏**，不依赖 `crates/**` 内部类型，也不复用/修改既有夹具：
//! - 真实进程：`target/debug/pony-sentry-server`（缺失时 `cargo build --offline -p pony-sentry-server`）；
//! - 真实 TCP：手写 HTTP/1.1 字节流，逐字节控制 `Host` / 代理头 / 畸形头（reqwest 会改写信头，故不用）；
//! - 独立端口 + 独立临时 sqlite 库，`Drop` 回收进程与目录（谁创建谁清理）；
//! - 三个 tailnet env 键在 spawn 前一律 `env_remove`，只允许本文件显式注入（防开发机污染造成假绿/假红）。
//!
//! 三类对抗面：
//! 1. 代理头对抗：信任门不得被 `X-Forwarded-For` / `X-Real-IP` 变体绕过；`X-Forwarded-Host` 不属冻结代理门，
//!    必须既不授予也不吊销信任；
//! 2. 畸形/超大输入：超长 Host、空 Host、伪造 Host、10KB 垃圾头 → 不得 panic/500，进程存活；
//! 3. 并发确定性竞态：`std::thread` + `Barrier`（无 sleep）16 线程 × 5 轮混合可信读 + 会话签发，
//!    断言零 5xx / 零连接重置 / 进程存活，且并发后会话仍能正确读。
//!
//! 判定口径：本文件是**对抗测试**，红相（实现未接线）是预期结果；任何 FAIL 都必须能归因到
//! 某个具体 HTTP 事实（状态码 / body / 进程存活），绝不使用 `should_panic` 或吞异常放行。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, OnceLock};
use std::time::{Duration, Instant};

/// 对抗用虚拟凭证（严禁真实 token）。
const TEST_ADMIN_TOKEN: &str = "adversarial-admin-token";
const TEST_CLIENT_TOKEN: &str = "adversarial-client-token";

/// 可信接缝（契约 R-T1 前置 env）：开启信任 + 本机 CIDR + Host 白名单。
const TRUST_ENV: [(&str, &str); 3] = [
    ("CONSOLE_TAILNET_TRUST", "1"),
    ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
    ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
];

/// 必须在 spawn 前从继承环境剔除的 tailnet 键：否则开发 shell 导出的同名变量会让
/// 「可信/不可信」用例被环境静默改写。
const TAILNET_ENV_KEYS: [&str; 3] = [
    "CONSOLE_TAILNET_TRUST",
    "CONSOLE_TAILNET_CIDRS",
    "CONSOLE_TRUSTED_HOSTS",
];

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(20);
const IO_TIMEOUT: Duration = Duration::from_secs(10);

// ── 逐用例台账：失败聚合，避免首个 FAIL 掩盖其余对抗面 ───────────────────────

#[derive(Default)]
struct Ledger {
    entries: Vec<(String, bool, String)>,
}

impl Ledger {
    fn record(&mut self, case: &str, pass: bool, detail: String) {
        println!(
            "[{}] {case} :: {detail}",
            if pass { "PASS" } else { "FAIL" }
        );
        self.entries.push((case.to_string(), pass, detail));
    }

    fn finish(&self, class: &str) {
        let failed: Vec<&(String, bool, String)> =
            self.entries.iter().filter(|(_, pass, _)| !pass).collect();
        if !failed.is_empty() {
            let lines: Vec<String> = failed
                .iter()
                .map(|(case, _, detail)| format!("  - {case}: {detail}"))
                .collect();
            panic!(
                "[{class}] {}/{} 个对抗用例失败：\n{}",
                failed.len(),
                self.entries.len(),
                lines.join("\n")
            );
        }
        println!("[{class}] 全部 {} 个对抗用例 PASS", self.entries.len());
    }
}

// ── HTTP/1.1 原始请求 / 响应 ────────────────────────────────────────────────

#[derive(Clone)]
struct RawRequest {
    path: String,
    host: Option<String>,
    extra_headers: Vec<String>,
}

impl RawRequest {
    fn get(path: &str) -> Self {
        Self {
            path: path.to_string(),
            host: Some("127.0.0.1".to_string()),
            extra_headers: Vec::new(),
        }
    }

    fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    fn extra(mut self, header_line: impl Into<String>) -> Self {
        self.extra_headers.push(header_line.into());
        self
    }

    /// 按字节拼装请求：`Connection: close` 让服务端在响应后关闭连接，便于读到 EOF。
    fn bytes(&self) -> Vec<u8> {
        let mut text = format!("GET {} HTTP/1.1\r\n", self.path);
        if let Some(host) = &self.host {
            text.push_str(&format!("Host: {host}\r\n"));
        }
        text.push_str("Connection: close\r\n");
        for header in &self.extra_headers {
            text.push_str(header);
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        text.into_bytes()
    }
}

#[derive(Clone)]
struct RawResponse {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl RawResponse {
    fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }

    fn describe(&self) -> String {
        let body = if self.body.len() > 240 {
            format!("{}…（{} 字节）", &self.body[..240], self.body.len())
        } else {
            self.body.clone()
        };
        format!("status={} reason={} body={body}", self.status, self.reason)
    }
}

fn dechunk(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    loop {
        let Some(line_end) = rest.find("\r\n") else {
            break;
        };
        let size_hex = rest[..line_end].split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_hex, 16) else {
            break;
        };
        rest = &rest[line_end + 2..];
        if size == 0 {
            break;
        }
        if rest.len() < size {
            out.push_str(rest);
            break;
        }
        out.push_str(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or("");
    }
    out
}

fn parse_response(buf: &[u8]) -> RawResponse {
    let text = String::from_utf8_lossy(buf).to_string();
    let (head, raw_body) = text
        .find("\r\n\r\n")
        .map(|idx| (&text[..idx], &text[idx + 4..]))
        .unwrap_or((text.as_str(), ""));
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next().unwrap_or("");
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let reason = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    let chunked = headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
    });
    let body = if chunked {
        dechunk(raw_body)
    } else {
        raw_body.to_string()
    };
    RawResponse {
        status,
        reason,
        headers,
        body,
    }
}

/// 发送原始字节并读回完整响应（`Connection: close` + EOF）。
///
/// `Err` 只表示**连接层失败**（拒连 / 写失败 / 重置）：对抗用例把它如实计为
/// 「连接重置」，绝不把空响应伪造成 4xx。
fn send_raw(port: u16, request: &[u8]) -> io::Result<RawResponse> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(request)?;
    stream.flush()?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            // 读超时：连接未按 `Connection: close` 关闭，已读字节照常解析（不伪造状态）。
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(err) => {
                if buf.is_empty() {
                    return Err(err);
                }
                break;
            }
        }
    }
    Ok(parse_response(&buf))
}

// ── 真实进程夹具（本文件私有；不修改 tests/harness.rs）────────────────────

struct Server {
    child: Child,
    port: u16,
    dir: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .unwrap_or_else(|| {
            panic!(
                "测试包必须位于仓库 crates/server 下：{}",
                manifest_dir.display()
            )
        })
        .to_path_buf()
}

/// 被测二进制：优先 Cargo 为集成测试注入的 `CARGO_BIN_EXE_*`（保证随当前源码构建）；
/// 否则按契约回退 `target/debug/pony-sentry-server`，不存在时先 `cargo build --offline`。
fn server_binary() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(explicit) = option_env!("CARGO_BIN_EXE_pony-sentry-server") {
            let path = PathBuf::from(explicit);
            if path.is_file() {
                return path;
            }
        }
        let root = repo_root();
        let candidate = root.join("target/debug/pony-sentry-server");
        if candidate.is_file() {
            return candidate;
        }
        let status = Command::new("cargo")
            .args(["build", "--offline", "-p", "pony-sentry-server"])
            .current_dir(&root)
            .status()
            .expect("无法执行 cargo build 构建 pony-sentry-server");
        assert!(
            status.success(),
            "构建 pony-sentry-server 失败（cargo build 退出码非 0）"
        );
        assert!(
            candidate.is_file(),
            "构建后仍未找到二进制：{}",
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
        "pony-sentry-adversarial-{}-{}-{}",
        std::process::id(),
        seq,
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("无法创建测试工作目录");
    dir
}

impl Server {
    fn start(extra_env: &[(&str, &str)]) -> Self {
        let binary = server_binary().clone();
        let port = free_port();
        let dir = unique_workdir();
        let db_path = dir.join("ponysentry.db");
        let stdout_path = dir.join("stdout.log");
        let stderr_path = dir.join("stderr.log");

        let mut cmd = Command::new(&binary);
        cmd.current_dir(&dir)
            .env("PORT", port.to_string())
            .env(
                "DATABASE_URL",
                format!("sqlite:{}?mode=rwc", db_path.display()),
            )
            .env("ADMIN_TOKEN", TEST_ADMIN_TOKEN)
            .env("CLIENT_TOKEN", TEST_CLIENT_TOKEN)
            .env("RUST_LOG", "info")
            .env_remove("PONY_REINDEX_FINGERPRINTS")
            .env_remove("WEBHOOK_URL")
            // 日志重定向到文件而非管道：长时间运行不会写满管道阻塞子进程。
            .stdout(Stdio::from(
                File::create(&stdout_path).expect("无法创建 stdout 日志文件"),
            ))
            .stderr(Stdio::from(
                File::create(&stderr_path).expect("无法创建 stderr 日志文件"),
            ));
        for key in TAILNET_ENV_KEYS {
            cmd.env_remove(key);
        }
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let child = cmd
            .spawn()
            .unwrap_or_else(|err| panic!("无法启动 {}：{err}", binary.display()));
        let mut server = Self {
            child,
            port,
            dir,
            stdout_path,
            stderr_path,
        };
        server.wait_ready();
        server
    }

    fn stderr_text(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    fn stdout_text(&self) -> String {
        std::fs::read_to_string(&self.stdout_path).unwrap_or_default()
    }

    fn combined_logs(&self) -> String {
        format!("{}\n{}", self.stdout_text(), self.stderr_text())
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "pony-sentry-server 启动即退出（{status}）：\nstdout:\n{}\nstderr:\n{}",
                    self.stdout_text(),
                    self.stderr_text()
                );
            }
            if let Ok(resp) = send_raw(self.port, &RawRequest::get("/healthz").bytes()) {
                if resp.status == 200 {
                    return;
                }
            }
            if Instant::now() >= deadline {
                panic!(
                    "pony-sentry-server 未在 {READY_TIMEOUT:?} 内就绪（port={}）：\nstdout:\n{}\nstderr:\n{}",
                    self.port,
                    self.stdout_text(),
                    self.stderr_text()
                );
            }
            std::thread::sleep(READY_POLL_INTERVAL);
        }
    }

    fn send(&self, request: &RawRequest) -> io::Result<RawResponse> {
        send_raw(self.port, &request.bytes())
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn healthz(&self) -> Option<u16> {
        self.send(&RawRequest::get("/healthz"))
            .ok()
            .map(|resp| resp.status)
    }

    fn assert_alive(&mut self, ledger: &mut Ledger, clause: &str) {
        match self.child.try_wait() {
            Ok(None) => ledger.record(
                &format!("{clause}.process_alive"),
                true,
                "子进程仍在运行".to_string(),
            ),
            Ok(Some(status)) => ledger.record(
                &format!("{clause}.process_alive"),
                false,
                format!(
                    "子进程已退出（{status}）：\nstderr:\n{}",
                    self.stderr_text()
                ),
            ),
            Err(err) => ledger.record(
                &format!("{clause}.process_alive"),
                false,
                format!("try_wait 失败：{err}"),
            ),
        }
    }

    fn assert_no_panic_in_logs(&self, ledger: &mut Ledger, clause: &str) {
        let logs = self.combined_logs();
        let hit = logs.contains("panicked at");
        ledger.record(
            &format!("{clause}.no_panic_in_logs"),
            !hit,
            if hit {
                format!("日志出现 panic 痕迹：\n{}", tail(&logs, 2000))
            } else {
                "日志无 panicked at 痕迹".to_string()
            },
        );
    }
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        text.to_string()
    } else {
        format!("…{}", &text[text.len() - limit..])
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ── 断言工具 ────────────────────────────────────────────────────────────────

fn status_in(resp: &RawResponse, expected: &[u16]) -> bool {
    expected.contains(&resp.status)
}

/// 401 且 body 为契约冻结的 `{"error":"unauthorized"}`（R-T5/R-T5b/R-T5c 统一口径）。
fn unauthorized_body(resp: &RawResponse) -> bool {
    resp.json()
        .and_then(|json| {
            json.get("error")
                .and_then(|v| v.as_str())
                .map(|s| s == "unauthorized")
        })
        .unwrap_or(false)
}

/// 从 `Set-Cookie` 头里取会话 token（`pony_sentry_session=<token>; ...`）。
fn session_token(resp: &RawResponse) -> Option<String> {
    let cookie = resp
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, value)| value.as_str())
        .find(|value| value.starts_with("pony_sentry_session="))?;
    let rest = cookie.strip_prefix("pony_sentry_session=")?;
    let token = rest.split(';').next()?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

// ── 1. 代理头对抗：信任门不可被头绕过 ──────────────────────────────────────

#[test]
fn adversarial_proxy_headers_never_bypass_trust_gate() {
    let mut server = Server::start(&TRUST_ENV);
    let mut ledger = Ledger::default();
    let host_with_port = format!("127.0.0.1:{}", server.port);

    // 控制组：信任接缝必须真实成立。没有这一组，后面的 401 无法归因于「代理头被拒」
    // （也可能是信任整体未生效），对抗结论不成立。
    let cases: Vec<(&str, RawRequest, &[u16], bool)> = vec![
        (
            "control.baseline_host_127_0_0_1",
            RawRequest::get("/api/v1/issues").host("127.0.0.1"),
            &[200],
            false,
        ),
        (
            "control.baseline_host_with_port_matches_ignoring_port",
            RawRequest::get("/api/v1/issues").host(host_with_port.clone()),
            &[200],
            false,
        ),
        (
            "xff.value_100_64_0_1",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-For: 100.64.0.1"),
            &[401],
            true,
        ),
        (
            "xff.empty_value",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-For: "),
            &[401],
            true,
        ),
        (
            "xff.duplicate_headers",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-For: 100.64.0.1")
                .extra("X-Forwarded-For: 127.0.0.1"),
            &[401],
            true,
        ),
        (
            "xff.uppercase_header_name",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-FORWARDED-FOR: 100.64.0.1"),
            &[401],
            true,
        ),
        (
            "xff.trusted_looking_value_is_still_rejected",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-For: 127.0.0.1"),
            &[401],
            true,
        ),
        (
            "x_real_ip.value_100_64_0_1",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Real-IP: 100.64.0.1"),
            &[401],
            true,
        ),
        (
            "xff_plus_x_real_ip",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-For: 100.64.0.1")
                .extra("X-Real-IP: 100.64.0.1"),
            &[401],
            true,
        ),
        // X-Forwarded-Host 不属冻结代理门（R-T5/R-T5b 仅 XFF 与 X-Real-IP；R-T11 同）：
        // 在 CIDR+Host 已命中的前提下，它既不得授予信任（无 XFF 时基线本就 Trusted），
        // 也不得吊销信任。若实现把它并入代理门，即为对冻结契约的过度拦截，按 FAIL 上报。
        (
            "x_forwarded_host.alone_does_not_revoke_trust",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-Host: evil.example"),
            &[200],
            false,
        ),
        // 同一头在 Host 不可信时也不能救命：仍是 401（不授予信任）。
        (
            "x_forwarded_host.with_untrusted_host",
            RawRequest::get("/api/v1/issues")
                .host("evil.example:3000")
                .extra("X-Forwarded-Host: 127.0.0.1"),
            &[401],
            true,
        ),
        // 与真正的代理门头并存时，代理门优先：401。
        (
            "x_forwarded_host.plus_xff",
            RawRequest::get("/api/v1/issues")
                .host(host_with_port.clone())
                .extra("X-Forwarded-Host: 127.0.0.1")
                .extra("X-Forwarded-For: 100.64.0.1"),
            &[401],
            true,
        ),
    ];

    for (case, request, expected, require_unauthorized) in cases {
        let detail = match server.send(&request) {
            Ok(resp) => {
                let body_ok = !require_unauthorized || unauthorized_body(&resp);
                let pass = status_in(&resp, expected) && body_ok;
                let mut note = format!("期望状态 {expected:?}，实际 {}", resp.describe());
                if require_unauthorized && !body_ok {
                    note.push_str("；body 不是 {\"error\":\"unauthorized\"}");
                }
                ledger.record(case, pass, note);
                continue;
            }
            Err(err) => format!("期望状态 {expected:?}，实际连接层失败：{err}"),
        };
        ledger.record(case, false, detail);
    }

    // 全部对抗结束后服务必须仍然健康（代理头不得触发进程级故障）。
    let health = server.healthz();
    ledger.record(
        "aftermath.healthz_still_200",
        health == Some(200),
        format!("/healthz 实际 {health:?}"),
    );
    server.assert_alive(&mut ledger, "aftermath");
    server.assert_no_panic_in_logs(&mut ledger, "aftermath");
    ledger.finish("proxy-headers");
}

// ── 2. 畸形 / 超大输入 ─────────────────────────────────────────────────────

#[test]
fn adversarial_malformed_hosts_and_oversized_inputs_keep_process_alive() {
    let mut server = Server::start(&TRUST_ENV);
    let mut ledger = Ledger::default();

    // 8KB Host：超过常见 8KB 头缓冲阈值；合法 HTTP 头语法但绝不在白名单。
    let long_host = "a".repeat(8192);
    // 10KB 垃圾头：与 Host 白名单无关，测试解析/上限路径不 panic。
    let junk_header = format!("X-Adversarial-Junk: {}", "j".repeat(10 * 1024));

    let cases: Vec<(&str, RawRequest, &[u16], &str)> = vec![
        (
            "host.8192_bytes",
            RawRequest::get("/api/v1/issues").host(long_host),
            &[400, 401, 431],
            "超长 Host 必须被 4xx 拒绝（允许 400/401/431），不得 2xx/5xx",
        ),
        (
            "host.empty_value",
            RawRequest::get("/api/v1/issues").host(""),
            &[400, 401, 431],
            "空 Host 必须被 4xx 拒绝（空白名单项不构成信任）",
        ),
        (
            "host.evil_example_3000_is_401",
            RawRequest::get("/api/v1/issues").host("evil.example:3000"),
            &[401],
            "伪造 Host 即使 CIDR 命中也必须 401（DNS rebinding 防线 R-T5c）",
        ),
        (
            "header.10kb_junk_no_5xx_no_crash",
            RawRequest::get("/api/v1/issues").extra(junk_header),
            &[200, 400, 401, 413, 431],
            "10KB 垃圾头：该头不属冻结代理门也不参与信任判定，可信前提下 200 合法；\
             只硬性要求非 5xx / 连接不被中断（4xx 亦允许）",
        ),
    ];

    for (case, request, expected, clause) in cases {
        match server.send(&request) {
            Ok(resp) => {
                let pass = status_in(&resp, expected);
                ledger.record(case, pass, format!("{clause}；实际 {}", resp.describe()));
            }
            Err(err) => ledger.record(
                case,
                false,
                format!("{clause}；实际连接层失败（拒连/重置）：{err}"),
            ),
        }
        // 每条畸形请求后立即验证进程存活：单条用例失败不得掩盖「是否崩溃」这一硬事实。
        let alive = server.alive();
        ledger.record(
            &format!("{case}.process_alive_after"),
            alive,
            if alive {
                "随后进程仍存活".to_string()
            } else {
                format!("进程在畸形请求后退出：\n{}", server.stderr_text())
            },
        );
    }

    let health = server.healthz();
    ledger.record(
        "aftermath.healthz_still_200",
        health == Some(200),
        format!("/healthz 实际 {health:?}（畸形输入后服务必须仍可用）"),
    );
    server.assert_alive(&mut ledger, "aftermath");
    server.assert_no_panic_in_logs(&mut ledger, "aftermath");
    ledger.finish("malformed-input");
}

// ── 3. 并发确定性竞态 ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MixedRead {
    /// 可信匿名读（R-T1：免 admin token 的只读放行）。
    TrustedIssues,
    /// 可信会话签发（R-T6：自动签发会话，验证并发下的会话表不破坏进程）。
    TrustedSession,
}

#[test]
fn adversarial_concurrent_trusted_reads_are_deterministic() {
    let mut server = Server::start(&TRUST_ENV);
    let mut ledger = Ledger::default();
    let port = server.port;
    let trusted_host = "127.0.0.1";

    // 前置：取得一条已知有效会话（用于并发后复验）。不可信/未实现时如实记录为 FAIL。
    let pre_session = match server.send(&RawRequest::get("/api/v1/auth/session").host(trusted_host))
    {
        Ok(resp) if resp.status == 200 => {
            let token = session_token(&resp);
            ledger.record(
                "setup.trusted_session_issued",
                token.is_some(),
                format!(
                    "期望 200 + Set-Cookie pony_sentry_session=…；实际 {}",
                    resp.describe()
                ),
            );
            token
        }
        Ok(resp) => {
            ledger.record(
                "setup.trusted_session_issued",
                false,
                format!("期望 200 + Set-Cookie；实际 {}", resp.describe()),
            );
            None
        }
        Err(err) => {
            ledger.record(
                "setup.trusted_session_issued",
                false,
                format!("期望 200；实际连接层失败：{err}"),
            );
            None
        }
    };

    // 确定性并发：Barrier(16) 同时放行，无 sleep、无计时窗口；每线程固定 5 轮，
    // 读类型由 (thread_id + iteration) 奇偶决定 —— 任意调度下请求集合完全一致。
    const THREADS: usize = 16;
    const ROUNDS: usize = 5;
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|thread_id| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut outcomes = Vec::with_capacity(ROUNDS);
                barrier.wait();
                for round in 0..ROUNDS {
                    let kind = if (thread_id + round) % 2 == 0 {
                        MixedRead::TrustedIssues
                    } else {
                        MixedRead::TrustedSession
                    };
                    let request = match kind {
                        MixedRead::TrustedIssues => RawRequest::get("/api/v1/issues"),
                        MixedRead::TrustedSession => RawRequest::get("/api/v1/auth/session"),
                    }
                    .host(trusted_host);
                    let result = send_raw(port, &request.bytes());
                    outcomes.push((thread_id, round, kind, result));
                }
                outcomes
            })
        })
        .collect();

    let mut outcomes = Vec::with_capacity(THREADS * ROUNDS);
    for handle in handles {
        outcomes.extend(handle.join().expect("并发线程自身 panic（测试代码缺陷）"));
    }

    // 判定口径（写死，非概率）：
    // - 零连接重置 / 零 5xx（不得出现 500/502/503 计数）；
    // - 全部 80 个响应状态 ∈ {200, 401}（未实现阶段全 401 也如实计入，不用重试掩盖）；
    // - 前台前置/后置会话断言承担「信任接缝真实成立」的硬门禁。
    let mut by_status: BTreeMap<u16, usize> = BTreeMap::new();
    let mut io_failures = Vec::new();
    for (thread_id, round, kind, result) in &outcomes {
        match result {
            Ok(resp) => *by_status.entry(resp.status).or_default() += 1,
            Err(err) => io_failures.push(format!(
                "thread={thread_id} round={round} kind={kind:?} err={err}"
            )),
        }
    }
    let total = outcomes.len();
    println!(
        "[INFO] concurrent mixed reads: total={total} by_status={by_status:?} io_failures={}",
        io_failures.len()
    );
    ledger.record(
        "flood.no_connection_reset",
        io_failures.is_empty(),
        format!(
            "连接重置/IO 失败 {} / {total}{}",
            io_failures.len(),
            if io_failures.is_empty() {
                String::new()
            } else {
                format!("：{}", io_failures.join(" | "))
            }
        ),
    );
    let has_5xx = by_status.keys().any(|status| *status >= 500);
    ledger.record("flood.no_5xx", !has_5xx, format!("状态分布 {by_status:?}"));
    let unexpected: Vec<u16> = by_status
        .keys()
        .copied()
        .filter(|status| !matches!(status, 200 | 401))
        .collect();
    ledger.record(
        "flood.statuses_within_200_or_401",
        unexpected.is_empty(),
        format!("状态分布 {by_status:?}；越界状态 {unexpected:?}"),
    );
    let expected_total = THREADS * ROUNDS;
    ledger.record(
        "flood.all_requests_answered",
        total == expected_total,
        format!("实收 {total} / 期望 {expected_total}"),
    );

    // 并发后进程存活且服务可用。
    let health = server.healthz();
    ledger.record(
        "aftermath.healthz_still_200",
        health == Some(200),
        format!("/healthz 实际 {health:?}"),
    );
    server.assert_alive(&mut ledger, "aftermath");
    server.assert_no_panic_in_logs(&mut ledger, "aftermath");

    // 并发后：新签发会话必须能读成功（若实现正确，这条与 flood 规模无关）。
    let post_session =
        match server.send(&RawRequest::get("/api/v1/auth/session").host(trusted_host)) {
            Ok(resp) if resp.status == 200 => session_token(&resp),
            _ => None,
        };
    match post_session.as_deref() {
        Some(token) => {
            let resp = server.send(
                &RawRequest::get("/api/v1/issues")
                    .host(trusted_host)
                    .extra(format!("Cookie: pony_sentry_session={token}")),
            );
            let (pass, detail) = match resp {
                Ok(resp) => (
                    resp.status == 200,
                    format!(
                        "并发后新签发会话读 /api/v1/issues 期望 200；实际 {}",
                        resp.describe()
                    ),
                ),
                Err(err) => (false, format!("并发后新会话读失败（IO）：{err}")),
            };
            ledger.record("after_flood.fresh_session_can_read", pass, detail);
        }
        None => ledger.record(
            "after_flood.fresh_session_can_read",
            false,
            "并发后无法重新签发可信会话（无法验证会话表正确性）".to_string(),
        ),
    }

    // 并发前取得的已知有效会话：并发 flood 后仍须可读（200）。
    //
    // 判定口径：本用例 flood 期间最多签发 40 个会话（80 个请求中奇偶各半）+ 前后各 1 个，
    // 峰值 ≤ 42 << `MAX_SESSIONS = 512`，**淘汰路径在本用例规模下不可达**；因此这里
    // 不接受 401 —— 401 只可能意味着并发下有效会话被误删/会话表损坏，按 FAIL 上报。
    // （若将来把 flood 规模扩到 >512 次签发，才允许把 401 归因于淘汰并在此放宽。）
    match pre_session.as_deref() {
        Some(token) => {
            let resp = server.send(
                &RawRequest::get("/api/v1/issues")
                    .host(trusted_host)
                    .extra(format!("Cookie: pony_sentry_session={token}")),
            );
            let (pass, detail) = match resp {
                Ok(resp) => (
                    resp.status == 200,
                    format!(
                        "并发前会话在 flood 后读 /api/v1/issues 期望 200；实际 {}",
                        resp.describe()
                    ),
                ),
                Err(err) => (false, format!("并发前会话在 flood 后读失败（IO）：{err}")),
            };
            ledger.record("after_flood.known_session_still_reads", pass, detail);
        }
        None => ledger.record(
            "after_flood.known_session_still_reads",
            false,
            "前置会话未取得，无法复验并发后的会话正确性".to_string(),
        ),
    }

    ledger.finish("concurrency");
}
