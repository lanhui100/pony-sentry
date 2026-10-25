# Agent Note: issue 治理（TTL 归档、列表/事件分页语义、统计与 ingest 治理指标）

Status: implemented

## Problem

issue 列表长期只有「一条时间线 + 一个 50 条上限」：`GET /api/v1/issues` 返回裸数组、
`limit` 默认 50 且无总数，前端把 50 条当全量；`ignored`/`resolved` 的历史 issue 没有生命周期出口，
永久占据列表并按 `last_seen_at DESC` 排序挤占首屏。生产 `project=pony-agent` 的实测分布是
11 条 issue / 185 次聚合事件，其中 6 条 `DIAG_STUB_UNIMPLEMENTED` 的 `ignored` issue 占 178 次事件（96%），
真正的 `unresolved` 只有 2 条——信噪比被历史噪声压住，且这条噪声是永久的（无 TTL、无归档）。

同一批缺口还有：`GET /api/v1/issues/:id/events` 的 `limit` 被硬编码成 20
（`crates/server/src/routes.rs` 的 `handle_get_issue_events` 直接把 20 传给仓储），
既没有游标也没有总数；按 project 维度的统计（status/level/environment 分组与时间序列）与
ingest 侧的治理计数（收到/接受/抑制）在服务端完全不可查——只能靠 `/metrics` 的 Prometheus 聚合与人工 psql。

## Decision

把「归档」定义成 issue 的可逆生命周期位，而不是删除，并围绕它补齐读侧语义：

1. **数据面**：`issues` 增 `archived_at`（归档时刻）与 `archive_restored_at`（手动恢复留痕），
   治理计数落 `ingest_metrics(bucket_date, project, outcome, count)`。
   迁移 `migrations/0007_issue_archive.sql`（SQLite）与 `migrations/postgres/0007_issue_archive.sql`（PG），
   PG 提供 `0007_issue_archive.down.sql`；两侧接线在 `PgIssueRepository::migrate` /
   `SqliteIssueRepository::migrate`（PG 走列/索引探测，SQLite 按列探测守卫整份迁移）。
2. **归档不是删除**：只写 `archived_at`，`status`/`count`/`events` 全不变；默认从列表与统计排除，
   `archived=include|only` 可查、`unarchive` 可恢复（写 `archive_restored_at`）。
3. **自动 TTL 归档**：作业只归档 `ignored`/`resolved` 且 `last_seen_at < now()-TTL`（默认 30 天）的项，
   `unresolved`/`in_progress`/`regression` 永不自动归档；`archive_restored_at ≥ last_seen_at` 时抑制重复归档。
   配置 `PONY_ISSUE_ARCHIVE_{ENABLED,TTL_DAYS,TTL_SECS,INTERVAL_SECS}`（非法值回落默认 + WARN）。
4. **活跃即解档**：ingest 命中已归档 issue 且写入后状态属活跃态时，在同一事务内清空两个归档列——
   归档不能把仍在发生的错误藏起来；`ignored` 不解档（抑制噪声正是它的用途）。
5. **列表语义显式化**：响应体仍是裸数组（控制台按数组消费），分页与总数走响应头
   `X-Total-Count`/`X-Limit`/`X-Offset`/`X-Has-More`/`X-Archived-Mode`/`X-Project-Filter`，
   `envelope=1` 才返回对象；`project` 缺省 = 不过滤（头里显式回 `all`）；
   `status`/`level`/`limit`/`offset`/`archived` 非法值一律 400（旧行为是静默忽略/钳制）。
6. **事件分页**：keyset 游标 `base64url({"created_at","id"})`，`ORDER BY created_at DESC, id DESC`，
   `limit` 默认 20/上限 100，`X-Total-Count` 给事件总行数，有下一页才下发 `X-Next-Cursor`。
7. **统计与治理指标**：`GET /api/v1/stats` 按 project 聚合 issue 木桶（status/level/environment）与
   `events.total = SUM(issues.count)`（聚合口径）+ `events.stored_rows = COUNT(*)`（行口径）+ 时间序列；
   `GET /api/v1/ingest/stats` 暴露 `received`/`accepted`/`created`/`duplicate`/`suppressed_ignored`/
   `rejected_invalid` 的按日/按 project 聚合，计数失败只 WARN、不影响 ingest 响应。
8. **SQLite 写事务改强隔离起点**：`BEGIN IMMEDIATE` + 连接级 `busy_timeout=5s` + 仅 `code 5` 的整事务有界重试，
   消除 deferred 事务锁升级导致的并发 `database is locked` 500。

契约与人读文档：`.dev-team/contract-issue-governance.md`（FROZEN v1.3，含 Gate-1 对抗审查后的 v1.1/v1.2 裁决）
与 `docs/issue-governance.md`（人读契约）。

## Alternatives considered

- **直接删除 ignored 历史（DELETE events/issues）**：被否。删除不可逆，且 issue 的历史计数是
  「这个问题发生过多少次」的唯一事实；`event_key` 幂等键依赖 events 行存在。归档保留全部数据面，
  只改变可见性，可用 `unarchive` 或 down 迁移整体回退。
- **在 issue 上加 `deleted`/`hidden` 布尔位**：被否。布尔位无法回答「什么时候归档的、什么时候恢复的」，
  也无法实现 TTL 抑制（需要比较「恢复时刻 vs 最近事件时刻」），
  所以采用 `archived_at` + `archive_restored_at` 两个时间戳。
- **TTL 归档在 ingest 路径上惰性触发（每次写入顺带扫一遍）**：被否。会把归档成本压到写热点上，
  且使 ingest 的延迟分布依赖归档表规模；改为独立周期作业（启动后首轮 + 固定周期），
  代价是归档延迟最多一个周期（生产默认 1 小时）。
- **列表直接改成 `{items,total,...}` 信封**：被否（本期）。控制台把响应当数组直接赋值
  （`web/src/App.vue`），改默认形态会破坏既有消费者；用响应头承载 total 是 additive 的，
  需要对象形态时显式加 `envelope=1`。
- **事件分页用 offset + count**：被否。offset 在时间序列上会因新事件插入而重复/漏行；
  keyset 用库内原始 `created_at` 串（SQLite 存纳秒文本，截断到微秒会漏行）保证不重不漏。
- **统计口径统一用 events 行数**：被否。生产事件行数（196）与聚合计数（185）不一致（历史重放写行不增计数），
  验收与运维直觉都要 `SUM(issues.count)`；因此两个口径都暴露且各带 `basis`/`stored_rows` 语义，禁止互相修正。
- **治理计数放进程内内存 + 定期 flush**：被否（本期）。重启丢数会让「抑制了多少噪声」不可追溯；
  改用 DB UPSERT，代价是每请求最多 3 次写（写放大列为遗留风险，后续可批量聚合）。
- **新增全局 `GET /api/v1/events` 列表**：本期不做。它需要新的分页与索引成本，且已有
  `/issues/:id/events` 覆盖排查路径；列入 README limitations 的非目标。

## Consequences

- 生产验收（2026-10-25 重启后 curl 实测）：`issues?project=pony-agent&status=unresolved` → 2 条、
  0 条 ignored 泄漏；`X-Total-Count=11`；`stats?project=pony-agent` → `issues.total=11`、
  `events.total=185`（basis=sum_issues_count）、`stored_rows=196`、`by_status={ignored:7,resolved:2,unresolved:2}`；
  `events?limit=5` → 恰好 5 行且游标翻页无重无漏；`limit=-5&offset=-10` → 400；归档/恢复往返后
  `count`/`status`/事件行数不变；ingest 冒烟 `created`(201) → `duplicate`(200) 且 `count` 不增。
- 零变更面：fingerprint v3、`event_id` 幂等（含 `uq_events_event_key`）、ingest 响应字段与状态码集合、
  `/metrics` 输出、traces 契约。回归基线 22 套件逐项保持绿（`.dev-team/receipts/regression-wave9-core-executor.txt`）。
- 显式 breaking（已在契约 C7.3 登记并做消费者普查）：列表/事件分页非法参数从静默忽略改为 400、
  列表默认排除归档项、事件排序补 `id` 次级键、`PATCH /api/v1/issues/archive` 因静态段优先变 405。
- 已知遗留：`ingest_metrics` 单热行写放大；SQLite 游标依赖 `created_at` 文本格式统一；
  归档作业单进程假设（多副本重复扫描但幂等）；`deploy/scripts/deploy-console.sh` 的 trace 读自检
  按 `PONY_TRACE_INGEST_ENABLED` 取 410/200（此前在停收配置下会误判部署失败并回滚）。
