# PonySentry

PonySentry 是自建的异常与 Agent 追踪控制台：单一 Rust 二进制同时提供 ingest API、
管理 API 与内嵌 Web 控制台（`web/dist/index.html` 通过 `include_str!` 编入二进制）。

- 决策记录（ADR）：`.agents/notes/`（写入规范见 `.agents/notes/README.md`）
- 文档标准与家分类：`docs/AGENTS.md`
- 本 README：人读契约（config / semantics / limitations）

## 快速开始

```bash
cd web && pnpm install && pnpm build && cd ..   # 先产出 web/dist（include_str! 依赖它）
cargo build --workspace
DATABASE_URL="sqlite:/tmp/ponysentry.db?mode=rwc" PORT=3000 \
  CLIENT_TOKEN=dev-client ADMIN_TOKEN=dev-admin ./target/debug/pony-sentry-server
```

启动即执行 `migrations/`（SQLite）或 `migrations/postgres/`（PG）下的全部迁移；
迁移失败即拒绝启动（`migrate()` 在启动路径内）。

## config

| 环境变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `DATABASE_URL` | `sqlite:ponysentry.db?mode=rwc` | `postgres://`/`postgresql://` 前缀切 PG，否则 SQLite |
| `PORT` | `3000` | 监听端口 |
| `CLIENT_TOKEN` | 空（关闭写入鉴权） | 写入端点凭证：`x-client-token` 或 `Authorization: Bearer` |
| `ADMIN_TOKEN` | 空（回退 `CLIENT_TOKEN`） | 读接口与 PATCH 的静态 API 凭证；也是控制台登录时交换短期会话的凭据 |
| `CONSOLE_SESSION_TTL_SECS` | `28800` | 控制台服务端会话有效期（秒，最小 60、最大 86400；重启即全部失效） |
| `CONSOLE_COOKIE_SECURE` | 未设置 | `=1` 时会话 Cookie 加 `Secure`；HTTPS 反向代理下应开启，纯 tailnet HTTP 保持关闭 |
| `CONSOLE_TAILNET_TRUST` | 未设置（关闭） | tailnet 可信对端免 token 的**唯一开关**：只有 `1`/`true`/`on`（大小写不敏感）开启；未设置或其它任何值一律关闭 |
| `CONSOLE_TAILNET_CIDRS` | `100.64.0.0/10,fd7a:115c:a1e0::/48` | 可信对端网段（逗号分隔 CIDR，IPv4/IPv6）；未设置用默认（Tailscale CGNAT + ULA）；含任一非法段 → 不崩溃、信任关闭 + 回退默认 + 启动 WARN |
| `CONSOLE_TRUSTED_HOSTS` | 空（无白名单） | Host 白名单（逗号分隔 `host` / `host:port`，比较忽略大小写与端口）；**空 = 信任恒不生效**（fail-closed）+ 开启状态下启动 WARN |
| `WEBHOOK_URL` | 空（关闭通知） | issue 新建/回归时的旁路通知（5s 超时，失败不重试） |
| `RETENTION_DAYS_EVENTS` | `180` | events 保留天数 |
| `RETENTION_DAYS_TRACES` | `90` | traces 保留天数 |
| `METRICS_TOKEN` | 空（不鉴权） | 设置后 `/metrics` 需 `x-metrics-token` 或 `Authorization: Bearer`；空值保持探针免鉴权 |
| `RETENTION_DISABLED` | 未设置 | `=1` 关闭每日清理任务 |
| `RETENTION_DRY_RUN` | 未设置 | `=1` 只打印清理计划不删除（首次上线观察用） |
| `PONY_REINDEX_FINGERPRINTS` | 未设置 | `=1` 执行一次全量指纹重索引后退出。**会重塑 issue 分组，仅运维手动执行** |
| `PONY_TRACE_INGEST_ENABLED` | `1`（开启） | tracing 接收开关（kill switch）：`0`/`false`/`off`（大小写不敏感、去空白）关闭后，`POST /api/v1/traces`、`GET /api/v1/traces`、`GET /api/v1/traces/:id`、`PATCH /api/v1/traces/:id` 一律返回 `410 Gone` + `{"error":"trace_ingest_disabled","message":"tracing moved to tempo+langfuse"}`，且在处理器之前拦截（不写库、不递增 trace 指标）；`/healthz`、`POST /api/v1/ingest`、`GET/PATCH /api/v1/issues*`、`/metrics` 行为不变。未设置/空值/其它值 = 开启 |
| `RUST_LOG` | — | tracing 过滤（结构化日志带 `request_id`/`issue_id` 关联字段） |

### Web 控制台会话

Web 产物**不再内嵌 ADMIN_TOKEN**，也不使用 localStorage 保存静态令牌。打开控制台后输入
`ADMIN_TOKEN`（或 `CLIENT_TOKEN`）登录：

1. `POST /api/v1/auth/session` 用静态 token 交换短期会话；服务端返回
   `HttpOnly; SameSite=Strict` Cookie，并只在响应体中返回 CSRF token。
2. `GET /api/v1/auth/session` 恢复页面刷新后的会话状态与 CSRF token。
3. Cookie 会话执行 PATCH 时必须带 `x-csrf-token`；静态 `x-admin-token` / Bearer API 客户端
   语义保持兼容，不走 CSRF 分支。
4. `DELETE /api/v1/auth/session` 注销并立即失效服务端会话；服务重启也会清空全部会话。

会话存储在进程内存中，当前部署为单副本；若未来多副本，需要改为共享会话存储。

生产部署请以普通用户直接执行 `bash deploy/scripts/deploy-console.sh`（不要 `sudo bash`；脚本内部
按需 sudo）——脚本从 `/etc/pony-sentry/pony-sentry.env` 读取 ADMIN_TOKEN，仅用于部署后的
登录冒烟，不写进 web 产物或二进制。部署自检覆盖：健康、匿名请求拒绝、登录换取 Cookie、
三个读接口、Cookie PATCH 缺 CSRF 必须 403、产物不包含 ADMIN_TOKEN、注销后旧 Cookie 失效。
脚本在 `EXIT` trap 中无条件还原 `web/dist`；安装前备份上一版本，任一步失败自动回滚。

控制台数据加载失败时会按原因显示可诊断文案：会话失效提示重新登录，超时提示网络/服务
响应过慢，网络错误与 5xx 分别提示；不再统一显示无法归因的“数据加载失败”。

前端读超时由 `web/src/lib/apiTimeout.js` 的 `API_TIMEOUT_MS`（8000ms）唯一决定，**必须严格大于**
服务端连接池 `acquire_timeout`（3000ms）再加 2000ms 余量：否则 DB 不可达时浏览器会先 abort，
把服务端 3s 后返回的 500 误报成「请求超时」（2026-10-23 生产误判 12 小时的事故根因）。
不变量的冻结契约与机器判据见 `.dev-team/contract-console-timeout.md`（R1–R8，`pnpm test` 中的
`api-timeout-*.test.js` 守卫）；任一侧调值都必须重走契约冻结。

备份（`deploy/systemd/pony-sentry-backup.{service,timer}` + `deploy/scripts/backup.sh`）：

| 环境变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `DATABASE_URL` | 必填 | 与主服务同源（`/etc/pony-sentry/pony-sentry.env`） |
| `AGE_RECIPIENT` | 必填 | age 收件人公钥；私钥离线保管 |
| `BACKUP_DIR` | `/var/backups/pony-sentry` | 备份目录，自动创建（750） |
| `BACKUP_RETENTION_DAYS` | `14` | 按 mtime 清理更早的备份与校验和 |
| `PG_DUMP_MODE` | `local` | `cluster` 时经 `kubectl exec` 在数据库 Pod 内执行 `pg_dump`；宿主 `pg_dump` 主版本低于服务端时（如宿主 16 / 服务端 18）为唯一可行路径 |
| `PONY_PG_NAMESPACE` / `PONY_PG_POD` / `PONY_PG_DB` / `PONY_PG_USER` | — | 仅 `PG_DUMP_MODE=cluster` 必填；`KUBECONFIG` 由 `backup.env` 提供 |

通知 relay（`deploy/systemd/pony-sentry-notify.service` + `deploy/scripts/notify-webhook.py`）：
把 `WEBHOOK_URL` 指向 `http://127.0.0.1:<NOTIFY_PORT>/webhook`，relay 将 `issue.created` /
`issue.regression` payload 转为企微文本投递（仅回环监听，失败只记 journal 不重试）。

| 环境变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `WX_WORK_EXTERNAL_API_URL` / `WX_WORK_EXTERNAL_API_KEY` | 必填 | 企微 relay 地址与 `X-API-KEY` |
| `WX_WORK_USER_ID` / `MY_USER_ID` | 空 | 通知接收人（中继 payload `to_user`）；为空则只发内容 |
| `NOTIFY_PORT` | `3091` | relay 监听端口（`3090` 在 devserver 已被其它服务占用） |
| `NOTIFY_PROJECTS` | 空（全部项目） | 逗号分隔的项目白名单，如 `job_copilot` |

## semantics

**幂等（契约 R4）**：`event_id`（客户端的确定性幂等键）落到 `events.event_key`，全局唯一。

| 场景 | 状态码 | body |
| :--- | :--- | :--- |
| 首次写入（无 `event_id`，或该键首次出现） | `201` | `{"result":"created","status":"<issue 状态>","issue_id":…,"event_id":…,"duplicate":false}` |
| 同 `event_key` 重复投递 | `200` | `{"result":"duplicate",…,"duplicate":true}` |
| 无/错 token（配置了 token 时） | `401` | 文案不作契约约束 |
| `event_id` >64/非可打印 ASCII、`timestamp` 非 ISO8601 | `422` | `{"error":"invalid_field","field":"<name>"}` |
| DB 故障 | `500` | `{"error":"persistence_error"}` |

- `status` 始终是 **issue 状态**（`unresolved|in_progress|resolved|ignored|regression`），
  与 `result`（`created|duplicate`）正交，不得混用。
- duplicate 时**不递增** `issues.count`、**不推进** `issues.last_seen_at`、**不新增** events 行，
  也不触发 Webhook 通知。并发同 `event_key` 由唯一索引兜底并归一为 duplicate（不返回 5xx）。
- 响应中的 `event_id` 回显客户端值；无 `event_id` 时回退服务端事件 UUID。

**指纹 v3（契约 R6）**：
`hex(SHA-256(platform ‖ 0x1F ‖ error_type ‖ [0x1F ‖ error_kind] ‖ [0x1F ‖ model] ‖ [0x1F ‖ culprit] ‖ 0x1F ‖ environment))`。
可选组件缺失时连分隔符一起跳过（与 v2 一致），`environment` 恒参与（缺省 `unknown`，小写归一），
因此**同一签名在不同环境形成两条独立 issue**。历史 issue 的 `fingerprint` 保持 v2 不改写。

**字段归一（契约 R2）**：超长字段截断（`message` 2048 字符；`namespace/app/component/service_name` 128；
`error_type` 256；`source_kind` 64；`grafana_url/loki_query` 1024；`tags` 键 64/值 512/最多 32 项；
`extra` 序列化 >8KB 时替换为 `{"_truncated":true,...}`）；非法 `level` 降级为 `warning` 并计数（WARN 日志）；
`grafana_url` 仅接受 `http`/`https`，`loki_query` 必须 `{...}` 且不含 `|`，不合法即丢弃字段 + WARN（不拒绝事件）。

**stacktrace 双形态（契约 R3）**：`exception.stacktrace` 可为 `Frame[]` 或字符串。
字符串形态会截断到 5000 字符存入 `extra.stacktrace_text`，并尽力解析首帧
（`path:line[:col] in func` 或 Python `File "x", line N, in f`）生成单元素 `Frame[]` 供 culprit 与指纹使用；
解析失败不报错。

**鉴权矩阵（契约 R10）**：

| 端点 | 无凭证 | 仅配 `CLIENT_TOKEN` | 配了 `ADMIN_TOKEN` |
| :--- | :--- | :--- | :--- |
| `POST /api/v1/ingest`、`POST /api/v1/traces` | 未配置 token 时放行；配置后 401 | ✅ | ✅ |
| `GET /api/v1/issues*`、`/projects`、`/traces*` | `401` | ✅ 静态 token 或会话 Cookie | ✅ 静态 token 或会话 Cookie |
| `PATCH /api/v1/issues/:id`、`/traces/:id` | `401` | ✅ 静态 token；Cookie 会话需 CSRF | ✅ 静态 token；Cookie 会话需 CSRF |
| `POST/GET/DELETE /api/v1/auth/session` | 登录/查询/注销会话 | 登录可用 CLIENT_TOKEN | 登录可用 ADMIN_TOKEN |

- 两个 token 都未配置时，读接口与 PATCH **fail-closed** 返回 `503 {"error":"admin_token_not_configured"}`，
  启动时打一条 WARN；写入端点沿用「未配置即放行」的历史语义。
- 静态 token 请求头为 `x-admin-token` / `x-client-token` / `Authorization: Bearer`；控制台 Cookie 会话的
  PATCH 必须额外携带 `x-csrf-token`，缺失或不匹配返回 `403 {"error":"csrf_invalid"}`。

**Tailnet 可信对端（契约 `.dev-team/contract-tailnet-trust.md`，默认关闭）**：开启后来自 tailnet 的对端
**只免 admin token 的只读请求**——`GET`/`HEAD` 管理路由直接放行（`OPTIONS` 不在放行面内）；**变更请求（PATCH/POST/DELETE）
仍必须持有会话 Cookie 并携带匹配的 `x-csrf-token`**，否则 401 / 403 `csrf_invalid`（信任不兑换写权限）。
放行需同时满足三个条件，缺一即 401：① 对端 IP 命中 `CONSOLE_TAILNET_CIDRS` ② `Host` 精确命中
`CONSOLE_TRUSTED_HOSTS`（**Host 白名单是 DNS rebinding 的唯一防线**，`Host: evil.example:3000` 即使 CIDR
命中也不放行）③ 请求不带 `x-forwarded-for`/`x-real-ip`/`forwarded`（代理头只拒不放，不取首 hop 推断对端）。
可信对端 `GET /api/v1/auth/session` 在无可验证会话时自动签发会话（同形 JSON + `Set-Cookie`），
控制台因此零输入进入；不可信来源一律 401。每次信任放行写一条结构化审计日志
（含 `request_id`/`trust_decision`/`peer`/`host` 四个字段，不含任何 token/CSRF/Cookie 值；
`request_id` 供同一请求的审计行关联）。

**残余风险（信任自动签发会话）**：
- **会话累积**：每次无 Cookie 的可信请求都会现场签发新会话；脚本轮询、多标签页刷新等重复/并发无 Cookie
  请求会持续累积会话（单进程内存表，上限 512 条）。
- **可能踢掉在用会话**：会话表满 512 条时先清理已过期，仍满则**只**驱逐 `TrustedPeer`（可信自动签发）来源中
  最早过期者——**手动 token 会话不会被无 Cookie 的可信流量驱逐**；仅当表内全部是手动会话时才回退驱逐最早过期者。
  被驱逐的可信会话影响仅限于需重新签发（可恢复），**不构成越权**——可信对端本身已等价管理员，
  重新 `GET /api/v1/auth/session` 即拿回同权限会话。
- **真正吊销**：`DELETE /api/v1/auth/session` 只清当前 Cookie，对可信对端没有阻断力（再次 GET 会重新签发）；
  要真正吊销读权限请置 `CONSOLE_TAILNET_TRUST=0`（kill switch）或收紧 tailnet ACL / `CONSOLE_TAILNET_CIDRS`。
- `/healthz` 与 `/metrics` 不鉴权（探针/抓取面）；`/healthz` 做 DB 探活（2s 超时），
  DB 不通 → `503 {"status":"degraded","db":"down"}`；`/metrics` 暴露
  `pony_sentry_issues_total{status,environment}`、`pony_sentry_events_total`、
  `pony_sentry_ingest_requests_total{result}`、`pony_sentry_ingest_level_degraded_total`，
  以及 trace 面四个指标：`pony_sentry_traces_total{project,turn_status}`、
  `pony_sentry_trace_turns_total{status}`、`pony_sentry_trace_status_conflicts_total`、
  `pony_sentry_trace_ingest_rejected_total{reason}`。

**retention（契约 R11.1）**：进程启动 30s 后首轮，此后每 24h 分批（5000 行/批，批间释放事务）
删除过期 `events`/`traces`；`issues` 聚合行永久保留、`count` 不回退。

## trace（Agent 轨迹）

`POST /api/v1/traces`（`x-client-token`）写入；`GET /api/v1/traces[/:id]`、`PATCH`（admin）读取与评审批注。
**字段/合并/诊断/指标的唯一契约在 [`docs/trace-contract.md`](docs/trace-contract.md)**，这里只列三个最容易踩的语义：

- **同 `(session_id, turn_id)` 增量合并，终态单调**：非终态→终态可覆盖，终态不会被迟到的非终态回退；
  重复上报不会丢 `error`/`tool_calls` 的迟到补全。
- **totals 是派生值**：`total_*` 由轮次求和，客户端上报值不落库；`payload.eval_status` 不再存在（列是唯一真源）。
- **服务端诊断**：`turns[].diagnostics` 标注 `error_contract:"missing"` / `status_conflict` / `error_missing`，
  控制台据此红/黄标；诊断在脱敏之后生成，不携带原始错误文本。

`traces` 每 `session_id` 一行（聚合视图），`trace_turns` 每轮次一行（权威存储）；并发上报由
PG `pg_advisory_xact_lock` / SQLite `BEGIN IMMEDIATE` 串行化。指标见 `/metrics` 的 `pony_sentry_trace*`。

## limitations

- **历史 v2 issue 分组冻结**：v3 指纹含 `environment`，新事件不会与历史（v2 指纹）issue 合并，
  历史 issue 计数停止增长、新旧并存。这是"不重索引"的既定代价；需要合并时只能用
  `PONY_REINDEX_FINGERPRINTS=1`（会重塑分组，仅运维手动执行）。
- **SQLite 的唯一约束是 `UNIQUE(fingerprint)` + 冗余 `UNIQUE(fingerprint, environment)` 索引**：
  SQLite 无法 DROP 0001 的内联唯一约束。因 v3 指纹已含环境分量，业务断言（跨环境拆成两条 issue）
  在两种后端上一致；直接绕过 v3 写入"同指纹不同环境"会被 SQLite 拒绝。
- **`error_type` 是派生视图字段**：`GET /api/v1/issues/:id` 从该 issue 最近一条 event 载荷派生
  （`payload.error_type` → `payload.exception.error_type`），不落 issues 表；列表接口不返回该字段。
- **`service_name` / `grafana_url` / `loki_query` 只随事件载荷落库**，不是 issue 维度，不能用于筛选。
- **自由文本脱敏沿既有管道**：字符串 stacktrace 走与 message 相同的正则脱敏
  （Bearer/JWT、`api_key=`/`password=`/`secret=` 形式、用户主目录路径），
  不覆盖任意 `keyword=value` 组合；命中不了的自定义密钥仍需上报侧先行脱敏。
- **公网暴露面**：`deploy/k3s/app.yaml` 的公网 Ingress 只放行 `POST /api/v1/ingest`（Exact）与
  `/healthz`；`/api/v1/traces` 与全部读接口仅集群内 svc / tailnet 可达，公网访问返回 404。
  Web 控制台同样只走 tailnet。**管理路由（`/api/v1/issues*`、`/api/v1/projects`、`/api/v1/traces*`、
  `/api/v1/auth/session`）不得加任何公网 Ingress**——尤其开启 tailnet 信任后，公网入口会把「免 token 读」
  直接暴露给外网。
- **单副本**：无多副本 HA / leader 选举；retention、指纹重索引与控制台会话都假设单进程运行。
  控制台会话存在进程内存中，服务重启即全部失效；多副本需改为共享会话存储并加过期清理。
  `migrate()` 也按单进程启动设计：多副本并发首次迁移可能撞 PG 目录表竞态
  （`CREATE TABLE IF NOT EXISTS` 不是原子 DDL），多副本场景需先加 advisory lock。
- **备份**：`pg_dump -Fc | age`，每日 03:30 由 timer 触发；恢复演练（`age -d` + `pg_restore`）
  需人工按 runbook 执行，未纳入自动化。
