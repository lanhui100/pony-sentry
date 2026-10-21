# Agent Note: wecom notify relay and cluster-mode backup for pg18 host

Status: implemented

## Problem

生产宿主（devserver）的 PonySentry 存在两处治理缺口：

1. **通知缺口**：服务端只有 `WEBHOOK_URL` 旁路，payload 是 `issue.created` /
   `issue.regression` 的语义 JSON（issue + latest_event），无法直接投企业微信机器人
   （payload 与 `msgtype` 结构不同），且 `/etc/pony-sentry/pony-sentry.env` 中该键长期被注释，
   新缺陷只能靠人主动打开控制台才会被发现。
2. **备份缺口**：`deploy/systemd/pony-sentry-backup.{service,timer}` 虽在仓库中，
   但宿主从未安装 timer；同时宿主 `pg_dump 16.15` 无法导出运行在 PG18.3 上的 `ponysentry` 库
   （`aborting because of server version mismatch`），即使装上 timer 也会每日失败。

## Decision

1. **企微通知走本机 relay**：新增 `deploy/scripts/notify-webhook.py`（Python 标准库单文件，
   零第三方依赖）+ `deploy/systemd/pony-sentry-notify.service`，监听 `127.0.0.1:3091`，
   只接受 `POST /webhook`；把 webhook payload 转成企微文本后投递到既有 relay
   （`WX_WORK_EXTERNAL_API_URL` + `X-API-KEY`，复用 job_copilot `alert_notify` 凭据面），
   5s 超时、失败只记 journal 不重试（与 PonySentry webhook 语义一致）。
   `NOTIFY_PROJECTS` 默认只放行 `job_copilot`，避免 ponyllm 等项目的噪声。
   PonySentry 侧只增加 `WEBHOOK_URL=http://127.0.0.1:3091/webhook`，不再改动服务端代码。
2. **备份适配宿主客户端版本**：`deploy/scripts/backup.sh` 新增 `PG_DUMP_MODE=cluster`，
   经 `kubectl exec <db-pod> -- pg_dump -Fc` 在容器内导出（容器自带匹配版本的 pg_dump 18），
   再本地 `age` 加密；`local` 仍为默认，保持其他部署形态零变化。
   `set -euo pipefail` 下 kubectl 失败会传播非零退出并清理半成品，不会产出"看起来成功"的残件。
3. `README.md` 同步两张配置表（备份 + relay），`deploy/systemd/notify.env.example` 提供模板。

## Alternatives considered

- **直接把 `WEBHOOK_URL` 指向企微机器人**：payload 结构不兼容，且无法做项目过滤与
  字段裁剪；改造服务端 webhook 编码会把"通知渠道"焊进核心服务。
- **复用 job_copilot 的 `alert_notify`（k8s 内 FastAPI）**：依赖 Tilt/集群在线，
  PonySentry 是宿主 systemd 单进程，通知链路不应被开发环境生命周期绑定。
- **安装 PostgreSQL PGDG 源升宿主 pg_dump 到 18**：Ubuntu 24.04 默认源无 18，
  引入第三方源与跨版本升级维护面；数据库本身运行在集群内，用集群内客户端是天然匹配。
- **用 docker/podman 跑 pg_dump 镜像**：宿主已有 kubectl 与集群资质，额外镜像与编排无收益。

## Consequences

- 新缺陷（首次创建 / resolved 后回归）可在分钟级触达企微；relay 故障不影响 ingest，
  仅通知丢失（journal 可见 `failed` 计数）。
- 备份恢复出流水线：每日 03:30 timer + 手动 `systemctl start`，产物含 sha256，
  收据见 job_copilot `.dev-team/evidence-pony-sentry-ops/receipt-20261021.md`。
- `PG_DUMP_MODE=cluster` 隐含单副本与固定 Pod 名假设（`PONY_PG_POD`），
  多副本/滚动改名时需同步改 `backup.env`。
- 未接入 ADR-003 的 DSH 自动诊断（`dsh headless`），本次只做触达。
