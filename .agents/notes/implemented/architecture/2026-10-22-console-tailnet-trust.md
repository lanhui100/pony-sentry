# Agent Note: 控制台 Tailnet 可信对端免 admin token

Status: implemented
ADR: `docs/decisions/004-console-tailnet-trust.md`
契约: `.dev-team/contract-tailnet-trust.md`（FROZEN ver 2） · NFR: `.dev-team/nfr-baseline.json#tailnet_trust`
前置决策: `2026-10-22-console-server-side-session-auth.md`（服务端会话 + CSRF 机制复用，本决策只在其凭证序末尾追加信任分支）

## Problem

控制台只经 tailnet MagicDNS 访问，但管理读接口要求 `ADMIN_TOKEN` 换短期会话，单人自用场景摩擦过高。
直连部署（无 serve/反代、nat 无 3000 规则、`ss` 对端为客户端 tailnet 地址）使「对端 IP 即来源网络」
这一事实可用，但服务端此前不取 `ConnectInfo`，应用层对来源网络零感知。

## Decision

信任 = `CONSOLE_TAILNET_TRUST=1` ∧ 对端 ∈ `CONSOLE_TAILNET_CIDRS`（默认 Tailscale CGNAT+ULA） ∧
无 `x-forwarded-for`/`x-real-ip` ∧ `Host` ∈ `CONSOLE_TRUSTED_HOSTS`。信任只兑换**只读**：
GET/HEAD 免 token；PATCH 仍要会话 Cookie + `x-csrf-token`；可信对端 `GET /api/v1/auth/session`
自动签发会话，浏览器因此零输入进控制台。默认关闭，由部署 env 显式开启；非法配置 fail-closed + 启动 WARN；
每次放行写结构化审计日志（`trust_decision`/`peer`/`host`，不含凭证）。

## Alternatives considered

- **保持每次输入 token**：零风险，但长期凭证会外溢到命令历史/便签，摩擦即风险。否决。
- **`tailscale serve` + `Tailscale-User-Login` 身份头**：身份来自 WireGuard 认证，比 IP 段诚实；但需新增
  serve 代理层与 443/证书（历史因端口占用放弃），且无 serve 时该头可被 `curl -H` 伪造，必须显式丢弃。否决，
  保留为未来增强。
- **`tailscale whois` 本地 API 收窄到具体账号/非 tagged 节点**：身份更强，但引入 tailscaled 运行时依赖、
  operator 权限、每请求缓存与 fail-closed 复杂度；单用户 tailnet 不划算。否决。
- **只信 `Host` 头 / 只信 `X-Forwarded-For` 首 hop**：均可伪造，等于无鉴权。否决。
- **绑定 tailnet 专用监听器（信任 = 接收接口）**：理论最强，但 tailnet IP 变更、双栈与启动顺序
  （tailscaled 未起即 bind 失败）会让服务起不来。否决，记录为可选加固。
- **默认开启信任**：其它部署会静默放宽鉴权（任何 `100.64/10` 来源获得管理员读权限）。否决。

## Consequences

- 安全边界显式等于「整个 tailnet 成员 + 能把流量落到本机 CGNAT 对端的路径」；读接口暴露全部错误事件。
- 写路径仍有会话 + CSRF 保护；DNS rebinding 由 Host 白名单拦截（`Host: evil.example:3000` → 401）。
- 代理/隧道（nginx、`ssh -L`、port-forward）会因缺 XFF 或 Host 不匹配而落回 token 登录（fail-closed）。
- tailnet 内注销不再有阻断力（再次 GET 会重新签发）；真正吊销用 kill switch 或收紧 ACL/CIDR。
- 残余风险：宿主存在非 tailnet 的 `100.64/10` 网段（`docker --subnet` 等）或 tailnet 节点做无 XFF 代理时
  信任面被放大；用 `CONSOLE_TAILNET_CIDRS` 收窄或关闭信任。
- 证据：`tests/api_tailnet_trust_contract.rs`（17 例黑盒）+ `crates/server/tests/trust_unit.rs`（11 例判定层），
  部署脚本新增 loopback+Host 双向门冒烟断言。
