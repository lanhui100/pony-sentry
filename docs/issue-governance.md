# Issue 治理与分析契约（归档 / 列表 / 事件分页 / 统计 / 治理指标）

> 人读契约的家：本文件。实现锚点：`crates/server/src/routes.rs`、`crates/core/src/{pg,sqlite}_repository.rs`、
> `crates/core/src/models.rs`、`migrations/0007_issue_archive.sql` 与 `migrations/postgres/0007_issue_archive.sql`。
> 变更本契约的行为必须 same-commit 更新本文件（根 AGENTS.md 命约 4）。
> 决策与替代方案：`.agents/notes/implemented/feature/2026-10-25-issue-governance-archive-and-analytics.md`。

## 0. 一句话语义

**归档不是删除**：归档只写 `issues.archived_at`（时间戳），`status`、`count`、`events` 行全部不变；
归档项默认从列表与统计中排除，但**可查询**（`archived=only|include`）且**可恢复**（`unarchive`）。

## 1. 数据模型与迁移

两个新列（`issues`）：

| 列 | 类型（PG / SQLite） | 语义 |
| :--- | :--- | :--- |
| `archived_at` | `TIMESTAMPTZ NULL` / `TEXT NULL`（ISO8601 UTC） | 非空 = 已归档。归档/查询/恢复唯一依据 |
| `archive_restored_at` | `TIMESTAMPTZ NULL` / `TEXT NULL` | 手动恢复留痕；阻止「恢复后立刻被 TTL 再归档」 |

治理计数表（`ingest_metrics`）：

| 列 | 类型（PG / SQLite） | 语义 |
| :--- | :--- | :--- |
| `bucket_date` | `DATE` / `TEXT`（`YYYY-MM-DD`，UTC） | 计数落桶日 |
| `project` | `TEXT NOT NULL DEFAULT ''` | 无项目维度落空串（读接口映射 `null`） |
| `outcome` | `TEXT NOT NULL` | `received` / `created` / `duplicate` / `suppressed_ignored` / `rejected_invalid`（应用层白名单，无 DB CHECK） |
| `count` | `BIGINT NOT NULL DEFAULT 0` | 累加值 |
| `updated_at` | `TIMESTAMPTZ NOT NULL DEFAULT now()` / `TEXT` | 最近一次累加时刻 |

索引：`issues(archived_at)`、`issues(project,status,archived_at)`、`issues(first_seen_at)`、
`events(issue_id,created_at,id)`、`events(created_at)`、`ingest_metrics(bucket_date)`。

- 迁移文件：`migrations/postgres/0007_issue_archive.sql`、`migrations/0007_issue_archive.sql`（SQLite 等价）；
  重放幂等（PG 侧 `IF NOT EXISTS` + 运行时列/索引探测；SQLite 侧列探测守卫整份迁移）。
- 运行时接线：启动时 `migrate()` 内执行（`PgIssueRepository::migrate` / `SqliteIssueRepository::migrate`），
  失败即启动失败；手工执行等价：`psql -v ON_ERROR_STOP=1 -f migrations/postgres/0007_issue_archive.sql`。
- 回滚：`migrations/postgres/0007_issue_archive.down.sql`（全 `IF EXISTS`，可重复执行）。
  **回滚顺序是必需的**：停服务 → 回退上一版二进制 → 执行 down.sql → 启动 → 验证。
  先 DROP COLUMN 会让仍在跑的旧二进制（列已并入 `ISSUE_COLUMNS` 投影）全部 500。
  SQLite 不支持 `DROP COLUMN`，**无 down 迁移**，回滚需重建表或保留列（列可空，旧版本忽略它）；
  升级前必须备份 `ponysentry.db`。

## 2. 归档语义

### 2.1 手动归档（admin 鉴权）

`POST /api/v1/issues/archive`，body：

```json
{"project": null, "statuses": ["ignored", "resolved"], "older_than_days": 30,
 "ids": null, "dry_run": false}
```

| 字段 | 语义 |
| :--- | :--- |
| `statuses` | 归档白名单，**只接受 `ignored`/`resolved`**（默认这两个；重复元素 → 400；其它值 → 400） |
| `ids` | 候选集（去重后 ≤1000，单项 ≤128 字符）。给出时 `project`/`older_than_days` **不参与筛选**，但 `statuses` **仍是白名单约束** |
| `older_than_days` | 只归档 `last_seen_at < now - N 天`；负数 → 400 |
| `dry_run` | 只统计、不改库（`archived` 恒 0） |

响应（200）：

```json
{"dry_run": false, "matched": 3, "archived": 3,
 "skipped_already_archived": 0, "skipped_not_archivable": 0, "skipped_missing": 0,
 "criteria": {"project": null, "statuses": ["ignored", "resolved"],
              "older_than_days": 30, "ids": null, "ids_take_priority": false}}
```

计数口径：`statuses` 模式 `matched ≡ archived`；`ids` 模式 `matched = dedup(ids).len()`，
**无并发时** `archived + skipped_not_archivable + skipped_already_archived + skipped_missing == matched`；
并发下只保证 `archived <= matched`。`criteria.statuses` 回显**生效白名单**；
`criteria.ids_take_priority=true` 只表示候选集由 `ids` 决定，**不表示** `statuses` 被忽略。

`POST /api/v1/issues/:id/archive`（单条）：200 + issue JSON；不存在 404；已归档幂等（`archived_at` 不变）。
`POST /api/v1/issues/:id/unarchive`（恢复）：200 + issue JSON（`archived_at` 变 `null`，`archive_restored_at` 记为恢复时刻）；
未归档幂等；不存在 404。

请求体上限 64 KB（超出 413）；所有 id/枚举走绑定参数（无字符串拼 SQL）。

### 2.2 自动 TTL 归档

| env | 默认 | 语义 |
| :--- | :--- | :--- |
| `PONY_ISSUE_ARCHIVE_ENABLED` | `1` | `0`/`false`/`off` 关闭自动作业（手动接口仍可用） |
| `PONY_ISSUE_ARCHIVE_TTL_SECS` | — | 优先于 `_DAYS`，测试/精细控制用 |
| `PONY_ISSUE_ARCHIVE_TTL_DAYS` | `30` | `ignored`/`resolved` 的 `last_seen_at` 早于 `now()-TTL` 即归档 |
| `PONY_ISSUE_ARCHIVE_INTERVAL_SECS` | `3600`（最小 1） | 扫描周期 |

- 归档条件：`status IN ('ignored','resolved') AND archived_at IS NULL AND last_seen_at < cutoff
  AND (archive_restored_at IS NULL OR archive_restored_at < last_seen_at)`。
- `unresolved` / `in_progress` / `regression` **永不自动归档**。
- 作业在 schema 迁移成功后启动首轮（非阻塞监听），失败以 `min(interval, 60s)` 退避重试；
  非法 env 值回落默认并 WARN，不 panic。
- 手动恢复过、且此后没有新事件的项**不会**在下一轮被重新归档（`archive_restored_at ≥ last_seen_at`）；
  恢复后一旦再有新事件（`last_seen_at` 前移），TTL 到期后重新可归档。

### 2.3 活跃即解档

ingest 写入命中「已归档」且写入后状态属**活跃态**（`unresolved` / `in_progress` / `regression`）时，
在**同一事务内**清空 `archived_at` 与 `archive_restored_at`——归档不会把仍在发生的错误藏起来。
`ignored` 与 `resolved` 不解档（`ignored` 归档正是抑制噪声的手段）。
该动作**不改** ingest 响应体、状态码、`count`、指纹与幂等语义。

## 3. 列表：`GET /api/v1/issues`

响应体**保持裸 JSON 数组**（web 控制台按数组消费）；分页与总数走**响应头**，避免"只回 50 条被误读为总数"。

| 参数 | 默认 | 非法值 |
| :--- | :--- | :--- |
| `project` | 缺省/空串 ⇒ **不过滤（全项目）** | — |
| `status` | 缺省 ⇒ 不过滤 | 非 `unresolved\|in_progress\|resolved\|ignored\|regression` → **400** |
| `level` | 缺省 | 非 `fatal\|error\|warning\|info` → 400 |
| `environment` / `platform` / `release` | 缺省 | 自由串 |
| `archived` | `exclude` | 非 `exclude\|include\|only` → 400 |
| `limit` | `50`，clamp 1..100（`0` → 1） | `<0` 或非整数 → 400 |
| `offset` | `0` | `<0` → 400 |
| `envelope` | 缺省 ⇒ 裸数组 | 非 `0\|1` → 400 |

响应头（200）：`X-Total-Count`（同过滤条件总数，**不随 `limit`/`offset` 变化**）、`X-Limit`、`X-Offset`、
`X-Has-More`（`offset+len<total`）、`X-Archived-Mode`、`X-Project-Filter`（缺省/空串为 `all`）。
`envelope=1` 时另返回 `{items,total,limit,offset,has_more,archived,filters:{project,status,level,environment,platform,release}}`。

排序不变：`ORDER BY last_seen_at DESC`。

## 4. 单 issue 事件分页：`GET /api/v1/issues/:id/events`

响应体保持裸 JSON 数组。

| 参数 | 默认 | 非法值 |
| :--- | :--- | :--- |
| `limit` | `20`，clamp 1..100 | `0`/负数/非整数 → 400 |
| `cursor` | 缺省（首页） | 解码失败/非 JSON/缺字段/多未知字段/>512 字符 → 400 `Bad request: invalid cursor` |
| `order` | `desc` | 非 `desc\|asc` → 400 |
| `envelope` | 缺省 ⇒ 裸数组 | 非 `0\|1` → 400 |

- 排序：`ORDER BY created_at DESC, id DESC`（`asc` 反向）——同 `created_at` 的相对顺序是确定的。
- 游标载荷：base64url（无 padding）编码的 `{"created_at":"<库内该行原始 RFC3339 串>","id":"<id>"}`，
  keyset 比较（**不是 offset**），**不截断小数位**（SQLite 存纳秒文本，截断会漏行）。
- 响应头：`X-Total-Count`（该 issue 事件总行数，与 cursor/limit 无关）、`X-Limit`、`X-Has-More`，
  **仅在有下一页时**下 `X-Next-Cursor`；`envelope=1` 另返回 `{items,next_cursor,has_more,limit,total}`。
- issue 不存在 → 200 `[]`（保持既有语义，不改为 404）。

## 5. 统计：`GET /api/v1/stats`

参数：`project?`、`environment?`、`level?`、`from?`（RFC3339，默认 `to-30d`）、`to?`（默认 now）、
`bucket=day|hour`（默认 `day`；`hour` 且区间 > 7 天 → 400；`from>to` → 400）、`include_archived=0|1`（默认 0）。

**作用域是逐字段定义的，不可互换**：

| 字段 | 口径 | 受 `from`/`to` | 受 `include_archived` |
| :--- | :--- | :--- | :--- |
| `issues.total` / `by_status` / `by_level` / `by_environment` | `issues` 行数（三组木桶各自之和 == `total`） | 否 | 是 |
| `events.total`（`basis="sum_issues_count"`） | `SUM(issues.count)` | 否 | 是 |
| `events.stored_rows` | `COUNT(*) FROM events` | **是**（`events.created_at`） | 是 |
| `events.by_level` / `by_environment` / `by_release` | `events` 行口径（NULL → `unknown`） | **是** | 是 |
| `series[].issues` | 按 `issues.first_seen_at` 落桶 | 是 | 是 |
| `series[].events` | 按 `events.created_at` 落桶 | 是 | 是 |

- 空桶补 0；`bucket=day` 键 `YYYY-MM-DD`，`bucket=hour` 键 `YYYY-MM-DDTHH:00:00Z`；点数上限 744。
- `sum(series[].events) == events.stored_rows`（两侧共用同一 WHERE）。
- 两个事件口径**必须分开读**：`events.total` 是聚合计数（`issues.count` 之和），`events.stored_rows` 是
  `events` 表真实行数；历史重放写入的行可能多于聚合计数，两者不得相加、不得互相修正。
  生产 `project=pony-agent` 实测：`issues.total=11`、`events.total=185`、`events.stored_rows=196`。

## 6. 治理指标：`GET /api/v1/ingest/stats`

参数：`window=24h|7d|30d`（默认 `24h`）、`project?`、`bucket=day|hour`（默认 `day`）；非法值 400。

响应：`{window, from, to, bucket, totals:{received, accepted, created, duplicate, suppressed_ignored, rejected_invalid},
by_project:[{project,received,accepted,suppressed_ignored}], series:[{bucket,received,accepted,suppressed_ignored}]}`。

- `accepted` 是派生值（恒等于 `created + duplicate`）。
- 计数时机：`received` = `POST /api/v1/ingest` 通过鉴权后每请求 +1（401 不计）；`created`/`duplicate` 按写入结果；
  `suppressed_ignored` = 写入成功但命中 `status='ignored'` 的 issue（ingest 对 ignored 照常写事件与 `count+1`，
  这里是旁路分类计数）；`rejected_invalid` = 400/413（401/410/5xx 不计）。
- 不变式（单写者）：`received == created + duplicate + rejected_invalid`，且 `received >= created + duplicate`。
- 写失败/超时（1000ms）只 WARN，**不影响 ingest 响应与状态码**；批量缓冲未实现，每事件一次 UPSERT（写放大见遗留风险）。

## 7. 零变更清单（改这些即违约）

- `POST /api/v1/ingest` 的路径、请求体解析、响应字段（`result/status/issue_id/event_id/fingerprint/count/duplicate/warnings`）
  与状态码（201/200/400/401/413/410）；fingerprint v3 计算；`event_id` 幂等（含 `uq_events_event_key` 唯一约束与并发归一 duplicate）。
- `/metrics` 的指标名/标签/HELP（治理指标走 DB，不走 Prometheus 输出）。
- traces 侧契约（`docs/trace-contract.md`）。

显式行为变更（已评估消费者）：列表/事件分页非法 `status`/`level`/`limit`/`offset` 由静默忽略改为 400；
列表默认排除归档项；事件排序补 `id` 次级键；`PATCH /api/v1/issues/archive` 因静态段优先返回 405。

## 8. 遗留风险

1. 治理计数每事件一次 UPSERT：高吞吐下形成 `ingest_metrics` 单热行写竞争，需要批量聚合（未实现）。
2. SQLite 游标依赖 `events.created_at` 文本格式统一（写入侧是 RFC3339 UTC 固定格式）；若历史库存在异构格式，
   字典序与时间序可能不一致。
3. 归档作业与手动归档并发时，`matched`/`skipped` 分桶是最终一致的（契约只承诺单写者等式）。
4. SQLite 绑定变量上限 999：`ids` 上限 1000 依赖实现分批（每批 ≤500）；其它后端不影响。
