//! Tailnet（Tailscale）可信对端判定——公共契约实现。
//!
//! 契约表：`.dev-team/contract-tailnet-trust.md`（R-T1..R-T13，ver 2）；NFR：`.dev-team/nfr-baseline.json`
//! 的 `tailnet_trust` 段。本模块只做**判定**：取 peer 地址、Host 头与代理头事实，产出
//! [`TrustDecision`]；HTTP 层（`routes.rs`）只把 [`TrustDecision::Trusted`] 兑换成「只读免 token」。
//!
//! 不变量（实现必须保持）：
//! - 安全默认：`CONSOLE_TAILNET_TRUST` 仅 `1`/`true`/`on`（大小写不敏感）开启，**未设置或其它值一律关闭**（R-T4）。
//! - 不得 panic：非法 CIDR / Host 配置只降级为「信任不生效」（fail-closed，R-T8）。
//! - Host 白名单是 DNS rebinding 的唯一防线，缺失/不匹配即不得放行（R-T5b/R-T13）。
//! - 信任只兑换只读：变更请求仍需会话 Cookie + `x-csrf-token`，本模块**不提供**写权限判定（R-T7）。
//! - 不得打印任何 token/凭据；日志只允许结构化字段 `trust_decision` / `peer` / `host`（NFR）。

use std::fmt;
use std::net::IpAddr;

use axum::http::HeaderMap;
use tracing::{info, warn};

/// 默认可信 CIDR：Tailscale IPv4 CGNAT 段 + IPv6 ULA 段（NFR `tailnet_trust.default_cidrs`）。
pub const DEFAULT_TRUSTED_CIDRS: &str = "100.64.0.0/10,fd7a:115c:a1e0::/48";

/// 一个已解析的 CIDR 段。
pub struct Cidr {
    net: IpAddr,
    prefix_len: u8,
}

/// 默认端口剥离：`host` / `host:port` / `[v6]:port` / 裸 IPv6 字面量。
///
/// 返回 `None` 表示格式非法（空端口、缺 `]` 等），由调用方降级为 fail-closed。
fn split_host_port(raw: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = raw.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail {
            "" => None,
            _ => Some(tail.strip_prefix(':')?),
        };
        return Some((host, port));
    }
    // 裸 IPv6 字面量（多个冒号且无括号）没有端口语义，整体视作 host。
    if raw.matches(':').count() > 1 {
        return Some((raw, None));
    }
    match raw.rsplit_once(':') {
        // 空端口（`host:`）是配置缺陷，交给调用方 fail-closed。
        Some((_, "")) => None,
        Some((host, port)) => Some((host, Some(port))),
        None => Some((raw, None)),
    }
}

impl Cidr {
    /// 解析 `addr/prefix_len`（IPv4 或 IPv6）；非法输入返回 `Err`，**不得 panic**（R-T8）。
    pub fn parse(s: &str) -> Result<Cidr, CidrParseError> {
        let raw = s.trim();
        let Some((addr, prefix)) = raw.split_once('/') else {
            return Err(CidrParseError::new(s));
        };
        let (addr, prefix) = (addr.trim(), prefix.trim());
        if addr.is_empty() || prefix.is_empty() || prefix.contains('/') {
            return Err(CidrParseError::new(s));
        }
        let net: IpAddr = addr.parse().map_err(|_| CidrParseError::new(s))?;
        let prefix_len: u8 = prefix.parse().map_err(|_| CidrParseError::new(s))?;
        let max_prefix_len = match net {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix_len > max_prefix_len {
            return Err(CidrParseError::new(s));
        }
        Ok(Cidr { net, prefix_len })
    }

    /// `ip` 是否落在本段内。入参须已由 [`normalize_peer`] 归一化（R-T1..R-T13 前置）。
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = ipv4_mask(self.prefix_len);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = ipv6_mask(self.prefix_len);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            // 家族不匹配即不命中（IPv4 对端不得被 IPv6 段命中，反之亦然）。
            _ => false,
        }
    }
}

fn ipv4_mask(prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    }
}

fn ipv6_mask(prefix_len: u8) -> u128 {
    if prefix_len == 0 {
        0
    } else {
        u128::MAX << (128 - prefix_len)
    }
}

/// Host 白名单条目：`host` 或 `host:port`。比较时忽略大小写与端口，Host 缺失即不匹配（R-T5b）。
pub struct TrustedHost {
    /// 已小写归一化的 host（端口按契约不参与匹配，故不入存储）。
    host: String,
}

impl TrustedHost {
    /// 解析单个条目；空串/非法输入返回 `Err`（由调用方降级为 fail-closed），**不得 panic**。
    pub fn parse(s: &str) -> Result<TrustedHost, CidrParseError> {
        let raw = s.trim();
        let Some((host, port)) = split_host_port(raw) else {
            return Err(CidrParseError::new(s));
        };
        if host.is_empty() {
            return Err(CidrParseError::new(s));
        }
        // 端口不参与匹配，但语法必须合法（拼错端口属配置缺陷，fail-closed 而非静默忽略）。
        if let Some(port) = port {
            port.parse::<u16>().map_err(|_| CidrParseError::new(s))?;
        }
        Ok(TrustedHost {
            host: host.to_ascii_lowercase(),
        })
    }

    /// `host_header` 是否命中本条目：忽略大小写与端口；`None` 或空串 → `false`。
    pub fn matches(&self, host_header: Option<&str>) -> bool {
        let Some(raw) = host_header else {
            return false;
        };
        let Some((host, port)) = split_host_port(raw.trim()) else {
            return false;
        };
        if port.is_some_and(|port| port.parse::<u16>().is_err()) {
            return false;
        }
        !host.is_empty() && host.eq_ignore_ascii_case(&self.host)
    }
}

/// 配置解析错误（CIDR 与 Host 共用）：`Display` + `std::error::Error`。
#[derive(Debug)]
pub struct CidrParseError {
    input: String,
}

impl CidrParseError {
    fn new(input: &str) -> Self {
        Self {
            input: input.to_string(),
        }
    }
}

impl fmt::Display for CidrParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let input = &self.input;
        write!(f, "非法的 CIDR/Host 配置项：{input:?}")
    }
}

impl std::error::Error for CidrParseError {}

/// 信任判定结果（结构化日志字段 `trust_decision` 的取值域）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// 对端地址命中可信 CIDR **且** Host 命中白名单且未经过代理：只读免凭证（R-T1）。
    Trusted,
    /// `CONSOLE_TAILNET_TRUST` 未开启（默认；R-T4）。
    Disabled,
    /// 拿不到对端地址（未接线 connect_info 或非 TCP）：按不可信处理（R-T2）。
    PeerUnknown,
    /// 请求携带 `x-forwarded-for` / `x-real-ip` / `forwarded`：一律不信任（R-T5/R-T4c）。
    Proxied,
    /// 对端地址明确落在可信 CIDR 之外（R-T3/R-T13）。
    OutsideCidrs,
    /// Host 头缺失或不在 `CONSOLE_TRUSTED_HOSTS` 白名单内（防 DNS rebinding，R-T5b）。
    HostNotAllowed,
}

/// 运行时信任配置。`enabled == false` 即关闭；`cidrs` 或 `trusted_hosts` 为空同样等于信任不生效。
pub struct TrustConfig {
    pub enabled: bool,
    cidrs: Vec<Cidr>,
    trusted_hosts: Vec<TrustedHost>,
}

impl TrustConfig {
    /// 从环境变量构造：
    /// - `CONSOLE_TAILNET_TRUST`：仅 `1`/`true`/`on`（大小写不敏感）开启；未设置或其它值一律关闭。
    /// - `CONSOLE_TAILNET_CIDRS`：逗号分隔；未设置（或空白）→ [`DEFAULT_TRUSTED_CIDRS`]。
    /// - `CONSOLE_TRUSTED_HOSTS`：逗号分隔 `host`/`host:port`；未设置或为空 → **无白名单（信任恒不生效）**，
    ///   开启状态下启动打印 WARN。
    ///
    /// 任一解析失败、或显式给出的 CIDR 列表里**零个有效 CIDR** → 信任关闭（fail-closed）并回退默认 CIDR，
    /// **不得 panic**（R-T8b）。空串/未设置仍走默认网段且不告警。
    pub fn from_env() -> TrustConfig {
        let enabled = std::env::var("CONSOLE_TAILNET_TRUST").ok();
        let cidrs = std::env::var("CONSOLE_TAILNET_CIDRS").ok();
        let hosts = std::env::var("CONSOLE_TRUSTED_HOSTS").ok();
        TrustConfig::from_components(enabled.as_deref(), cidrs.as_deref(), hosts.as_deref())
    }

    /// [`Self::from_env`] 的纯函数内核（env 只读一次）：单独暴露给本模块单测，避免测试并发改 env。
    fn from_components(
        enabled_env: Option<&str>,
        cidrs_env: Option<&str>,
        hosts_env: Option<&str>,
    ) -> TrustConfig {
        let enabled = enabled_env.map(is_enabled_value).unwrap_or(false);
        // 空串 = 未设置（走默认网段，不告警）；非空但解析后零个有效 CIDR（如 ","）= 非法配置。
        let cidrs_override = cidrs_env.map(str::trim).filter(|value| !value.is_empty());
        let cidrs_src = cidrs_override.unwrap_or(DEFAULT_TRUSTED_CIDRS);
        let hosts_src = hosts_env.unwrap_or_default();

        let config = match TrustConfig::parse(enabled, cidrs_src, hosts_src) {
            Ok(config) if !(cidrs_override.is_some() && config.cidrs.is_empty()) => config,
            Ok(_) => {
                warn!(
                    trust_enabled = false,
                    fallback_cidrs = DEFAULT_TRUSTED_CIDRS,
                    "CONSOLE_TAILNET_CIDRS 未解析出任何有效 CIDR（显式设置为空白/逗号串）：已关闭信任并回退默认 CIDR（fail-closed）"
                );
                default_config()
            }
            Err(err) => {
                warn!(
                    trust_enabled = false,
                    config_error = %err,
                    fallback_cidrs = DEFAULT_TRUSTED_CIDRS,
                    "tailnet trust 配置非法：已关闭信任并回退默认 CIDR（fail-closed）"
                );
                default_config()
            }
        };

        if config.enabled && config.trusted_hosts.is_empty() {
            warn!(
                trust_enabled = true,
                "CONSOLE_TRUSTED_HOSTS 未设置或为空：Host 白名单为空 ⇒ tailnet 信任恒不生效（fail-closed，防 DNS rebinding）"
            );
        }
        info!(
            trust_enabled = config.enabled,
            trust_cidrs = config.cidrs.len(),
            trusted_hosts = config.trusted_hosts.len(),
            "tailnet trust 配置已加载（默认关闭；开启后只读 GET/HEAD 免 admin token）"
        );
        config
    }

    /// 纯函数构造：`cidrs` / `hosts` 均为逗号分隔列表（空串 = 空列表），任一段非法即返回 `Err`。
    pub fn parse(enabled: bool, cidrs: &str, hosts: &str) -> Result<TrustConfig, CidrParseError> {
        let mut parsed_cidrs = Vec::new();
        for segment in cidrs.split(',') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            parsed_cidrs.push(Cidr::parse(segment)?);
        }

        let mut parsed_hosts = Vec::new();
        for segment in hosts.split(',') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            parsed_hosts.push(TrustedHost::parse(segment)?);
        }

        Ok(TrustConfig {
            enabled,
            cidrs: parsed_cidrs,
            trusted_hosts: parsed_hosts,
        })
    }

    /// 判定优先级（冻结）：`Disabled` > `Proxied` > `PeerUnknown` > `HostNotAllowed` > `Trusted`/`OutsideCidrs`。
    /// [`normalize_peer`] 必须在 CIDR 匹配前应用；Host 门必须在 CIDR 门之前（R-T5b/R-T13）。
    pub fn decide(&self, peer: Option<IpAddr>, proxied: bool, host: Option<&str>) -> TrustDecision {
        if !self.enabled {
            return TrustDecision::Disabled;
        }
        // 代理头只拒不放：不得据 XFF 推断真实对端（R-T5/R-T11）。
        if proxied {
            return TrustDecision::Proxied;
        }
        let Some(peer) = peer else {
            return TrustDecision::PeerUnknown;
        };
        let peer = normalize_peer(peer);
        // Host 门先于 CIDR 门：Host 白名单是 DNS rebinding 的唯一防线（R-T5c/R-T13）。
        if !self
            .trusted_hosts
            .iter()
            .any(|trusted| trusted.matches(host))
        {
            return TrustDecision::HostNotAllowed;
        }
        if self.cidrs.iter().any(|cidr| cidr.contains(peer)) {
            TrustDecision::Trusted
        } else {
            TrustDecision::OutsideCidrs
        }
    }
}

/// 降级兜底配置：信任关闭 + 默认 CIDR + 空白名单。
///
/// [`DEFAULT_TRUSTED_CIDRS`] 恒可解析，`unwrap_or` 只是防御，绝不 panic（R-T8b）。
fn default_config() -> TrustConfig {
    TrustConfig::parse(false, DEFAULT_TRUSTED_CIDRS, "").unwrap_or(TrustConfig {
        enabled: false,
        cidrs: Vec::new(),
        trusted_hosts: Vec::new(),
    })
}

/// `CONSOLE_TAILNET_TRUST` 的唯一开启口径：`1` / `true` / `on`（大小写不敏感，容忍首尾空白）。
fn is_enabled_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on"
    )
}

/// IPv4-mapped IPv6（`::ffff:a.b.c.d`）归一化为 IPv4，其余原样返回（R-T8）。
pub fn normalize_peer(ip: IpAddr) -> IpAddr {
    match ip {
        // 必须用 `to_ipv4_mapped`：`to_ipv4` 会把 `::1`/`::` 也折叠成 0.0.0.1/0.0.0.0。
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// 请求是否携带代理头：存在 `x-forwarded-for`、`x-real-ip` 或 RFC 7239 `forwarded` 即 `true`（R-T5/R-T4c）。
///
/// 只看存在性（HeaderMap 大小写不敏感），**不解析取值**——绝不据首 hop 推断真实对端（R-T11）。
pub fn is_proxied(headers: &HeaderMap) -> bool {
    headers.contains_key("x-forwarded-for")
        || headers.contains_key("x-real-ip")
        || headers.contains_key("forwarded")
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::panic::catch_unwind;

    use axum::http::HeaderValue;

    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    /// R-T4c：RFC 7239 `forwarded` 与 XFF/X-Real-IP 同等视为代理头（只看存在性）。
    #[test]
    fn is_proxied_detects_forwarded_header() {
        assert!(!is_proxied(&HeaderMap::new()), "无代理头时必须为 false");

        let mut forwarded = HeaderMap::new();
        forwarded.insert("forwarded", HeaderValue::from_static("for=100.64.0.1"));
        assert!(
            is_proxied(&forwarded),
            "[R-T4c] 仅带 Forwarded 头也必须判定为经过代理"
        );

        // 大小写不敏感（HTTP 头名语义）+ 空值同样算「存在」。
        let mut upper = HeaderMap::new();
        upper.insert("Forwarded", HeaderValue::from_static(""));
        assert!(is_proxied(&upper), "Forwarded 头存在即拒绝，与取值无关");
    }

    /// 变更 2：显式 `CONSOLE_TAILNET_CIDRS=,`（零个有效 CIDR）按非法配置降级，不 panic、不信任、回退默认；
    /// 而未设置/空串仍走默认网段并保持开启（不降级）。
    #[test]
    fn empty_cidr_override_degrades_to_disabled_without_panicking() {
        let outcome =
            catch_unwind(|| TrustConfig::from_components(Some("1"), Some(","), Some("127.0.0.1")));
        let config = outcome.expect("零个有效 CIDR 的配置不得 panic");
        assert!(!config.enabled, "零个有效 CIDR 必须关闭信任（fail-closed）");
        assert_eq!(
            config.cidrs.len(),
            2,
            "必须回退 DEFAULT_TRUSTED_CIDRS（CGNAT + ULA 两段）"
        );
        assert_ne!(
            config.decide(Some(v4(127, 0, 0, 1)), false, Some("127.0.0.1")),
            TrustDecision::Trusted,
            "降级后即使 CIDR/Host 文本命中也不得判定 Trusted"
        );

        // 未设置 / 空串：走默认网段，不降级（enabled 保持开启）。
        for unset in [None, Some(""), Some("   ")] {
            let config = TrustConfig::from_components(Some("1"), unset, Some("127.0.0.1"));
            assert!(config.enabled, "未设置/空串不得降级为关闭");
            assert_eq!(config.cidrs.len(), 2, "未设置/空串应使用默认网段");
        }
    }
}
