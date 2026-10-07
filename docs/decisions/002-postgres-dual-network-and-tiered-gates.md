# ADR-002: PostgreSQL 适配、双网络隔离与三层安全门禁流水线

**状态**: 已采纳
**日期**: 2026-10-07
**影响范围**: 存储抽象层、k3s 网络与 Ingress 拓扑、CI/CD 安全发布

## 背景

PonySentry 初始基于 SQLite 单文件运行。现需在 k3s 集群进行单例生产化部署：
1. **数据存储**：需无缝对接开发环境现有的 PostgreSQL 实例（`dev/job-copilot-postgres`），支持 SQLite/PostgreSQL 双模自适应。
2. **网络隔离与客户端上报**：
   - 客户端（Rust/Tauri/Vue/FastAPI）散落在公网与边缘，需要公网加密入口（HTTPS/TLS）回传错误（`/api/v1/ingest`）；
   - Web 控制台及敏感 Issue 诊断接口只能允许 Tailscale 私网设备（如开发机 `devserver.taildb165c.ts.net`、开发员办公机等）访问，禁止公网裸露。
3. **开源合规与隔离部署流水线**：
   - 代码开源仓库中严禁携带任何真实数据库密码、公网证书或集群 Token；
   - 依据 ponygo 规范建立分层门禁：`pre-commit`（本地低成本语法/格式/脱敏信息拦截）、`pre-push`（单元测试/集成测试/构建验证）、`CI/CD`（自动化门禁 + 镜像构建安全发布）。

## 决策

1. **持久层双模抽象**：
   - 保留 `IssueRepository` trait，增加 `PgIssueRepository` 实现；
   - 根据 `DATABASE_URL` 协议前缀（`sqlite:` 或 `postgres:` / `postgresql:`）自动初始化连接池并执行对应 DDL 迁移；
   - 新增 `migrations/postgres/0001_init.sql`。
2. **k3s 双网络接入拓扑**：
   - **公网加密入口 (Public Ingest)**：通过 Traefik Ingress 暴露独立域名（如 `sentry-ingest.ponyjob.top` 或路径路由），挂载 `letsencrypt-prod` 证书，仅路由 `/api/v1/ingest` 与 `/healthz`；
   - **私网控制台 (Tailscale Private Web)**：通过 Tailscale 内部域名（如 `sentry.local.ponyjob.top` 或基于 Traefik Ingress 的白名单 CIDR 中间件 `100.64.0.0/10`）限制仅 Tailscale 节点可访问 Web UI 及管理 API。
3. **Ponygo 三层门禁与开源安全发布体系**：
   - **Layer 1: Pre-commit 门禁**：本地极速运行，包括 `cargo fmt --check`、基于正则与特定模式的敏感凭据（Token/Secret/Private Key）扫描防泄露检查。
   - **Layer 2: Pre-push 门禁**：本地中成本运行，包含 `cargo check`、`cargo test --workspace` 与 `ponygo status` 机械合规自检。
   - **Layer 3: CI/CD 隔离部署流水线**：GitHub Actions / 自动化脚本执行全量门禁，流水线敏感配置通过 Kubernetes Secret 隔离注入，避免在代码库出现任何物理凭据。

## 备选方案

| 方案 | 优点 | 缺点 | 结论 |
|------|------|------|------|
| 仅限 SQLite 部署 | 零依赖、极轻量 | 跨 Pod 重启与多环境数据共享受限，无法复用已有基础设施 | 未选择 |
| 公网开放全量 Web 与 Ingest | 配置简单 | 内部错误堆栈与敏感 Issue 暴露于公网，存在严重安全隐患 | 未选择 |
| PG 双模 + 双路由隔离 + 三层门禁（选中） | 既能满足公网跨端上报，又能严密防护内部诊断控制台，代码库零泄露 | 需要针对 Traefik 中间件与 SQLx 做针对性配置 | 已选择 |

## 理由

双网络隔离实现了客户端上报的高可用与运维控制台的安全加固；三层门禁保证了代码开源时的零敏感泄露底线。

## 影响

### 正面影响
- 支持生产级 PostgreSQL 并保留 SQLite 嵌入式轻量特性。
- 外部恶意网络无法触及 Web 控制台与管理数据。
- 提交前与推送前机械防线保障代码清洁度与治理合规。

### 负面影响 / 风险
- 需要在 k3s 维护 Traefik 中间件配置及 Cert-Manager 证书生命周期。

## 相关文档

- 系统蓝图: `docs/architecture/system-design.md`
- 决策索引: `docs/decisions/INDEX.md`
