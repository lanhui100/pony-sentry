//! Tailnet 可信对端免 admin token —— 契约级黑盒验收测试。
//!
//! **唯一真源**：`.dev-team/contract-tailnet-trust.md`（FROZEN ver 2，R-T1..R-T13）。
//! 本文件逐条把契约表的「机器可验证期望」落成真实进程 + 真实 HTTP 断言：
//! - 三条红线：① 安全默认关闭（R-T2/R-T4）② Host 白名单防 DNS rebinding（R-T5c/R-T5d/R-T13）
//!   ③ 信任不兑换写权限、CSRF 不得被绕过（R-T7）。
//! - 代理头门只拒不放（R-T5/R-T5b/R-T11），凭证优先不得反向降级（R-T12）。
//!
//! 设计约束（与 `harness.rs` 一致）：
//! - 只经进程级接缝（env + HTTP）驱动被测二进制，零 `crates/**` 内部类型依赖；
//! - 三个 tailnet env 键由 harness 在 spawn 前 `env_remove`，用例只能显式注入（防开发机污染）；
//! - 异步无关的确定性断言：不用 `sleep` 猜状态，服务就绪由 `/healthz` 轮询保证。
//!
//! 标注约定：`[REVIEW-ONLY]` 表示契约里靠人眼复核的措辞（WARN 文案、日志字段「只含」），
//! 测试只断言可机械判定的部分，绝不伪造断言。

#[path = "harness.rs"]
mod harness;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use harness::{Resp, Server, TEST_ADMIN_TOKEN, TEST_CLIENT_TOKEN};

/// 可信对端基线 env（R-T1）：开启信任 + 本机 CIDR + Host 白名单命中 `127.0.0.1`。
fn trusted_env() -> [(&'static str, &'static str); 5] {
    [
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
        ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
    ]
}

fn start_trusted() -> Server {
    Server::start_with(&trusted_env())
}

/// R-T2：信任默认关闭——`CONSOLE_TAILNET_TRUST` 未设置，其余条件全部命中。
fn trust_env_unset() -> [(&'static str, &'static str); 4] {
    [
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
        ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
    ]
}

/// R-T3：`CONSOLE_TAILNET_CIDRS` 未设置 → 默认 CGNAT/ULA 网段，不含 localhost。
fn trust_env_default_cidrs() -> [(&'static str, &'static str); 4] {
    [
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
    ]
}

// ── 断言工具 ────────────────────────────────────────────────────────────────

fn assert_status(resp: &Resp, expected: u16, clause: &str) {
    assert_eq!(
        resp.status,
        expected,
        "[{clause}] 期望 HTTP {expected}，实际 {}",
        resp.describe()
    );
}

/// 401 `{"error":"unauthorized"}` —— 契约表所有「反例即 FAIL」的统一口径。
fn assert_unauthorized(resp: &Resp, clause: &str) {
    assert_status(resp, 401, clause);
    assert_eq!(
        resp.field("error").as_str(),
        Some("unauthorized"),
        "[{clause}] 401 响应体必须是 {{\"error\":\"unauthorized\"}}：{}",
        resp.describe()
    );
}

fn assert_ok_json_without_error(resp: &Resp, clause: &str) {
    assert_status(resp, 200, clause);
    assert!(
        resp.json.is_array(),
        "[{clause}] 200 响应体必须是 JSON 数组：{}",
        resp.describe()
    );
    assert!(
        resp.field("error").is_null(),
        "[{clause}] 放行响应不得携带 error 字段：{}",
        resp.describe()
    );
}

/// 原始 HTTP/1.1 请求：用于必须逐字节控制 `Host` 头的用例（reqwest 可能改写信头）。
struct RawResp {
    status: u16,
    raw: String,
}

fn raw_request(server: &Server, request: &str) -> RawResp {
    let addr = server.tcp_addr();
    let mut stream = TcpStream::connect(&addr)
        .unwrap_or_else(|err| panic!("原始 HTTP 连接 {addr} 失败：{err}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("无法设置读超时");
    stream
        .write_all(request.as_bytes())
        .expect("原始 HTTP 写请求失败");
    stream.flush().expect("原始 HTTP flush 失败");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("原始 HTTP 读响应失败");
    let raw = String::from_utf8_lossy(&buf).to_string();
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("无法解析原始 HTTP 状态行：{raw}"));
    RawResp { status, raw }
}

fn assert_raw_unauthorized(resp: &RawResp, clause: &str) {
    assert_eq!(
        resp.status, 401,
        "[{clause}] 期望原始 HTTP 401，实际响应：{}",
        resp.raw
    );
    assert!(
        resp.raw.contains("\"error\":\"unauthorized\""),
        "[{clause}] 401 响应体必须含 error=unauthorized：{}",
        resp.raw
    );
}

// ── R-T1：可信对端 + 无凭证 GET 放行 ───────────────────────────────────────

#[test]
fn rt1_trusted_peer_readonly_get_without_credentials_is_allowed() {
    // Arrange：可信基线 env（CIDR 命中 127.0.0.1 + Host 白名单命中）
    let server = start_trusted();

    // Act：无 Authorization、无 Cookie、无任何自定义头
    let list = server.get_anon("/api/v1/issues");

    // Assert（R-T1）：200 + JSON 且无 error 字段
    assert_ok_json_without_error(&list, "R-T1 可信对端无凭证 GET /api/v1/issues");
}

// ── R-T2：安全默认关闭（红线①）────────────────────────────────────────────

#[test]
fn rt2_trust_disabled_when_enable_env_absent() {
    // Arrange：CIDR/Host 全部命中，但 CONSOLE_TAILNET_TRUST 未设置（harness 已 env_remove）
    let server = Server::start_with(&trust_env_unset());

    // Act
    let list = server.get_anon("/api/v1/issues");

    // Assert（R-T2）：未显式开启 ⇒ 走既有 R10 鉴权 ⇒ 401
    assert_unauthorized(&list, "R-T2 未设置 CONSOLE_TAILNET_TRUST 时必须 401");
}

// ── R-T3：CIDR 不命中（默认网段不含 localhost）─────────────────────────────

#[test]
fn rt3_default_cidrs_do_not_include_loopback() {
    // Arrange：开启信任 + Host 命中，但 CIDR 未设置 → 默认 100.64.0.0/10 + fd7a:115c:a1e0::/48
    let server = Server::start_with(&trust_env_default_cidrs());

    // Act
    let list = server.get_anon("/api/v1/issues");

    // Assert（R-T3）：对端 127.0.0.1 不在默认可信网段 ⇒ 401
    assert_unauthorized(&list, "R-T3 默认 CIDR 不命中 loopback 时必须 401");
}

// ── R-T4：kill switch 优先于 Host/CIDR 命中（红线①）───────────────────────

#[test]
fn rt4_kill_switch_values_disable_trust_even_on_match() {
    // Arrange / Act / Assert：逐个验证「关闭口径」——0 / false / off / 空串 / 任意其它值
    let variants: [(&str, &str); 5] = [
        ("0", "0"),
        ("false", "false"),
        ("off", "off"),
        ("空串", ""),
        ("任意其它值 yes", "yes"),
    ];
    for (label, value) in variants {
        let server = Server::start_with(&[
            ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
            ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
            ("CONSOLE_TAILNET_TRUST", value),
            ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
            ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
        ]);
        let list = server.get_anon("/api/v1/issues");
        assert_unauthorized(
            &list,
            &format!("R-T4 CONSOLE_TAILNET_TRUST={label} 时 kill switch 必须生效"),
        );
    }
}

// ── R-T5 / R-T5b：代理头门（只拒不放）──────────────────────────────────────

#[test]
fn rt5_x_forwarded_for_blocks_trust_even_when_cidr_and_host_match() {
    // Arrange：可信基线
    let server = start_trusted();

    // Act：真实对端命中，但伪造代理头
    let list = server.send(
        reqwest::Method::GET,
        "/api/v1/issues",
        &[("x-forwarded-for", "100.64.0.1")],
        None,
    );

    // Assert（R-T5）
    assert_unauthorized(&list, "R-T5 携带 X-Forwarded-For 时必须 401");
}

#[test]
fn rt5b_x_real_ip_blocks_trust_even_when_cidr_and_host_match() {
    // Arrange
    let server = start_trusted();

    // Act
    let list = server.send(
        reqwest::Method::GET,
        "/api/v1/issues",
        &[("x-real-ip", "100.64.0.1")],
        None,
    );

    // Assert（R-T5b）
    assert_unauthorized(&list, "R-T5b 携带 X-Real-IP 时必须 401");
}

// ── R-T11：代理头只拒不放，不得取首个 hop 当对端 ───────────────────────────

#[test]
fn rt11_proxy_header_never_grants_trust_via_first_hop_inference() {
    // Arrange：真实对端 127.0.0.1 不在 CIDR 内，XFF 首 hop 伪装成可信网段地址
    let server = Server::start_with(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "100.64.0.0/10"),
        ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
    ]);

    // Act
    let list = server.send(
        reqwest::Method::GET,
        "/api/v1/issues",
        &[("x-forwarded-for", "100.64.0.1, 127.0.0.1")],
        None,
    );

    // Assert（R-T11）：不得根据 XFF 推断真实对端 ⇒ 401
    assert_unauthorized(&list, "R-T11 代理头不得授予读权限（不取首 hop）");
}

// ── R-T5c：Host 不在白名单 → 401（红线②，DNS rebinding 防线）───────────────

#[test]
fn rt5c_host_not_in_allowlist_blocks_trust_with_raw_host_header() {
    // Arrange：可信基线；用原始 HTTP 逐字节控制 Host 头（reqwest 可能按 URL 重写 Host）
    let server = start_trusted();

    // Act
    let resp = raw_request(
        &server,
        "GET /api/v1/issues HTTP/1.1\r\nHost: evil.example:3000\r\nConnection: close\r\n\r\n",
    );

    // Assert（R-T5c）：CIDR 命中但 Host 不匹配 ⇒ 401
    assert_raw_unauthorized(&resp, "R-T5c Host: evil.example:3000 必须 401");
}

// ── R-T5d：空白名单 = 信任恒不生效（fail-closed，红线②）───────────────────

#[test]
fn rt5d_empty_host_allowlist_disables_trust_and_warns() {
    // Arrange / Act / Assert：两种「空」口径——未设置 + 空串，都必须 401
    // [REVIEW-ONLY] WARN 的确切文案由人眼复核；此处机械断言「stdout 日志出现 WARN 级别行」。
    let unset_server = Server::start_with_log_capture(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
    ]);
    let unset_list = unset_server.get_anon("/api/v1/issues");
    assert_unauthorized(&unset_list, "R-T5d CONSOLE_TRUSTED_HOSTS 未设置时信任必须恒不生效");
    assert_startup_warn(&unset_server, "R-T5d 未设置白名单必须启动 WARN");

    let empty_server = Server::start_with_log_capture(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
        ("CONSOLE_TRUSTED_HOSTS", ""),
    ]);
    let empty_list = empty_server.get_anon("/api/v1/issues");
    assert_unauthorized(&empty_list, "R-T5d CONSOLE_TRUSTED_HOSTS 为空串时信任必须恒不生效");
    assert_startup_warn(&empty_server, "R-T5d 空白名单必须启动 WARN");
}

/// 启动 WARN 的机械断言：stdout 已成功捕获（非空）且含 WARN 级别行。
fn assert_startup_warn(server: &Server, clause: &str) {
    let log = server.captured_log();
    assert!(
        !log.is_empty(),
        "[{clause}] 未捕获到服务端 stdout 日志：无法断言启动 WARN（harness 日志接缝失效）"
    );
    assert!(
        log.lines().any(|line| line.contains("WARN")),
        "[{clause}] 启动日志缺少 WARN 级别行：\n{log}"
    );
}

// ── R-T6：可信对端无 Cookie 自动签发会话 ───────────────────────────────────

#[test]
fn rt6_trusted_peer_gets_session_cookie_and_csrf_without_credentials() {
    // Arrange
    let server = start_trusted();

    // Act：无 Cookie 的 GET /api/v1/auth/session
    let session = server.get_anon("/api/v1/auth/session");

    // Assert（R-T6）：200 + Set-Cookie（HttpOnly，pony_sentry_session=）+ authenticated + csrf
    assert_status(&session, 200, "R-T6 可信对端无 Cookie GET /api/v1/auth/session");
    let set_cookie = session
        .headers
        .get("set-cookie")
        .unwrap_or_else(|| panic!("[R-T6] 可信会话响应缺 Set-Cookie：{}", session.describe()))
        .to_str()
        .expect("Set-Cookie 非 ASCII");
    assert!(
        set_cookie.contains("pony_sentry_session="),
        "[R-T6] Set-Cookie 必须含 pony_sentry_session=：{set_cookie}"
    );
    assert!(
        set_cookie.to_ascii_lowercase().contains("httponly"),
        "[R-T6] 会话 Cookie 必须是 HttpOnly：{set_cookie}"
    );
    assert_eq!(
        session.field("authenticated").as_bool(),
        Some(true),
        "[R-T6] body 必须 authenticated=true：{}",
        session.describe()
    );
    let csrf = session.str_field("csrf_token");
    assert!(
        !csrf.trim().is_empty(),
        "[R-T6] csrf_token 不得为空：{}",
        session.describe()
    );
}

#[test]
fn rt6b_untrusted_peer_never_gets_session() {
    // Arrange：开启信任但 CIDR 不命中（默认网段）
    let server = Server::start_with(&trust_env_default_cidrs());

    // Act
    let session = server.get_anon("/api/v1/auth/session");

    // Assert（R-T6b）：不可信来源不得被签发会话
    assert_unauthorized(&session, "R-T6b 不可信来源 GET /api/v1/auth/session 必须 401");
}

// ── R-T7：信任 ≠ 跳过 CSRF（红线③）────────────────────────────────────────

#[test]
fn rt7_trust_never_bypasses_csrf_on_mutation() {
    // Arrange：可信对端先取会话（R-T6 路径），再造一条真实 issue 作为 PATCH 目标
    let server = start_trusted();
    let created = server.post_ingest_client(&harness::full_v1_ingest_payload(
        "evt-contract-tailnet-rt7-0001",
        "preprod",
        "PermissionError: tailnet trust csrf",
    ));
    assert_status(&created, 201, "R-T7 前置 ingest（CLIENT_TOKEN）必须 201");
    let issue_id = created.str_field("issue_id");
    let issue_path = format!("/api/v1/issues/{issue_id}");

    let session = server.get_anon("/api/v1/auth/session");
    assert_status(&session, 200, "R-T7 前置：可信对端取会话必须 200");
    let cookie = session
        .headers
        .get("set-cookie")
        .unwrap_or_else(|| panic!("[R-T7] 可信会话缺 Set-Cookie：{}", session.describe()))
        .to_str()
        .expect("Set-Cookie 非 ASCII")
        .split(';')
        .next()
        .expect("Set-Cookie 格式异常")
        .to_string();
    let csrf = session.str_field("csrf_token");

    // Act 1：无会话 Cookie 的 PATCH —— 信任不兑换写权限
    let no_cookie = server.patch_anon(&issue_path, &serde_json::json!({"status": "resolved"}));
    // Assert ①（R-T7①）
    assert_unauthorized(&no_cookie, "R-T7① 无会话 Cookie PATCH 必须 401（信任不兑换写权限）");

    // Act 2：有 Cookie 但缺 x-csrf-token
    let no_csrf = server.send(
        reqwest::Method::PATCH,
        &issue_path,
        &[("cookie", &cookie)],
        Some(&serde_json::json!({"status": "resolved"})),
    );
    // Assert ②（R-T7②）：403 csrf_invalid，且不得因处于可信对端而被跳过
    assert_status(&no_csrf, 403, "R-T7② 有 Cookie 无 CSRF 必须 403");
    assert_eq!(
        no_csrf.field("error").as_str(),
        Some("csrf_invalid"),
        "[R-T7②] 403 响应体必须是 {{\"error\":\"csrf_invalid\"}}：{}",
        no_csrf.describe()
    );

    // Act 3：有 Cookie + 正确 x-csrf-token
    let with_csrf = server.send(
        reqwest::Method::PATCH,
        &issue_path,
        &[("cookie", &cookie), ("x-csrf-token", &csrf)],
        Some(&serde_json::json!({"status": "resolved"})),
    );
    // Assert ③（R-T7③）：200 且业务码生效（不是 401/403）
    assert_status(&with_csrf, 200, "R-T7③ 有 Cookie + 正确 CSRF 必须 200");
    let reread = server.get_anon(&issue_path);
    assert_status(&reread, 200, "R-T7③ 可信对端回读 issue");
    assert_eq!(
        reread.str_field("status"),
        "resolved",
        "[R-T7③] 带 CSRF 的 PATCH 必须真实生效：{}",
        reread.describe()
    );
}

// ── R-T12：凭证优先，不得因代理头/Host 不匹配反向降级 ──────────────────────

#[test]
fn rt12_static_token_still_wins_over_deny_signals() {
    // Arrange：可信基线
    let server = start_trusted();

    // Act 1：合法 ADMIN_TOKEN + 代理头（trust_decision=Proxied）
    let proxied_with_token = server.send(
        reqwest::Method::GET,
        "/api/v1/issues",
        &[("x-admin-token", TEST_ADMIN_TOKEN), ("x-forwarded-for", "100.64.0.1")],
        None,
    );
    // Assert 1（R-T12）：不得反向降级
    assert_status(
        &proxied_with_token,
        200,
        "R-T12 带合法 ADMIN_TOKEN + 代理头必须 200",
    );

    // Act 2：合法 ADMIN_TOKEN + 伪造 Host（trust_decision=HostNotAllowed，原始 HTTP 精确控制 Host）
    let raw = raw_request(
        &server,
        &format!(
            "GET /api/v1/issues HTTP/1.1\r\nHost: evil.example:3000\r\nx-admin-token: {TEST_ADMIN_TOKEN}\r\nConnection: close\r\n\r\n"
        ),
    );
    // Assert 2（R-T12）
    assert_eq!(
        raw.status, 200,
        "[R-T12] 带合法 ADMIN_TOKEN + 伪造 Host 必须 200，实际响应：{}",
        raw.raw
    );
}

// ── R-T13：Host 门与 CIDR 门 AND 关系，缺一不可（红线②）──────────────────

#[test]
fn rt13_host_match_without_cidr_match_is_denied() {
    // Arrange：Host 白名单命中（127.0.0.1），但 CIDR 只含 100.64.0.0/10
    let server = Server::start_with(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "100.64.0.0/10"),
        ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
    ]);

    // Act
    let list = server.get_anon("/api/v1/issues");

    // Assert（R-T13）：Host 命中但 CIDR 不命中 ⇒ OutsideCidrs ⇒ 401
    assert_unauthorized(&list, "R-T13 Host 命中但 CIDR 不命中必须 401");
}

#[test]
fn rt13_cidr_match_without_host_match_is_denied() {
    // Arrange：CIDR 命中（127.0.0.1/32），但 Host 白名单只含另一个域名
    let server = Server::start_with(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "127.0.0.1/32"),
        ("CONSOLE_TRUSTED_HOSTS", "console.internal"),
    ]);

    // Act
    let list = server.get_anon("/api/v1/issues");

    // Assert（R-T13）：CIDR 命中但 Host 不命中 ⇒ HostNotAllowed ⇒ 401
    assert_unauthorized(&list, "R-T13 CIDR 命中但 Host 不命中必须 401");
}

// ── R-T8b：非法 CIDR 不 panic、fail-closed、回退默认网段 ───────────────────

#[test]
fn rt8b_invalid_cidr_config_degrades_without_crashing() {
    // Arrange：CIDR 列表含非法段
    // [REVIEW-ONLY] WARN 的确切文案由人眼复核；此处机械断言「进程存活 + /healthz 200 + 401 + 日志含 WARN」。
    let server = Server::start_with_log_capture(&[
        ("CLIENT_TOKEN", TEST_CLIENT_TOKEN),
        ("ADMIN_TOKEN", TEST_ADMIN_TOKEN),
        ("CONSOLE_TAILNET_TRUST", "1"),
        ("CONSOLE_TAILNET_CIDRS", "not-a-cidr,100.64.0.0/10"),
        ("CONSOLE_TRUSTED_HOSTS", "127.0.0.1"),
    ]);

    // Act 1：进程未 panic/未退出（能起来本身就说明未崩溃，healthz 再次确认存活）
    let health = server.healthz();
    // Assert 1
    assert_status(&health, 200, "R-T8b 非法 CIDR 配置下进程必须存活且 /healthz 200");

    // Act 2：无凭证读
    let list = server.get_anon("/api/v1/issues");
    // Assert 2（R-T8b）：回退默认 CIDR（不含 loopback）⇒ fail-closed 401，绝不 500、绝不 200
    assert_unauthorized(&list, "R-T8b 非法 CIDR 配置必须 fail-closed 401");

    // Assert 3：启动 WARN（配置降级必须可观测）
    assert_startup_warn(&server, "R-T8b 非法 CIDR 配置必须启动 WARN");
}

// ── R-T10：审计日志脱敏（不得出现任何凭证值）──────────────────────────────

#[test]
fn rt10_logs_never_contain_credentials() {
    // Arrange：捕获 stdout，制造一次可信读 + 一次代理头请求 + 一次会话签发
    let server = Server::start_with_log_capture(&trusted_env());
    let _ = server.get_anon("/api/v1/issues");
    let _ = server.send(
        reqwest::Method::GET,
        "/api/v1/issues",
        &[("x-forwarded-for", "100.64.0.1")],
        None,
    );
    let _ = server.get_anon("/api/v1/auth/session");
    let log = server.captured_log();

    // Assert 0：日志接缝必须真实生效（否则下面的「不含」断言会假绿）
    assert!(
        !log.trim().is_empty(),
        "[R-T10] 未捕获到任何服务端日志：脱敏断言无法成立（接缝失效）"
    );

    // Assert 1：静态凭证值绝不入日志
    for secret in [TEST_ADMIN_TOKEN, TEST_CLIENT_TOKEN] {
        assert!(
            !log.contains(secret),
            "[R-T10] 日志泄漏凭证值 `{secret}`：\n{log}"
        );
    }

    // Assert 2：结构性禁止字段（值语义）不得出现
    // [REVIEW-ONLY] 「信任日志只含 trust_decision/peer/host」的完整性靠 review；
    // 这里机械断言：若出现 trust_decision 字段行，则同一行必须同时带 peer 与 host 结构化字段。
    let lower = log.to_ascii_lowercase();
    for forbidden in ["authorization:", "cookie:", "set-cookie:", "csrf_token="] {
        assert!(
            !lower.contains(forbidden),
            "[R-T10] 日志出现禁止字段 `{forbidden}`：\n{log}"
        );
    }
    for line in log.lines().filter(|line| line.contains("trust_decision")) {
        assert!(
            line.contains("peer") && line.contains("host"),
            "[R-T10] trust_decision 日志行必须同时含 peer 与 host 结构化字段：{line}"
        );
    }
}
