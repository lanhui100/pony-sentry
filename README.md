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
| `ADMIN_TOKEN` | 空（回退 `CLIENT_TOKEN`） | 读接口与 PATCH 凭证：`x-admin-token` 或 `Authorization: Bearer` |
| `WEBHOOK_URL` | 空（关闭通知） | issue 新建/回归时的旁路通知（5s 超时，失败不重试） |
| `RETENTION_DAYS_EVENTS` | `180` | events 保留天数 |
| `RETENTION_DAYS_TRACES` | `90` | traces 保留天数 |
| `RETENTION_DISABLED` | 未设置 | `=1` 关闭每日清理任务 |
| `RETENTION_DRY_RUN` | 未设置 | `=1` 只打印清理计划不删除（首次上线观察用） |
| `PONY_REINDEX_FINGERPRINTS` | 未设置 | `=1` 执行一次全量指纹重索引后退出。**会重塑 issue 分组，仅运维手动执行** |
| `RUST_LOG` | — | tracing 过滤（结构化日志带 `request_id`/`issue_id` 关联字段） |

Web 控制台令牌二选一（构建期或运行时）：

```bash
# 方式 A（构建期，产物内嵌，适合内网自建 console）：构建前导出
VITE_PONY_SENTRY_TOKEN=<admin 或 client token> pnpm build

# 方式 B（运行时，无需重建）：浏览器控制台执行一次
localStorage.setItem('pony_sentry_token', '<admin 或 client token>')
```

生产部署请以普通用户直接执行 `bash deploy/scripts/deploy-console.sh`（不要 `sudo bash`；脚本内部
按需 sudo）——从 `/etc/pony-sentry/pony-sentry.env` 读取 `ADMIN_TOKEN` 烘焙进产物并重装二进制，
幂等可重跑。脚本在 `EXIT` trap 中无条件还原无 token 的 `web/dist`；安装前备份上一版本，
服务/健康/三个鉴权读接口/token 内嵌/文件权限任一自检失败即自动回滚。构建期 `umask 077`
且 build artifact 置 `0600`，内嵌 token 的二进制安装为 `0750 root:<service-group>`，
避免同机其他用户读取。

控制台数据加载失败时会按原因显示可诊断文案：401/403 提示令牌缺失或失效，超时提示网络/服务
响应过慢，网络错误与 5xx 分别提示；不再统一显示无法归因的“数据加载失败”。

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

| 端点 | 无 token | 仅配 `CLIENT_TOKEN` | 配了 `ADMIN_TOKEN` |
| :--- | :--- | :--- | :--- |
| `POST /api/v1/ingest`、`POST /api/v1/traces` | 未配置 token 时放行；配置后 401 | ✅ | ✅ |
| `GET /api/v1/issues*`、`/projects`、`/traces*` | `401` | ✅ | ✅ |
| `PATCH /api/v1/issues/:id`、`/traces/:id` | `401` | ✅ | ✅ |

- 两个 token 都未配置时，读接口与 PATCH **fail-closed** 返回 `503 {"error":"admin_token_not_configured"}`，
  启动时打一条 WARN；写入端点沿用「未配置即放行」的历史语义。
- `/healthz` 与 `/metrics` 不鉴权（探针/抓取面）；`/healthz` 做 DB 探活（2s 超时），
  DB 不通 → `503 {"status":"degraded","db":"down"}`；`/metrics` 暴露
  `pony_sentry_issues_total{status,environment}`、`pony_sentry_events_total`、
  `pony_sentry_ingest_requests_total{result}`、`pony_sentry_ingest_level_degraded_total`。

**retention（契约 R11.1）**：进程启动 30s 后首轮，此后每 24h 分批（5000 行/批，批间释放事务）
删除过期 `events`/`traces`；`issues` 聚合行永久保留、`count` 不回退。

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
  Web 控制台同样只走 tailnet。
- **单副本**：无多副本 HA / leader 选举；retention 与指纹重索引假设单进程运行。
  `migrate()` 也按单进程启动设计：多副本并发首次迁移可能撞 PG 目录表竞态
  （`CREATE TABLE IF NOT EXISTS` 不是原子 DDL），多副本场景需先加 advisory lock。
- **备份**：`pg_dump -Fc | age`，每日 03:30 由 timer 触发；恢复演练（`age -d` + `pg_restore`）
  需人工按 runbook 执行，未纳入自动化。
