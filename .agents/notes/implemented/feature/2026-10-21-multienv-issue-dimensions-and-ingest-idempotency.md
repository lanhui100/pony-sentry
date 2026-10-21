# Agent Note: add environment dimension and deterministic ingest idempotency

Status: implemented

## Problem

PonySentry 只有一个来源维度 `platform`，issue 的唯一键是 `fingerprint`（`migrations/0001_init.sql:4`）。
迁移目标是把 job_copilot 的智能体问题上报从自建治理中心迁到 PonySentry，且三环境（dev/preprod/production）
共用一个实例。在现状下有两类硬冲突：

1. **环境混同**：v2 指纹不含环境分量（`crates/fingerprint/src/hasher.rs:53-81`），
   dev 与 production 的同签名异常会并入同一只 issue，`count` 与 `last_seen_at` 互相污染，
   排查时无法判断"这个问题只在预发布出现还是生产也在发生"。
2. **投递不幂等**：治理中心的 outbox worker 会重试投递（200/201/409 都算成功，见
   `.dev-team/contracts-pony-sentry-migration.md` R4.3），而 ingest 每次写入都无条件 `count+1`
   并插一条 events 行（`crates/core/src/sqlite_repository.rs` 原 `record_event_and_upsert_issue`）。
   重试即重复计数，告警噪声与真实频次不可分辨。

同时，读接口与 PATCH 长期无鉴权（`crates/server/src/routes.rs` 原 `admin_routes` 无 token 校验），
公网 Ingress 还暴露了 `/api/v1/traces`（`deploy/k3s/app.yaml`），而 `/healthz` 只返回常量 `OK`、
没有任何保留期治理与 `/metrics`。

## Decision

以多环境维度 + 确定性幂等键为一等能力，改动集中在 `migrations/`、`crates/{core,ingest,fingerprint,server}`、
`web/` 与 `deploy/`：

1. **迁移双份**：`migrations/0004_add_issue_dimensions.sql`（SQLite）与
   `migrations/postgres/0004_add_issue_dimensions.sql`（PG）。issues 增 `environment/app/namespace/component/level/source_kind`，
   events 增 `event_key/level` 并建 `WHERE event_key IS NOT NULL` 的部分唯一索引。
   SQLite 无 `DROP CONSTRAINT`，0001 的内联 `UNIQUE(fingerprint)` 不可移除，故**保留**它并另加
   `uq_issues_fingerprint_env` 冗余唯一索引——因 v3 指纹已含环境分量，
   `UNIQUE(fingerprint) ⟹ UNIQUE(fingerprint, environment)`，两种后端的业务行为一致。
   `ADD COLUMN` 无 `IF NOT EXISTS`，幂等守卫写在 `SqliteIssueRepository::migrate()`（列探测 + 单事务整批执行，
   任一句失败整体回滚），PG 侧则全用 `IF NOT EXISTS` / `DROP … IF EXISTS` 逐次重放。
2. **历史环境回填**：迁移内 `UPDATE issues SET environment = COALESCE(最近一条 events.environment, 'unknown')`；
   **历史 issue 的 fingerprint 保持 v2 不改写**，重索引仍只走运维手动开关 `PONY_REINDEX_FINGERPRINTS=1`。
3. **指纹 v3**（`FingerprintEngine::compute_fingerprint_v3`）：沿用 v2 的"缺失组件连分隔符一起跳过"规则，
   末尾追加 `0x1F ‖ environment`（缺省 `unknown`，小写归一）。v2 保留供历史重索引回放。
4. **幂等键**：ingest 一级字段 `event_id`（≤64 可打印 ASCII，越界 422 而非截断）落 `events.event_key`。
   仓库层先查键、命中即返回既有 issue/event 且**不**动 `count`/`last_seen_at`/events 行；
   并发同键由唯一索引兜底，冲突回滚后归一为 duplicate（不返回 5xx）。
   响应新增 `result`（`created|duplicate`）与 `duplicate`，`status` 仍是 issue 状态（与 result 正交），
   新建 201 / 重复 200，保证既有消费侧"200/201/409 均视为成功"的 ack 语义不破。
5. **ingest v1 字段面**：`level`（非法值降级 `warning` 并 WARN+计数）、`namespace/app/component/service_name/
   error_type/source_kind/grafana_url/loki_query/timestamp` 全部可选；超长字段截断（不拒绝上报），
   结构非法的 `grafana_url`/`loki_query` 丢弃并 WARN；`timestamp` 非法 422（事件时间不可信则拒收）。
   `exception.stacktrace` 用 `#[serde(untagged)]` 枚举接受 `Frame[]` 与字符串，字符串落 `extra.stacktrace_text`
   并尽力解析首帧。
6. **鉴权矩阵**：`ADMIN_TOKEN` 优先、回退 `CLIENT_TOKEN`，覆盖全部读接口与 PATCH；两者皆缺时读/PATCH
   fail-closed 503（启动 WARN 一次），写入端点沿用 `CLIENT_TOKEN` 门控的历史语义。
   公网 Ingress 只留 `POST /api/v1/ingest`(Exact) 与 `/healthz`，`/api/v1/traces` 移出公网。
7. **运维基线**：`/healthz` 做 2s 硬超时的 DB 探活（503 + `db:"down"`）；新增 `/metrics`
   （issues 按 status/environment、events 总数、ingest 结果计数、level 降级计数）；内置每日 retention
   （`RETENTION_DAYS_EVENTS`/`RETENTION_DAYS_TRACES`/`RETENTION_DISABLED`，5000 行/批）；
   新增 `deploy/systemd/pony-sentry-backup.{service,timer}` 与 `deploy/scripts/backup.sh`（`pg_dump -Fc | age`，
   失败清理半成品并产出 sha256）。
8. **Web 控制台**：展示并按 `environment`/`level` 过滤 issue；请求头注入令牌
   （构建期 `VITE_PONY_SENTRY_TOKEN` 或运行时 `localStorage.pony_sentry_token`，README 说明二选一）。

## Alternatives considered

- **SQLite 12-step 重建表去掉内联 `UNIQUE(fingerprint)`**：能精确等价 PG 的约束集合，但要在
  `sqlx::raw_sql` 批里切 `PRAGMA foreign_keys`（该 PRAGMA 在事务内无效）、重建父表并迁移 events 外键；
  一旦语句中途失败或崩溃，events 有被级联清空的风险。由于 v3 指纹已含环境分量，
  保留旧唯一约束不改变任何业务断言（跨环境仍拆两条 issue），故放弃重建。
- **新增 `issues.error_type` 列**：让 issue 直接持有错误类型。否决理由：错误类型是事件级属性，
  同一 issue 的后续事件可能给出不同 error_type；落列会引入"以哪条事件为准"的历史一致性问题。
  改为读接口从最近一条 event 载荷派生，并在 README limitations 标注为派生视图。
- **`event_id` 超长时截断**：与其它超长字段的处理保持一致，但会静默改变幂等键、把两件事汇报成同一件事。
  故对 `event_id` 单独走 422（契约 R2.3）。
- **响应 `status` 直接改为 `created|duplicate`**：语义更直白，但会破坏既有黑盒用例与消费侧对
  issue 状态的依赖。改为新增正交字段 `result`，`status` 保持 issue 状态。
- **用应用层计数器实现 `/metrics` 的 created/duplicate**：DB 无法区分"重复被拒"的投递，
  故在 `AppState` 持 `Arc<IngestMetrics>`（进程内 AtomicU64），重启归零；issues/events 指标仍实时查库。

## Consequences

- 历史 v2 issue 计数停止增长、与新 v3 issue 并存（不 reindex 的既定代价，已在 README 标注）；
  `PONY_REINDEX_FINGERPRINTS=1` 成为唯一的合并手段，且会重塑分组，仅限运维手动执行。
- 迁移为纯增量（加列/加索引/一次回填 UPDATE），回滚只需回退二进制；旧二进制对新 schema 兼容
  （INSERT 省略新列走 DEFAULT，SELECT 显式列名）。
- 读接口与 PATCH 现在必须带令牌：Web 控制台与脚本调用方需按 README 注入 token，否则 401；
  两 token 全未配置的实例读接口直接 503，属刻意的 fail-closed 取舍。
- 幂等只在客户端提供 `event_id` 时生效；无幂等键的上报仍是"每次一条 events 行"（语义与历史一致）。
- retention 只删 events/traces，issues 聚合与 `count` 永久保留，历史趋势不会因清理而回退。
