# ADR 004: 控制台在 Tailnet 内免 admin token（可信对端只读放行）

**状态**: 已采纳
**日期**: 2026-10-22
**影响范围**: 控制台管理读接口鉴权、`GET /api/v1/auth/session`、服务端监听接线、部署环境变量、安全边界

**契约真源**: `.dev-team/contract-tailnet-trust.md`（FROZEN ver 2，R-T1..R-T13）
· **NFR**: `.dev-team/nfr-baseline.json` 的 `tailnet_trust` 段
· **决策记录**: `.agents/notes/implemented/architecture/2026-10-22-console-tailnet-trust.md`

## 问题

控制台（`GET /api/v1/issues*`、`/projects`、`/traces*` 与两处 PATCH）要求凭证：静态 `ADMIN_TOKEN`
或用它换来的短期会话 Cookie。控制台只经 tailnet MagicDNS（`devserver.taildb165c.ts.net:3000`）访问，
使用者就是 tailnet 内本人 —— 每次打开页面输 token 的摩擦与安全收益不成比例。

部署事实（只读核验）：

- 服务 systemd 直接 `bind 0.0.0.0:3000`（`crates/server/src/main.rs`），无 tailscale serve / funnel、
  无 nginx / Traefik 反代控制台，`nat` 表无 3000 相关 DNAT；`ss -tn '( sport = :3000 )'` 的对端即客户端
  tailnet 地址（如 `100.97.143.121`）。
- 公网 Ingress 只有两条 Exact 路径（`/api/v1/ingest`、`/healthz`），管理路由无公网入口。
- 服务端监听直接 `bind 0.0.0.0:3000` 并以 `ConnectInfo` 取对端地址，应用层据此感知来源网络。

## 决策

**信任判定 = 四个门的与（AND）**，任一不满足即回落到既有 R10 token/会话鉴权：

| 门 | 实现 | 失败语义 |
| --- | --- | --- |
| 总开关 | `CONSOLE_TAILNET_TRUST` 仅 `1`/`true`/`on` 开启；**未设置或其它值 = 关闭** | `TrustDecision::Disabled` |
| 对端网段 | `CONSOLE_TAILNET_CIDRS`，默认 `100.64.0.0/10,fd7a:115c:a1e0::/48`（Tailscale CGNAT + ULA） | `OutsideCidrs` |
| 非代理 | 请求带 `x-forwarded-for`/`x-real-ip` 即不可信（不取首 hop 反推） | `Proxied` |
| Host 白名单 | `CONSOLE_TRUSTED_HOSTS`（忽略大小写/端口）；空列表 = 信任恒不生效 | `HostNotAllowed` |

追加约束：

1. **信任只兑换只读**：`GET`/`HEAD` 管理路由免 admin token（`OPTIONS` 不在放行面内）；**PATCH 不被信任直接放行**，
   仍必须持有会话 Cookie 且带匹配的 `x-csrf-token`（可信 ≠ 跳过 CSRF）。
2. 可信对端无会话访问 `GET /api/v1/auth/session` 时**自动签发**短期会话（`Set-Cookie` + 返回 `csrf_token`），
   浏览器因此零人工输入即可进入控制台；不可信来源仍 401。
3. 默认关闭：代码默认不信任任何来源，由部署 env（`/etc/pony-sentry/pony-sentry.env`）显式开启，
   便于审计与一键回退（`CONSOLE_TAILNET_TRUST=0` 即 kill switch）。
4. 非法 `CONSOLE_TAILNET_CIDRS`/`CONSOLE_TRUSTED_HOSTS` 不 panic：降级为信任关闭 + 回退默认网段 + 启动 WARN。
5. 每次信任放行写结构化审计日志（`trust_decision`/`peer`/`host`），任何日志不含 token/cookie/csrf。

## Alternatives considered

- **保持现状（每次输 admin token）**：零新增风险，但把长期管理凭证的摩擦留给人，实际会导致 token 被
  贴在便签/命令历史里；本次诉求即消除该摩擦。否决。
- **`tailscale serve` 注入 `Tailscale-User-Login` 身份头**：身份信号比 IP 段更诚实（WireGuard 层认证）。
  但需引入 serve 代理层 + 443/证书，历史已因端口占用与跨节点放弃（ADR 002）；且**无 serve 时该头纯客户端
  可控**（`curl -H` 即可伪造），实现必须显式丢弃，不能作为身份来源。否决（可作为未来增强）。
- **`tailscale whois` 本地 API 校验收窄到具体账号/非 tagged 节点**：身份更强，但引入 tailscaled 运行时依赖、
  需要 `tailscale set --operator=dm`、每请求缓存与 fail-closed 复杂度；单用户 tailnet 收益不抵运维成本。否决。
- **只看 Host 白名单 / 只看 `Host` 头**：Host 可被任意客户端伪造，单独使用等于没有鉴权。否决。
- **`X-Forwarded-For` 首 hop 判可信**：头可伪造且当前限流键已证明其不可信（`session.rs`）。否决。
- **绑定 tailnet 专用监听器（信任 = 接收接口）**：理论上最强（连接只能来自 WireGuard 接口），但 tailnet IP
  变更、双栈、启动顺序（tailscaled 未起则 bind 失败）都会让服务起不来；收益主要覆盖「100.64/10 冲突环境」，
  该环境可用 `CONSOLE_TAILNET_CIDRS` 显式收窄。否决，记录为后续可选加固。
- **信任默认开启**：对其它部署会静默放宽鉴权（任何 100.64/10 来源即管理员读权限），安全默认不可接受。否决。

## Consequences

- **安全边界明确为「整个 tailnet 成员 + 任何能让流量落到本机 100.64/10 对端的路径」**：tailnet ACL、
  节点授权、`tailscale up` 的 key 分发成为控制台读权限的实际边界；读接口暴露全部错误事件与堆栈。
- **写操作仍有凭证**：PATCH 需会话 Cookie + CSRF；会话由可信对端自动签发，因此浏览器无感、纯 API 客户端
  仍需先 `GET /auth/session` 拿 csrf。
- **DNS rebinding 由 Host 白名单拦截**：`evil.example:3000` 解析到本机时 Host 不匹配 → 401（R-T5c）。
- **代理/隧道会打断信任（fail-closed）**：经 nginx/kubectl port-forward/ssh -L 转发（无 XFF 时对端才是
  100.64/10 或 loopback）需显式配置，或直接落回 token 登录；aliyun 等节点若做「不加 XFF 的代理」会把公网
  流量放大成可信来源 —— 属已知残余风险，见下。
- **注销在 tailnet 内不再具有阻断力**：`DELETE /auth/session` 后再次 `GET /auth/session` 会重新签发，
  这是「无需 token」的必然结果；需要真正吊销时用 kill switch 或收紧网段/ACL。
- **会话签发不幂等（残余风险）**：可信对端每次无 Cookie 请求 `GET /auth/session` 都会签发新会话；
  重复/并发无 Cookie 请求会累积会话。会话表满 512 条时先清理过期，仍满则**只**驱逐 `TrustedPeer`
  来源中最早者 —— 手动 token 会话不被可信流量驱逐（仅当表内全是手动会话才回退驱逐最早者）。
  被驱逐的可信会话影响是重新签发（可恢复），不构成越权。按对端 IP 复用会话会引入同 IP 多设备共享会话
  与会话固定风险，收益为负，故不采用。
- **残余风险（显式记录）**：① 若宿主/上游存在 `100.64.0.0/10` 的非 tailnet 网段（如 `docker network create
  --subnet 100.64.0.0/16`、CGNAT 上游），这些来源会被判可信 —— 需用 `CONSOLE_TAILNET_CIDRS` 收窄；
  ② tailnet 内任何做透明代理的节点会扩大暴露面；③ 单副本内存会话模型不变。
- **测试与证据**：契约级黑盒 `tests/api_tailnet_trust_contract.rs`（17 例）+ 判定层单测
  `crates/server/tests/trust_unit.rs`（11 例）；部署脚本新增「Host 白名单 + loopback 双向门」冒烟断言。
