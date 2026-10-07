# HANDOVER: PonySentry 项目自我实现交接计划

> **注意**：本交接文档仅用于初始化引导。当负责本项目的 Agent 加载并理解本交接文档的所有意图与架构约束后，**请立即删除此文件 (`rm HANDOVER.md`)**，并基于工程规范展开实现。

---

## 1. 项目定位与核心意图

**PonySentry** 是 Pony 家族生态中专为**应用层缺陷追踪、智能体闭环诊断与多端轻量化治理**而生的极简开源服务。

它不是用于替代 Loki/Prometheus/Grafana 这一类集群与基础设施观测栈，而是作为**针对客户端 Bug 收集、堆栈去重与生命周期闭环的专用平台**。

### 核心价值主张
1. **多端开箱即用接入**：
   - 官方首批必须支持主流技术栈的无缝上报接入：
     - **Rust**（原生 panic hook、tracing 错误集成）
     - **Tauri**（桌面端崩溃与 IPC 错误归集，尤其是 Windows 客户端）
     - **Vue / Web SPA**（全局 unhandled rejection、Vue errorHandler）
     - **FastAPI (Python)**（后端异常中间件接入）
2. **面向 AI Agent 的设计（Machine-First API）**：
   - 提供极简、机器友好的 REST API，使 Dev 开发主机上的 Agent 能够：
     - 拉取特定端（如 `windows`）、特定版本未解决的错误列表；
     - 获取清洗后的调用栈、Commit SHA、状态机面包屑（Breadcrumbs）；
     - 认领 Bug (`in_progress`)、修复后通过 API 闭环置为 `resolved`。
3. **人类友好的 Web 控制台**：
   - 内置轻量 Web UI，供工程师直观查看各端 Issues、搜索堆栈、查看错误发生频率趋势、手动修改 Issue 状态。
4. **单二进制极简运维**：
   - 纯 Rust 技术栈（推荐 Axum + SQLite/嵌入式存储或轻量 Postgres），资源占用极低（< 50MB 内存），摆脱官方 Sentry 动辄 16GB 内存及微服务集群的沉重运维包袱。

---

## 2. 技术栈契约

- **后端/核心引擎**：Rust (推荐 Axum / Tokio / SQLx / tracing / serde)
- **前端 Web 界面**：Vue 3 + Vite + TailwindCSS / shadcn-vue（嵌入或反代于 Rust 单二进制中）
- **持久化层**：SQLite（默认单文件开箱即用）+ 可选 PostgreSQL 适配
- **SDK / 接入契约**：
  - 兼容标准 Sentry DSN 或提供极简原生 HTTP POST Ingest 契约（便于各端直接集成）。

---

## 3. 功能特性与模块规划

1. **Ingest Gateway (接收网关)**：
   - 接收来自 Rust/Tauri/Vue/FastAPI 的结构化错误事件；
   - 客户端零信任脱敏管道（路径敏感信息剔除、Token/密码脱敏过滤）。
2. **Fingerprint & Deduplication (指纹计算与聚合)**：
   - 根据调用栈顶帧与错误类型计算 Hash，自动折叠重复错误；
   - 计数器统计（发生次数、受影响客户端数、首次/最后发生时间）。
3. **Issue Lifecycle 状态机**：
   - 状态包含：`unresolved` -> `in_progress` -> `resolved` -> `ignored`。
   - 回退机制：若新版本中同一指纹再度触发，状态自动重开为 `regression`。
4. **Agent-Facing REST API**：
   - `GET /api/v1/issues` (支持 query: status, platform, release)
   - `GET /api/v1/issues/:id` (获取精准堆栈与无敏面包屑)
   - `PATCH /api/v1/issues/:id` (更新状态、分配处理者)
5. **Human Web UI**：
   - Issue 列表、全文搜索、端筛选、错误详情堆栈展示、一键标记状态。

---

## 4. 治理与规范流程（已初始化）

本项目已由 `ponygo init` 注入 L0 治理体系：
- 核心宪法：`.meta/constitution/constitution.md`
- 架构决策目录：`docs/decisions/`
- 工程门禁说明：`.meta/gates/README.md`

### 下一步行动指南
1. 本交接文档阅读完成并确认上下文后，**执行 `rm HANDOVER.md` 删除本文件**。
2. 梳理并在 `docs/decisions/` 沉淀首份架构决策 ADR（选型 Axum、嵌入式 SQLite、前后端集成打包形式）。
3. 初始化 Rust workspace / Cargo.toml 及 Vue 前端工作空间。
4. 推进第一版 Ingest 与 Issue 聚合原型。
