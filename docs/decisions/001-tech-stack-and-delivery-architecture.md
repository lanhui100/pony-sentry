# ADR-001: 核心技术栈选型与单二进制交付架构

**状态**: 已采纳
**日期**: 2026-10-07
**影响范围**: 全局架构、前后端通信、部署模型

## 背景

PonySentry 专为轻量化应用层缺陷收集、堆栈去重和智能体自动化修复闭环而设计。传统 Sentry 依赖庞大的分布式微服务集群（Kafka、ClickHouse、Snuba、Relay、Postgres、Redis 等），极度消耗系统资源（通常需要 >16GB 内存），无法在边缘开发机或个人服务器以 <50MB 资源轻量跑起。我们需要极简单二进制或极轻运维形态，同时提供对 Rust/Tauri/Vue/FastAPI 多端开箱即用的 Ingest、去重聚合和机器 API。

## 决策

1. **核心服务端**：采用 Rust (Axum + Tokio + SQLx + serde + tracing)。
2. **持久化**：采用 SQLite (WAL 模式)，支持纯文件持久化或单二进制嵌入式运行，满足轻量低开销要求。
3. **前端 Web 控制台**：Vue 3 + Vite + TailwindCSS 构建轻量 SPA，编译产物内嵌于 Rust 二进制（通过 `rust-embed` 或静态路由伺服）。
4. **接入协议**：提供极简原生 HTTP POST Ingest 接口（`/api/v1/ingest`）并兼容 Sentry DSN Envelope/Store 规范的核心子集。

## 备选方案

| 方案 | 优点 | 缺点 | 结论 |
|------|------|------|------|
| 官方 Sentry 全家桶 | 功能完备、生态成熟 | 运维极重、资源消耗 >16GB、无法单机轻量自愈 | 未选择 |
| Go + Gin + SQLite | 易上手、二进制较小 | 堆栈脱敏与内存并发安全性略逊于 Rust，缺乏生态统一性 | 未选择 |
| Rust (Axum) + SQLite + Vue 3 | 极致性能、<50MB 内存占用、单二进制交付、强类型保障 | 需要严谨的生命周期与并发架构设计 | 已选择 |

## 理由

Rust Axum + SQLite 能够在毫秒级延迟下并发处理客户端错误收集，内存开销通常低于 30MB，极度契合轻量运维与开发者本地/私有化部署。

## 影响

### 正面影响
- 单一可执行文件交付，支持无需安装任何外部中间件即开即用。
- 极低的内存与 CPU 开销。
- 机器友好 REST API 直接服务于本地 AI Agent 闭环修复。

### 负面影响 / 风险
- 单机 SQLite 面对超大规模（单秒数万次并发打点）写入需受 WAL 与锁模型限制，需在 Ingest 层做异步批量缓冲与限流。

## 相关文档

- 架构蓝图: `docs/architecture/system-design.md`
