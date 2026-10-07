# PonySentry 系统架构蓝图 (System Blueprint)

**状态**: 提案中  
**日期**: 2026-10-07  
**架构负责人**: Team Lead  
**关联版本/里程碑**: MVP / v0.1.0  

---

## 1. 目标与产品边界 (Scope & Non-Goals)

### 核心交付目标 (Goals)
- **多端轻量上报网关 (Ingest Gateway)**：提供极简 HTTP Ingest 端点 (`/api/v1/ingest`)，接收来自 Rust、Tauri、Vue、FastAPI 等多端结构化错误报告，支持客户端信息、堆栈与面包屑收集。
- **敏感数据零信任脱敏管道 (Sanitization Pipeline)**：在服务入库前自动对路径中个人用户名/敏感路径以及环境变量、请求头中的 Token/Password 进行掩码与清洗。
- **智能指纹计算与聚合引擎 (Fingerprinting & Deduplication)**：基于异常类型、顶层核心栈帧（剔除外部依赖/行号变动干扰）生成哈希指纹，自动归并相同 Issue 并累计发生频次与时间窗口。
- **完整缺陷生命周期状态机 (Issue Lifecycle Machine)**：支持 `unresolved` -> `in_progress` -> `resolved` -> `ignored` 状态迁移；支持已解决 Issue 在新版本中再度复现时自动触发 `regression` 回退重开。
- **面向 AI Agent 的 Machine-First API**：提供易于自动化集成的 REST API（支持按状态/平台/版本筛选错误、获取清洗后栈帧面包屑、认领与标记修复闭环）。
- **极简 Human Web UI 控制台**：单页面展示 Issue 列表、多维筛选、详情展开、堆栈查看与状态快捷操作。
- **低资源单二进制交付**：Rust (Axum + SQLite WAL)，整机常驻内存 < 50MB。

### 明确非目标 (Non-Goals)
- **非指标/日志基础设施**：不替代 Prometheus/Loki/Grafana 等大规模 APM 与集群监控系统。
- **非分布式微服务架构**：不做跨节点分布式 Kafka/ClickHouse 集群，保持单机/嵌入式 SQLite 极简运维。
- **不实现复杂全量 Sentry 协议矩阵**：首发仅聚焦崩溃与异常 Ingest，不实现复杂的 Performance APM、Replay 录屏或 Cron Monitors。

---

## 2. 系统拓扑与模块契约 (Topology & Contracts)

### 系统拓扑

```
[ Clients / SDKs ]
  - Rust (tracing/panic)
  - Tauri (desktop)
  - Vue (frontend)
  - FastAPI (backend)
         │  POST /api/v1/ingest
         ▼
┌────────────────────────────────────────────────────────┐
│                      PonySentry                        │
│ ┌────────────────────────────────────────────────────┐ │
│ │ Ingest & Sanitizer Layer                           │ │
│ │ (Token Masking, Path Redaction, Payload Normalize) │ │
│ └────────────────────────┬───────────────────────────┘ │
│                          ▼                             │
│ ┌────────────────────────────────────────────────────┐ │
│ │ Fingerprint & Deduplication Engine                 │ │
│ │ (Stack Hashing, Grouping, Frequency Counter)       │ │
│ └────────────────────────┬───────────────────────────┘ │
│                          ▼                             │
│ ┌────────────────────────────────────────────────────┐ │
│ │ Issue Lifecycle & Storage Engine (SQLite WAL)      │ │
│ │ (State Machine, Regression Auto-Trigger, Events)   │ │
│ └────────────────────────┬───────────────────────────┘ │
│                          │                             │
│         ┌────────────────┴────────────────┐            │
│         ▼                                 ▼            │
│ ┌──────────────────────┐       ┌─────────────────────┐ │
│ │ Agent-First REST API │       │ Embedded Web UI     │ │
│ │ (JSON / Machine-Op)  │       │ (Vue 3 Static SPA)  │ │
│ └──────────────────────┘       └─────────────────────┘ │
└────────────────────────────────────────────────────────┘
```

### 核心模块职责与边界

- **模块 A: `ingest` (接收与脱敏模块)**
  - 职责：校验 Ingest Payload，执行零信任敏感字段清洗，输出规范化的 `NormalizedEvent`。
  - 公开接口：`fn process_incoming_event(raw: RawEvent) -> Result<NormalizedEvent, IngestError>`。
- **模块 B: `fingerprint` (指纹与聚合模块)**
  - 职责：提取核心特征（error type, value, sanitized top frame），计算稳定的 SHA-256 指纹。
  - 公开接口：`fn compute_fingerprint(event: &NormalizedEvent) -> String`。
- **模块 C: `storage & lifecycle` (存储与状态机模块)**
  - 职责：管理 Issue 与 Event 的持久化（SQLite），驱动 Issue 状态转移与 Regression 检测。
  - 公开接口：`IssueRepository`、`LifecycleEngine`。
- **模块 D: `api & web` (对外服务与 Web 控制台)**
  - 职责：提供 Axum HTTP 路由、RESTful Agent API、嵌入式 Web UI 资源静态分发。

---

## 3. 核心数据模型与状态流转 (Data & State)

### 核心实体/Schema

1. **Issue 实体 (`issues` 表)**
   - `id`: TEXT (UUID 或短哈希, 主键)
   - `fingerprint`: TEXT (唯一指纹索引)
   - `title`: TEXT (错误名称/摘要)
   - `culprit`: TEXT (核心出错函数或顶层栈帧)
   - `platform`: TEXT (`rust` | `tauri` | `vue` | `python` | `other`)
   - `status`: TEXT (`unresolved` | `in_progress` | `resolved` | `ignored` | `regression`)
   - `assigned_to`: TEXT (可选处理者，如 `agent-coder-01`)
   - `count`: INTEGER (累计触发次数)
   - `last_release`: TEXT (最近触发版本)
   - `first_seen_at`: DATETIME
   - `last_seen_at`: DATETIME

2. **Event 实体 (`events` 表)**
   - `id`: TEXT (主键)
   - `issue_id`: TEXT (外键关联 issues.id)
   - `payload`: JSON (清洗后的完整堆栈、环境上下文、breadcrumbs)
   - `release`: TEXT
   - `environment`: TEXT
   - `created_at`: DATETIME

### 关键状态机流转

```
                ┌──────────────────────────────────────┐
                │                                      │
                ▼                                      │
          [unresolved] ──(claim)──> [in_progress]      │
                │                         │            │
                │                         │ (resolve)  │
                ▼                         ▼            │ (new event with
            [ignored]                 [resolved]       │  newer release)
                                          │            │
                                          └──(re-hit)──┘
                                                 │
                                                 ▼
                                           [regression]
```

- 当接收到已知指纹的事件时：
  - 若当前状态为 `resolved` 且该事件的 release 版本大于等于 resolve 时的标记版本，状态自动转为 `regression`；
  - 否则仅递增 `count` 并更新 `last_seen_at`。

---

## 4. 关键技术选型与 ADR 索引 (Decisions)

| 领域 | 选型结果 | 核心考量 | 对应决策记录 (ADR) |
| :--- | :--- | :--- | :--- |
| 服务端框架 | Rust Axum + Tokio | 高性能、轻量低开销、类型安全与异步生态完备 | [ADR-001](../decisions/001-tech-stack-and-delivery-architecture.md) |
| 存储引擎 | SQLite (WAL) / PostgreSQL 双模 | 单文件轻量运行与集群化生产级持久化兼具 | [ADR-001](../decisions/001-tech-stack-and-delivery-architecture.md), [ADR-002](../decisions/002-postgres-dual-network-and-tiered-gates.md) |
| 网络隔离拓扑 | 公网 Ingest + Tailscale 私网 UI | 兼顾客户端跨网络上报与内部控制台高安全性 | [ADR-002](../decisions/002-postgres-dual-network-and-tiered-gates.md) |
| 交付质量治理 | 三层门禁 (pre-commit/pre-push/CI) | 防敏感信息泄露、保证开源代码与发布流水线安全隔离 | [ADR-002](../decisions/002-postgres-dual-network-and-tiered-gates.md) |
| 控制台技术 | Vue 3 + TailwindCSS | 组件化轻量控制台，SPA 可直接编译嵌入 Rust 二进制 | [ADR-001](../decisions/001-tech-stack-and-delivery-architecture.md) |

---

## 5. 任务分解映射 (Backlog Decomposition Map)

将本蓝图切割为一组独立可验证、向 `dev-backlog` 注册的事项：

| 阶段 / 事项 ID | 任务名称 | 范围契约与交付标准 | 依赖关系 |
| :--- | :--- | :--- | :--- |
| **Stage 1 (B001)** | 工程骨架与数据存储层 (Core & Storage) | 建立 Rust workspace、SQLite 迁移与基础 Issue/Event CRUD 模型与状态机 | 无 |
| **Stage 2 (B002)** | Ingest网关、脱敏管道与指纹聚合引擎 (Ingest & Aggregation) | 实现零信任敏感数据脱敏、指纹提取算法、去重聚合与 Regression 自动触发 | 依赖 B001 |
| **Stage 3 (B003)** | Agent-Facing REST API 与多端 SDK 契约 (APIs & Client Contracts) | 暴露 Ingest 与 Issue 管理 REST 接口，验证多端模拟 Payload 接入与 Agent 操作链路 | 依赖 B002 |
| **Stage 4 (B004)** | 嵌入式 Web 控制台与单二进制交付集成 (Web UI & Packaging) | 实现 Vue 3 极简管理界面，与后端打通并完成集成冒烟交付 | 依赖 B003 |
