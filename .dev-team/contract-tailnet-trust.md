# 契约表：Tailnet 可信对端免 admin token（R-T1..R-T13，含 R-T4c / R-T8c）

**状态**：FROZEN **ver 3**（L3A 评审后仅文档修订：新增 R-T4c/R-T8c、校正 R-T10；代码/测试零改动；覆盖 ver 2/ver 1） · **Owner**：protocol-agent（T1） · **执行方**：T3 Executor · **验收方**：T2 Test Agent（本表只读）
**冻结面**：`crates/server/src/trust.rs` 公共签名 + 环境变量语义 + 下表 HTTP 断言口径。改本表任何一行 = 破坏冻结，须 Lead 裁决并升 ver。
**依赖事实**：管理路由由 `admin_auth_middleware`（`crates/server/src/routes.rs:438-441`）保护；`authorize_admin`（`routes.rs:109-150`）判定顺序 静态 token → Cookie 会话(+CSRF) → Bearer 会话；失败响应 401 `{"error":"unauthorized"}`、CSRF 失败 403 `{"error":"csrf_invalid"}`、未配置 token 503 `{"error":"admin_token_not_configured"}`。
**目标**：来自 Tailscale 对端（可信 CIDR **且** Host 命中白名单 **且** 无代理头）的**只读**请求免 admin token；其他来源/其他方法的行为与响应码完全不变。

## 0. 核心红线（三条，任一缺失即 FAIL）

> **① 安全默认关闭。** `CONSOLE_TAILNET_TRUST` 只有 `1`/`true`/`on`（大小写不敏感）才开启；未设置或任何其它值一律关闭（`default_enabled: false`），由部署 env 显式开启。
>
> **② Host 白名单是 DNS rebinding 的唯一防线。** 缺失或不匹配即不得放行；空白名单 = 信任恒不生效，且开启状态下启动必须 WARN。`Host: evil.example:3000` 之类即使 CIDR 命中 → 401。
>
> **③ 可信对端 ≠ 跳过 CSRF。** 信任只兑换**只读**（GET/HEAD）免凭证；PATCH 等变更请求必须持有会话 Cookie 并携带匹配的 `x-csrf-token`，否则 403 `csrf_invalid`。会话由可信对端 `GET /api/v1/auth/session` 自动签发（仍无需人工 token），trust 模块**不**提供写权限判定函数。

## 1. 环境变量语义（冻结，ver 3）

| 变量 | 取值 | 语义 |
| --- | --- | --- |
| `CONSOLE_TAILNET_TRUST` | `1` / `true` / `on`（大小写不敏感） | 开启信任（唯一开启口径） |
| `CONSOLE_TAILNET_TRUST` | **未设置**，或 `0`/`false`/`off`/任意其它值 | 关闭信任（`TrustDecision::Disabled`，全部来源走既有 R10 鉴权） |
| `CONSOLE_TAILNET_CIDRS` | 逗号分隔 CIDR；未设置 | 用 `DEFAULT_TRUSTED_CIDRS = "100.64.0.0/10,fd7a:115c:a1e0::/48"` |
| `CONSOLE_TAILNET_CIDRS` | 含任一非法段 | 不 panic；**信任关闭**并回退默认 CIDR、启动 WARN（R-T8b） |
| `CONSOLE_TRUSTED_HOSTS` | 逗号分隔 `host` 或 `host:port`（比较忽略大小写与端口） | Host 白名单；Host 头必须精确匹配某条目（忽略端口） |
| `CONSOLE_TRUSTED_HOSTS` | 未设置 / 空串 | **无白名单 → 信任恒不生效**（fail-closed），开启状态下启动 WARN |

## 2. 判定优先级（冻结，`TrustConfig::decide`）

`Disabled` > `Proxied` > `PeerUnknown` > `HostNotAllowed` > `Trusted` / `OutsideCidrs`。

- `normalize_peer` 必须先于 CIDR 匹配（R-T8）；Host 门先于 CIDR 门（R-T5c/R-T13）。
- `peer == None`（未接线 connect_info）→ `PeerUnknown`，绝不 fail-open。
- **代理门判定头集合（ver 3 冻结）**：`x-forwarded-for` | `x-real-ip` | `forwarded`（RFC 7239）。任一存在即 `Proxied`，**只拒不放、不解析取值**（不取 hop、不解析 `for=` 参数），见 R-T4c。
- **CIDR 列表降级（ver 3 冻结）**：`CONSOLE_TAILNET_CIDRS` 解析后为**零个有效 CIDR**（如 `,`、` , ` 等仅分隔符/空白的串）视为非法配置 → 降级 + WARN（R-T8c）；**未设置或空串** → 用默认值且**不告警**。
- `Trusted` **只影响**「无可识别凭证 + 方法为 GET/HEAD」这一个分支；已带合法静态 token 或合法会话的请求走原路径，`trust_decision` 仅用于日志（R-T12 不可回归）。

| TrustDecision | 无凭证 GET/HEAD 管理路由 | 无凭证/无会话 PATCH |
| --- | --- | --- |
| `Trusted` | 200（放行） | 401 `unauthorized`（信任不兑换写权限） |
| `Disabled` / `PeerUnknown` / `Proxied` / `OutsideCidrs` / `HostNotAllowed` | 401 `unauthorized` | 401 `unauthorized` |

## 3. 契约表 R-T1..R-T8（黑盒 HTTP 断言口径）

真实进程（`tests/harness.rs` 启服务）为对象；「env」列是该用例额外注入的环境变量；「无凭证」= 不带 `Authorization`、不带会话 Cookie。测试客户端经 `127.0.0.1:<port>` 访问时 Host 头为 `127.0.0.1:<port>`，故 Host 白名单条目写 `127.0.0.1`（忽略端口）。

| ID | 前置 env | 请求 | 期望断言（机器可验证） | 反例即 FAIL |
| --- | --- | --- | --- | --- |
| **R-T1** | `CONSOLE_TAILNET_TRUST=1`，`CONSOLE_TAILNET_CIDRS=127.0.0.1/32`，`CONSOLE_TRUSTED_HOSTS=127.0.0.1` | 无凭证 `GET /api/v1/issues` | **200**，body 为 JSON 且无 `error` 字段 | 401/403 |
| **R-T2** | 同 R-T1，但 `CONSOLE_TAILNET_TRUST` **未设置** | 无凭证 `GET /api/v1/issues` | **401** `unauthorized`（安全默认关闭） | 200 |
| **R-T3** | `CONSOLE_TAILNET_TRUST=1`，`CONSOLE_TAILNET_CIDRS` 未设置（默认网段，不含 localhost），`CONSOLE_TRUSTED_HOSTS=127.0.0.1` | 无凭证 `GET /api/v1/issues` | **401** `unauthorized`（CIDR 不命中） | 200 |
| **R-T4** | `CONSOLE_TAILNET_TRUST=0`（另各跑 `false`/`off`/空串 变体），`CONSOLE_TAILNET_CIDRS=127.0.0.1/32`，`CONSOLE_TRUSTED_HOSTS=127.0.0.1` | 无凭证 `GET /api/v1/issues` | **401** `unauthorized`（kill switch 优先于 Host/CIDR 命中） | 200 |
| **R-T4c** | 同 R-T1（CIDR+Host 均命中） | 无凭证 `GET /api/v1/issues` + 头 `Forwarded: for=100.64.0.1`（RFC 7239） | **401** `unauthorized`（`forwarded` 同属代理门，只拒不放、不解析取值） | 200 |
| **R-T5** | 同 R-T1 | 无凭证 `GET /api/v1/issues` + 头 `X-Forwarded-For: 100.64.0.1` | **401** `unauthorized`（代理头门） | 200 |
| **R-T5b** | 同 R-T1 | 无凭证 `GET /api/v1/issues` + 头 `X-Real-IP: 100.64.0.1` | **401** `unauthorized`（代理头门） | 200 |
| **R-T5c** | 同 R-T1 | 无凭证 `GET /api/v1/issues` + 头 `Host: evil.example:3000` | **401** `unauthorized`（Host 不在白名单，DNS rebinding 防线） | 200 |
| **R-T5d** | `CONSOLE_TAILNET_TRUST=1`，`CONSOLE_TAILNET_CIDRS=127.0.0.1/32`，`CONSOLE_TRUSTED_HOSTS` **未设置/空** | 无凭证 `GET /api/v1/issues` | **401** `unauthorized`（空白名单 = 信任恒不生效）；启动日志含 `WARN` | 200 / 无 WARN |
| **R-T6** | 同 R-T1 | 无 Cookie `GET /api/v1/auth/session` | **200**；`Set-Cookie` 含 `pony_sentry_session=`（HttpOnly）；body `authenticated == true` 且 `csrf_token` 非空 | 401 / 无 `Set-Cookie` / csrf 空 |
| **R-T6b** | 同 R-T3（CIDR 不可信） | 无 Cookie `GET /api/v1/auth/session` | **401** `unauthorized`（不可信来源不得被签发会话） | 200 |
| **R-T7** | 同 R-T1；客户端先走 R-T6 取 Cookie+csrf | ① 无会话 Cookie 的 `PATCH /api/v1/issues/:id`　② 有 Cookie 但无 `x-csrf-token`　③ 有 Cookie + 正确 `x-csrf-token` | ① **401** `unauthorized` ② **403** `csrf_invalid` ③ **200**（业务码，非 401/403） | ① 200/204 或 403 ② 200/204 ③ 401/403 |
| **R-T8** | 单元 + 判定层（传输层不参与） | `normalize_peer(::ffff:100.64.0.1)`、`normalize_peer(fd7a:115c:a1e0::1)`；`TrustConfig::parse(true,"100.64.0.0/10,fd7a:115c:a1e0::/48","host")` 后 `decide(Some(::ffff:100.64.0.1), false, Some("host"))` | 前者 **== 100.64.0.1**；IPv6 原样；`decide` **== Trusted** | 返回 IPv6 原值 / OutsideCidrs / HostNotAllowed |
| **R-T8b** | `CONSOLE_TAILNET_TRUST=1`，`CONSOLE_TAILNET_CIDRS="not-a-cidr,100.64.0.0/10"`，`CONSOLE_TRUSTED_HOSTS=127.0.0.1` | 进程启动 → `GET /healthz` → 无凭证 `GET /api/v1/issues` | 进程**未 panic/未退出**；`/healthz` **200**；`/api/v1/issues` **401**（fail-closed + 回退默认 CIDR + 启动 WARN） | 崩溃 / 信任被打开 / 500 |
| **R-T8c** | `CONSOLE_TAILNET_TRUST=1`，`CONSOLE_TAILNET_CIDRS=","`（另跑 `" , "` / 仅逗号空白串 变体），`CONSOLE_TRUSTED_HOSTS=127.0.0.1` | 进程启动 → `GET /healthz` → 无凭证 `GET /api/v1/issues`（来源 127.0.0.1） | 零个有效 CIDR 视为非法配置：进程**存活**；`/healthz` **200**；`/api/v1/issues` **401**（信任关闭 + 回退默认 CIDR）；启动日志含 **WARN**。反例区分：`CONSOLE_TAILNET_CIDRS` **未设置或空串** → 用默认值且**不告警** | 崩溃 / 信任被打开 / 500 / 空串被误判为非法并告警 |

### 3.1 补充冻结点（同属验收面）

| ID | 规则 | 断言 |
| --- | --- | --- |
| **R-T9** | 不可回归 | `tests/` 既有 R10 鉴权用例**一行不改**全绿：不可信来源（不设上述三个 env）的 200/401/403/503 响应码与 body 与冻结前逐字节一致 |
| **R-T10** | 审计日志（ver 3 校正） | 信任放行审计行字段集 = **`scope` / `request_id` / `trust_decision` / `peer` / `host`**（五字段，与实现/README 对齐）；`peer` 为归一化后地址。全量日志内**禁止**出现任何凭证值：`admin_token`/`client_token`/会话 token/`csrf_token`/`Authorization`/`Cookie` |
| **R-T11** | 代理头只拒不放 | `X-Forwarded-For`/`X-Real-IP` 存在时任何来源都不得获得免 token 读权限；实现不得「取首个 hop 当对端」 |
| **R-T12** | 凭证优先 | 携带合法 `ADMIN_TOKEN`（或合法会话 Cookie）的请求，无论 `trust_decision` 为何值都必须 200/正常业务码，不得因代理头/未知对端/Host 不匹配而反向降级 |
| **R-T13** | 双重门槛缺一不可 | Host 命中但 CIDR 不命中 → 401（`OutsideCidrs`）；CIDR 命中但 Host 不命中 → 401（`HostNotAllowed`）；**任一为假都不得放行**（AND 关系）。Host 白名单缺失即视为安全缺陷，评审判 FAIL |

## 4. 与既有契约的边界（非目标）

- 不改 `POST /api/v1/auth/session`（token 换会话）语义；不改 ingest 路由与其跨域层；不改未开启信任时的任何行为。
- 不做「按 XFF 首 hop 取真实 IP」的信任推断；不引入公网可信网段；不把 Host 白名单当鉴权（它只做 rebinding 防护）。
- 不新增 HTTP 状态码：复用 401 `unauthorized` / 403 `csrf_invalid` / 503 `admin_token_not_configured`。

## 5. 机器自检（T1 交付物自身）

```bash
cargo build --offline -p pony-sentry-server                    # Exit 0
cargo clippy --offline -p pony-sentry-server -- -D warnings    # Exit 0（CI 口径）
grep -c "unimplemented!" crates/server/src/trust.rs            # 11 = 10 个函数体 + 1 处模块文档引用
grep -nE "^\s*[^/].*\b(if|for|while|return|match|split|parse::)\b" crates/server/src/trust.rs  # 仅 impl 头命中
```

`lib.rs` 仅新增一行 `pub mod trust;`（**Lead 已追认豁免**：允许的编译衍生写入，T3 不得删除）；接线仍是 **T3 职责**，本模块目前「已编译、未接线」。

## 6. 变更历史

| 版本 | 变更 | 触发 |
| --- | --- | --- |
| ver 1 | 初版 R-T1..R-T8 + R-T9..R-T12（默认开启、无 Host 门） | T1 首轮 |
| ver 2 | 严格默认关；新增 `TrustedHost` 白名单门、`decide(peer,proxied,host)`、`TrustDecision::HostNotAllowed`；R-T5c/5d、R-T13 | Lead 两轮裁决 |
| ver 3 | 新增 R-T4c（`forwarded` 同属代理门）、R-T8c（零个有效 CIDR 降级）；**校正 R-T10 为五字段**（scope/request_id/trust_decision/peer/host）。纯文档修订，代码/测试零改动 | L3A 评审 notes |
