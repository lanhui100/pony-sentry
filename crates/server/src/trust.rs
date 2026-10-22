//! Tailnet（Tailscale）可信对端判定——**公共契约空桩**。
//!
//! 契约表：`.dev-team/contract-tailnet-trust.md`（R-T1..R-T13，ver 2）；NFR：`.dev-team/nfr-baseline.json`
//! 的 `tailnet_trust` 段。本文件当前只冻结公共类型/签名，函数体一律 `unimplemented!()`，
//! 实现与接线由 Executor 在其写域内补齐（main.rs 取 peer 地址、routes.rs 调用 `decide`）。
//!
//! 不变量（实现时必须保持）：
//! - 安全默认：`CONSOLE_TAILNET_TRUST` 仅 `1`/`true`/`on`（大小写不敏感）开启，**未设置或其它值一律关闭**（R-T4）。
//! - 不得 panic：非法 CIDR / Host 配置只降级为「信任不生效」（fail-closed，R-T8）。
//! - Host 白名单是 DNS rebinding 的唯一防线，缺失/不匹配即不得放行（R-T5b/R-T13）。
//! - 信任只兑换只读：变更请求仍需会话 Cookie + `x-csrf-token`，本模块**不提供**写权限判定（R-T7）。
//! - 不得打印任何 token/凭据；日志只允许结构化字段 `trust_decision` / `peer` / `host`（NFR）。

use std::fmt;
use std::net::IpAddr;

use axum::http::HeaderMap;

/// 默认可信 CIDR：Tailscale IPv4 CGNAT 段 + IPv6 ULA 段（NFR `tailnet_trust.default_cidrs`）。
pub const DEFAULT_TRUSTED_CIDRS: &str = "100.64.0.0/10,fd7a:115c:a1e0::/48";

/// 一个已解析的 CIDR 段。私有字段为实现细节（Executor 可调整），公共方法签名冻结。
pub struct Cidr {
    #[allow(dead_code)] // 空桩阶段无读取方，实现后移除
    net: IpAddr,
    #[allow(dead_code)]
    prefix_len: u8,
}

impl Cidr {
    /// 解析 `addr/prefix_len`（IPv4 或 IPv6）；非法输入返回 `Err`，**不得 panic**（R-T8）。
    #[allow(unused_variables)] // 空桩阶段签名参数暂无读取方
    pub fn parse(s: &str) -> Result<Cidr, CidrParseError> {
        unimplemented!("契约空桩：Cidr::parse 由 Executor 实现（见 R-T8）")
    }

    /// `ip` 是否落在本段内。入参须已由 [`normalize_peer`] 归一化（R-T1..R-T13 前置）。
    #[allow(unused_variables)]
    pub fn contains(&self, ip: IpAddr) -> bool {
        unimplemented!("契约空桩：Cidr::contains 由 Executor 实现（见 R-T3）")
    }
}

/// Host 白名单条目：`host` 或 `host:port`。比较时忽略大小写与端口，Host 缺失即不匹配（R-T5b）。
pub struct TrustedHost {
    #[allow(dead_code)] // 空桩阶段无读取方，实现后移除
    host: String,
    #[allow(dead_code)]
    port: Option<u16>,
}

impl TrustedHost {
    /// 解析单个条目；空串/非法输入返回 `Err`（由调用方降级为 fail-closed），**不得 panic**。
    #[allow(unused_variables)]
    pub fn parse(s: &str) -> Result<TrustedHost, CidrParseError> {
        unimplemented!("契约空桩：TrustedHost::parse 由 Executor 实现（见 R-T5b）")
    }

    /// `host_header` 是否命中本条目：忽略大小写与端口；`None` 或空串 → `false`。
    #[allow(unused_variables)]
    pub fn matches(&self, host_header: Option<&str>) -> bool {
        unimplemented!("契约空桩：TrustedHost::matches 由 Executor 实现（见 R-T5b/R-T13）")
    }
}

/// 配置解析错误（CIDR 与 Host 共用）：`Display` + `std::error::Error`。
#[derive(Debug)]
pub struct CidrParseError {
    #[allow(dead_code)] // 空桩阶段无读取方，实现后移除
    input: String,
}

impl fmt::Display for CidrParseError {
    #[allow(unused_variables)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        unimplemented!("契约空桩：CidrParseError::fmt 由 Executor 实现")
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
    /// 请求携带 `x-forwarded-for` / `x-real-ip`：一律不信任（R-T5）。
    Proxied,
    /// 对端地址明确落在可信 CIDR 之外（R-T3/R-T13）。
    OutsideCidrs,
    /// Host 头缺失或不在 `CONSOLE_TRUSTED_HOSTS` 白名单内（防 DNS rebinding，R-T5b）。
    HostNotAllowed,
}

/// 运行时信任配置。`enabled == false` 即关闭；`cidrs` 或 `trusted_hosts` 为空同样等于信任不生效。
pub struct TrustConfig {
    pub enabled: bool,
    #[allow(dead_code)] // 空桩阶段无读取方，实现后移除
    cidrs: Vec<Cidr>,
    #[allow(dead_code)]
    trusted_hosts: Vec<TrustedHost>,
}

impl TrustConfig {
    /// 从环境变量构造：
    /// - `CONSOLE_TAILNET_TRUST`：仅 `1`/`true`/`on`（大小写不敏感）开启；未设置或其它值一律关闭。
    /// - `CONSOLE_TAILNET_CIDRS`：逗号分隔；未设置 → [`DEFAULT_TRUSTED_CIDRS`]。
    /// - `CONSOLE_TRUSTED_HOSTS`：逗号分隔 `host`/`host:port`；未设置或为空 → **无白名单（信任恒不生效）**，
    ///   开启状态下启动打印 WARN。
    ///
    /// 任一解析失败 → 信任关闭（fail-closed）并回退默认 CIDR，**不得 panic**（R-T8b）。
    pub fn from_env() -> TrustConfig {
        unimplemented!("契约空桩：TrustConfig::from_env 由 Executor 实现（见 R-T4/R-T8）")
    }

    /// 纯函数构造：`cidrs` / `hosts` 均为逗号分隔列表（空串 = 空列表），任一段非法即返回 `Err`。
    #[allow(unused_variables)]
    pub fn parse(
        enabled: bool,
        cidrs: &str,
        hosts: &str,
    ) -> Result<TrustConfig, CidrParseError> {
        unimplemented!("契约空桩：TrustConfig::parse 由 Executor 实现（见 R-T5b/R-T8）")
    }

    /// 判定优先级（冻结）：`Disabled` > `Proxied` > `PeerUnknown` > `HostNotAllowed` > `Trusted`/`OutsideCidrs`。
    /// [`normalize_peer`] 必须在 CIDR 匹配前应用；Host 门必须在 CIDR 门之前（R-T5b/R-T13）。
    #[allow(unused_variables)]
    pub fn decide(
        &self,
        peer: Option<IpAddr>,
        proxied: bool,
        host: Option<&str>,
    ) -> TrustDecision {
        unimplemented!("契约空桩：TrustConfig::decide 由 Executor 实现（见 R-T1..R-T13）")
    }
}

/// IPv4-mapped IPv6（`::ffff:a.b.c.d`）归一化为 IPv4，其余原样返回（R-T8）。
#[allow(unused_variables)]
pub fn normalize_peer(ip: IpAddr) -> IpAddr {
    unimplemented!("契约空桩：normalize_peer 由 Executor 实现（见 R-T8）")
}

/// 请求是否携带代理头：存在 `x-forwarded-for` 或 `x-real-ip` 即 `true`（R-T5）。
#[allow(unused_variables)]
pub fn is_proxied(headers: &HeaderMap) -> bool {
    unimplemented!("契约空桩：is_proxied 由 Executor 实现（见 R-T5）")
}
