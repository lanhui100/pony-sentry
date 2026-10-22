//! Tailnet 信任判定的**纯函数判定层**单测（R-T5/R-T7/R-T8 的「传输层不参与」部分）。
//!
//! 契约真源：`.dev-team/contract-tailnet-trust.md`（FROZEN ver 2）。
//! 本文件只调用 `pony_sentry_server::trust` 的冻结公共面，不依赖 axum 路由与数据库；
//! 因此任何失败都只能归因于「判定能力缺失」，不会被传输层噪声掩盖。
//!
//! 红相口径：空桩阶段这些函数体是 `unimplemented!()` → 调用即 panic → 用例失败，
//! 失败原因如实为「能力缺失」。**故意不使用 `#[should_panic]`**：那会让空桩 panic 被
//! 计成通过（假绿），与「红相 = 用例存在且失败原因为能力缺失」的验收口径相反。
//! 「非法输入不得 panic」用 `catch_unwind` 表达真实断言（panic 即 FAIL）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::panic::catch_unwind;

use axum::http::{HeaderMap, HeaderValue};
use pony_sentry_server::trust::{
    is_proxied, normalize_peer, Cidr, TrustConfig, TrustDecision, TrustedHost,
    DEFAULT_TRUSTED_CIDRS,
};

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn v6(text: &str) -> IpAddr {
    IpAddr::V6(
        text.parse::<Ipv6Addr>()
            .unwrap_or_else(|err| panic!("非法测试地址 {text}：{err}")),
    )
}

/// 契约 §1：默认可信网段常量冻结为 CGNAT + Tailscale ULA。
#[test]
fn rt8_default_trusted_cidrs_match_frozen_contract() {
    assert_eq!(
        DEFAULT_TRUSTED_CIDRS, "100.64.0.0/10,fd7a:115c:a1e0::/48",
        "[R-T8] DEFAULT_TRUSTED_CIDRS 必须与契约 §1 逐字节一致"
    );
}

/// R-T8：`normalize_peer` 把 IPv4-mapped IPv6 归一化为 IPv4，其余原样返回。
#[test]
fn rt8_normalize_peer_maps_ipv4_mapped_ipv6() {
    // Act / Assert：::ffff:100.64.0.1 → 100.64.0.1
    assert_eq!(
        normalize_peer(v6("::ffff:100.64.0.1")),
        v4(100, 64, 0, 1),
        "[R-T8] ::ffff:100.64.0.1 必须归一化为 100.64.0.1"
    );
    // 真实 IPv6（Tailscale ULA）原样返回
    assert_eq!(
        normalize_peer(v6("fd7a:115c:a1e0::1")),
        v6("fd7a:115c:a1e0::1"),
        "[R-T8] IPv6 对端必须原样返回"
    );
    // 已是 IPv4 的对端不受影响
    assert_eq!(
        normalize_peer(v4(127, 0, 0, 1)),
        v4(127, 0, 0, 1),
        "[R-T8] IPv4 对端必须原样返回"
    );
    // IPv6 loopback 不得被误归一化为 0.0.0.0/其它
    assert_eq!(
        normalize_peer(v6("::1")),
        v6("::1"),
        "[R-T8] ::1 不是 IPv4-mapped，必须原样返回"
    );
}

/// R-T3/R-T8：IPv4 CIDR 的边界命中与不命中（100.63.255.255 / 100.128.0.0 不得命中）。
#[test]
fn rt8_cidr_contains_ipv4_boundaries() {
    let cidr = Cidr::parse("100.64.0.0/10").expect("[R-T8] 100.64.0.0/10 必须是合法 CIDR");

    assert!(
        cidr.contains(v4(100, 64, 0, 1)),
        "[R-T3] 100.64.0.1 必须命中 /10"
    );
    assert!(
        cidr.contains(v4(100, 64, 0, 0)),
        "[R-T3] 网段首地址必须命中"
    );
    assert!(
        cidr.contains(v4(100, 127, 255, 255)),
        "[R-T3] 100.127.255.255 是 100.64.0.0/10 的末地址，必须命中"
    );
    assert!(
        !cidr.contains(v4(100, 63, 255, 255)),
        "[R-T3] 100.63.255.255 在 /10 之外，不得命中"
    );
    assert!(
        !cidr.contains(v4(100, 128, 0, 0)),
        "[R-T3] 100.128.0.0 在 /10 之外，不得命中"
    );
    assert!(
        !cidr.contains(v4(127, 0, 0, 1)),
        "[R-T3] IPv4 CIDR 不得命中家族不匹配以外的 loopback"
    );
    assert!(
        !cidr.contains(v6("fd7a:115c:a1e0::1")),
        "[R-T3] IPv6 对端不得命中 IPv4 CIDR（家族不匹配即不命中）"
    );
}

/// R-T3/R-T8：Tailscale ULA `fd7a:115c:a1e0::/48` 的命中与边界。
#[test]
fn rt8_cidr_contains_ipv6_ula_boundaries() {
    let cidr = Cidr::parse("fd7a:115c:a1e0::/48").expect("[R-T8] ULA /48 必须是合法 CIDR");

    assert!(
        cidr.contains(v6("fd7a:115c:a1e0::1")),
        "[R-T3] fd7a:115c:a1e0::1 必须命中 ULA /48"
    );
    assert!(
        cidr.contains(v6("fd7a:115c:a1e0:ffff::1")),
        "[R-T3] /48 末段 fd7a:115c:a1e0:ffff::1 必须命中"
    );
    assert!(
        !cidr.contains(v6("fd7a:115c:a1e1::1")),
        "[R-T3] fd7a:115c:a1e1::1 超出 /48，不得命中"
    );
    assert!(
        !cidr.contains(v4(100, 64, 0, 1)),
        "[R-T3] IPv4 对端不得命中 IPv6 CIDR"
    );
}

/// R-T8：非法 CIDR 输入必须 `Err` 且**不 panic**（catch_unwind 捕获即 FAIL）。
#[test]
fn rt8_invalid_cidr_returns_err_without_panicking() {
    // 契约 §1：逗号分隔 CIDR，格式为 `addr/prefix_len`；以下均为非法段
    let invalid = [
        "not-a-cidr",
        "100.64.0.0",    // 缺 prefix
        "100.64.0.0/33", // IPv4 prefix 越界
        "100.64.0.0/-1",
        "100.64.0.0/abc",
        "300.0.0.0/8",          // 非法 IPv4
        "fd7a:115c:a1e0::/129", // IPv6 prefix 越界
        "/10",
        "",
    ];
    for input in invalid {
        let outcome = catch_unwind(|| Cidr::parse(input));
        assert!(
            outcome.is_ok(),
            "[R-T8] Cidr::parse({input:?}) 不得 panic（fail-closed 必须可降级）"
        );
        assert!(
            outcome.unwrap().is_err(),
            "[R-T8] Cidr::parse({input:?}) 必须返回 Err"
        );
    }
}

/// R-T5b/R-T13：Host 白名单比较忽略大小写与端口；缺失/空串一律不匹配。
#[test]
fn rt5b_trusted_host_matches_ignoring_case_and_port() {
    let host = TrustedHost::parse("127.0.0.1").expect("[R-T5b] 127.0.0.1 必须是合法 Host 条目");

    assert!(
        host.matches(Some("127.0.0.1")),
        "[R-T5b] 精确 Host 必须命中"
    );
    assert!(
        host.matches(Some("127.0.0.1:3000")),
        "[R-T5b] Host 头带端口（测试客户端形态）必须命中（忽略端口）"
    );
    assert!(
        !host.matches(Some("evil.example:3000")),
        "[R-T5c] evil.example:3000 不得命中 127.0.0.1 白名单"
    );
    assert!(
        !host.matches(None),
        "[R-T5b] Host 缺失必须视为不匹配（fail-closed）"
    );
    assert!(!host.matches(Some("")), "[R-T5b] 空 Host 必须视为不匹配");

    let named = TrustedHost::parse("Console.Example.COM:443")
        .expect("[R-T5b] host:port 形态必须是合法条目");
    assert!(
        named.matches(Some("console.example.com")),
        "[R-T5b] 大小写不敏感且忽略端口"
    );
    assert!(
        named.matches(Some("CONSOLE.example.com:8443")),
        "[R-T5b] 条目端口与 Host 头端口不同也必须命中（忽略端口）"
    );
    assert!(
        !named.matches(Some("evil.console.example.com")),
        "[R-T5b] 不得做子域后缀匹配（必须精确匹配某条目）"
    );
}

/// R-T5/R-T11：代理头判定只看存在性，不看取值（不得据 XFF 推断对端）。
#[test]
fn rt5_is_proxied_detects_header_presence_only() {
    let none = HeaderMap::new();
    assert!(!is_proxied(&none), "[R-T5] 无代理头时必须为 false");

    let mut xff = HeaderMap::new();
    xff.insert("x-forwarded-for", HeaderValue::from_static("100.64.0.1"));
    assert!(is_proxied(&xff), "[R-T5] 存在 x-forwarded-for 必须为 true");

    let mut xri = HeaderMap::new();
    xri.insert("x-real-ip", HeaderValue::from_static("100.64.0.1"));
    assert!(is_proxied(&xri), "[R-T5b] 存在 x-real-ip 必须为 true");

    // 只看存在性：空值同样视为经过代理（不得解读取值、不得放行）
    let mut empty = HeaderMap::new();
    empty.insert("x-forwarded-for", HeaderValue::from_static(""));
    assert!(is_proxied(&empty), "[R-T11] 代理头存在即拒绝，与取值无关");
}

/// R-T8：契约原文判定链——`parse(true, 默认网段, "host")` + IPv4-mapped 对端 → `Trusted`。
#[test]
fn rt8_contract_decide_chain_returns_trusted() {
    let config = TrustConfig::parse(true, "100.64.0.0/10,fd7a:115c:a1e0::/48", "host")
        .expect("[R-T8] 契约给定的合法配置必须解析成功");

    // 契约原文：decide(Some(::ffff:100.64.0.1), false, Some("host")) == Trusted
    // （normalize_peer 必须在 CIDR 匹配前应用，故 decide 内部需自行归一化）
    assert_eq!(
        config.decide(Some(v6("::ffff:100.64.0.1")), false, Some("host")),
        TrustDecision::Trusted,
        "[R-T8] IPv4-mapped 对端 + Host 命中必须判定 Trusted"
    );
    // 归一化后再传入也必须 == Trusted
    assert_eq!(
        config.decide(
            Some(normalize_peer(v6("::ffff:100.64.0.1"))),
            false,
            Some("host")
        ),
        TrustDecision::Trusted,
        "[R-T8] 归一化后的 100.64.0.1 + Host 命中必须判定 Trusted"
    );
    // Tailscale ULA 对端同样命中
    assert_eq!(
        config.decide(Some(v6("fd7a:115c:a1e0::1")), false, Some("host")),
        TrustDecision::Trusted,
        "[R-T8] ULA 对端 + Host 命中必须判定 Trusted"
    );
}

/// R-T7/R-T8：`decide` 冻结优先级 `Disabled > Proxied > PeerUnknown > HostNotAllowed > Trusted/OutsideCidrs`。
#[test]
fn rt8_decide_priority_matrix_is_frozen() {
    let disabled = TrustConfig::parse(false, "127.0.0.1/32", "127.0.0.1")
        .expect("[R-T4] 关闭态配置必须可解析");
    let enabled =
        TrustConfig::parse(true, "127.0.0.1/32", "127.0.0.1").expect("[R-T1] 可信配置必须可解析");

    // ① Disabled 最高：即使 proxy 头 / Host / CIDR 全部命中
    assert_eq!(
        disabled.decide(Some(v4(127, 0, 0, 1)), true, Some("127.0.0.1")),
        TrustDecision::Disabled,
        "[R-T4] 关闭态必须优先于一切命中"
    );
    // ② Proxied 次之：即使 Host + CIDR 命中
    assert_eq!(
        enabled.decide(Some(v4(127, 0, 0, 1)), true, Some("127.0.0.1")),
        TrustDecision::Proxied,
        "[R-T5] 代理头必须优先于 Host/CIDR 命中"
    );
    // ③ PeerUnknown：拿不到对端地址时绝不 fail-open
    assert_eq!(
        enabled.decide(None, false, Some("127.0.0.1")),
        TrustDecision::PeerUnknown,
        "[R-T2] peer=None 必须 PeerUnknown（绝不 fail-open）"
    );
    // ④ Host 门先于 CIDR 门：Host 缺失 / 不匹配时，即使对端在可信网段也 HostNotAllowed
    assert_eq!(
        enabled.decide(Some(v4(127, 0, 0, 1)), false, None),
        TrustDecision::HostNotAllowed,
        "[R-T5c/R-T13] Host 缺失必须 HostNotAllowed"
    );
    assert_eq!(
        enabled.decide(Some(v4(127, 0, 0, 1)), false, Some("evil.example:3000")),
        TrustDecision::HostNotAllowed,
        "[R-T5c] Host 不在白名单必须 HostNotAllowed（即使 CIDR 命中）"
    );
    assert_eq!(
        enabled.decide(Some(v4(8, 8, 8, 8)), false, Some("evil.example")),
        TrustDecision::HostNotAllowed,
        "[R-T13] Host 门必须先于 CIDR 门（两者都不满足时报 HostNotAllowed）"
    );
    // ⑤ OutsideCidrs：Host 命中但对端在网段外
    assert_eq!(
        enabled.decide(Some(v4(8, 8, 8, 8)), false, Some("127.0.0.1")),
        TrustDecision::OutsideCidrs,
        "[R-T13] Host 命中但 CIDR 不命中必须 OutsideCidrs"
    );
    // ⑥ Trusted：两个门槛都命中（Host 带端口也必须命中）
    assert_eq!(
        enabled.decide(Some(v4(127, 0, 0, 1)), false, Some("127.0.0.1:3000")),
        TrustDecision::Trusted,
        "[R-T1] Host（忽略端口）+ CIDR 双命中必须 Trusted"
    );
}

/// R-T5d：空白名单 = 信任恒不生效（fail-closed），空 CIDR 列表同理。
#[test]
fn rt5d_empty_allowlist_or_empty_cidrs_never_trusts() {
    let no_hosts = TrustConfig::parse(true, "127.0.0.1/32", "")
        .expect("[R-T5d] 空白名单是合法配置，但语义是信任不生效");
    assert_eq!(
        no_hosts.decide(Some(v4(127, 0, 0, 1)), false, Some("127.0.0.1")),
        TrustDecision::HostNotAllowed,
        "[R-T5d] 空白名单时即使 CIDR+Host 文本都命中也不得 Trusted"
    );

    let no_cidrs = TrustConfig::parse(true, "", "127.0.0.1").expect("[R-T8] 空 CIDR 列表可解析");
    assert_eq!(
        no_cidrs.decide(Some(v4(127, 0, 0, 1)), false, Some("127.0.0.1")),
        TrustDecision::OutsideCidrs,
        "[R-T8] 空 CIDR 列表不得命中任何对端"
    );
}

/// R-T8b：非法 CIDR 配置必须 `Err` 且不 panic（由 `from_env` 降级为信任关闭）。
#[test]
fn rt8b_illegal_cidr_config_returns_err_without_panicking() {
    let outcome =
        catch_unwind(|| TrustConfig::parse(true, "not-a-cidr,100.64.0.0/10", "127.0.0.1"));
    assert!(
        outcome.is_ok(),
        "[R-T8b] 非法 CIDR 配置不得 panic（进程必须存活）"
    );
    assert!(
        outcome.unwrap().is_err(),
        "[R-T8b] 非法 CIDR 配置必须返回 Err，由 from_env 降级为 fail-closed"
    );
}
