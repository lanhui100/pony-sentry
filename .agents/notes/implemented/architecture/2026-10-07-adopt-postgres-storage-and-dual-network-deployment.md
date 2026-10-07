# Agent Note: Adopt PostgreSQL Storage and Dual-Network Deployment

Status: implemented

## Problem

PonySentry 需在 k3s 生产集群作为单例服务运行，复用现有的 PostgreSQL 实例，并满足两层网络安全诉求：公网加密上报 (Ingest) 与 Tailscale 私网限定访问 (Web UI)。同时，开源项目需要一套三层渐进门禁与脱敏防线。

## Decision

1. 扩展数据抽象层以原生支持 PostgreSQL，根据 `DATABASE_URL` 动态选择 SQLite 或 Postgres 仓储。
2. 在 k3s 中部署单例服务，通过 Traefik Ingress 分离公网加密 Ingest 路由与绑定 Tailscale 白名单/私有域名的 Web UI。
3. 建立三层门禁脚本（`pre-commit` 轻量脱敏与格式、`pre-push` 测试构建与 ponygo 状态、`CI` 隔离镜像构建与自动化校验）。

## Alternatives considered

- **仅使用 SQLite 挂载 PersistentVolume**：增加跨节点状态绑定的运维复杂性，且无法与已有数据系统统一备份治理。
- **全量暴露 Web 控制台到公网**：存在未授权扫描与敏感崩溃堆栈信息暴露的巨大风险。

## Consequences

- 支持双模存储，增强生产可靠性；
- 杜绝控制台公网直接裸露风险；
- 提交前与推送前门禁阻止任何真实密码或未脱敏数据进版本库。
