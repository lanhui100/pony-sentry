# issue 治理实施契约 v1.1（FROZEN）

> 状态：**FROZEN v1.1**（Protocol Agent 落笔；v1.0 + Gate-1 逆向对抗裁决 A-1…B-12 已并入；未列入 amend 记录的任何偏离都视为违约）。
> 范围：issue 归档（归档 ≠ 删除）+ issue 列表语义修正 + 单 issue 事件分页 + `/api/v1/stats` + `/api/v1/ingest/stats`。
> 本文件是 Test Agent 冻结红相与 Executor 实现对齐的**唯一口径**；每条判据均为机器可判定（状态码 / 字段名 / 字段类型 / 响应头名 / 默认值 / 边界值）。
> 非目标见 §C8。本文全部判据已定值（状态码/字段名/类型/默认值/边界值），无未决项。

## 0. 事实依据（path:line，已核实，可直接采信）

| # | 事实 | 证据 |
| :-- | :--- | :--- |
| F1 | 路由表：`ingest_routes` / `admin_routes`（admin 组统一挂 `admin_auth_middleware`） | `crates/server/src/routes.rs:845-911`，鉴权中间件挂载点 `:879-882` |
| F2 | `GET /api/v1/issues` → `handle_list_issues`，返回 **bare JSON array**（无 total） | `crates/server/src/routes.rs:1555-1578` |
| F3 | 过滤结构 `IssueFilter`（status/platform/release/project/environment/level/limit/offset） | `crates/core/src/repository.rs:21-32` |
| F4 | `limit` 缺省 50、`clamp(1,100)`；`offset` 缺省 0、`max(0)`（sqlite 同构） | `crates/core/src/pg_repository.rs:624-630` |
| F5 | `status` 非法值当前被**静默忽略**（`and_then(... .ok())`） | `crates/server/src/routes.rs:1559` |
| F6 | `GET /api/v1/issues/:id/events` → `handle_get_issue_events`，**硬编码 20 行**，Query 参数完全未解析 | `crates/server/src/routes.rs:1688-1699`（`get_events_for_issue(&id, 20)` 在 `:1694`） |
| F7 | 未知 issue 的 events 返回 **200 `[]`**（保持，不改 404） | `crates/server/src/routes.rs:1692-1698` |
| F8 | `/api/v1/stats`、`/api/v1/events` 均**不存在**（404） | 路由表 `routes.rs:845-911` 无该路径 |
| F9 | **既有先例**：traces 列表用响应头 `X-Total-Count`，配套 `TraceRepository::count_traces` | `crates/server/src/routes.rs:2022-2052`；`crates/core/src/repository.rs:133-136` |
| F10 | 生产库实测（project=pony-agent）：issues=11（ignored 7 / resolved 2 / unresolved 2）、environment 全 dev、level 全 error、`sum(issues.count)=185`、`events` 表 196 行 | Lead 侦察，验收硬值（A8） |
| F11 | 无 `schema_migrations` 版本表；迁移由 `ensure_schema(include_str!(...))` 在运行时应用，必须**可重复执行** | `crates/core/src/pg_repository.rs:39-115`；`crates/core/src/sqlite_repository.rs:38-130` |
| F12 | PG 侧 `ALTER TABLE ... ADD COLUMN` 有存在性探测特判 | `crates/core/src/pg_repository.rs:112`、`:325` |
| F13 | SQLite 无 `ADD COLUMN IF NOT EXISTS`，沿用「探测-再 ALTER」模式 | `crates/core/src/sqlite_repository.rs:38-50`（范式同 0002/0004/0005/0006） |
| F14 | status 全集合 `unresolved\|in_progress\|resolved\|ignored\|regression`；`regression` 属**活跃态** | `crates/core/src/models.rs:6-38` |
| F15 | issues 表**无** created_at/updated_at（只有 `first_seen_at`/`last_seen_at`）；events 只有 `created_at` | `migrations/postgres/0001_init.sql:12-13,22` |
| F16 | events 现排序 `ORDER BY created_at DESC`，**无次级键**（同 created_at 顺序不稳定；sqlite 同构） | `crates/core/src/pg_repository.rs:662`；`crates/core/src/sqlite_repository.rs:557` |
| F17 | ingest 对 `ignored` issue **不拦截**：仍写 events 且 count+1；只有 `resolved` 触发 regression 跃迁 | `crates/core/src/pg_repository.rs:421-428` |
| F18 | 既有 Prometheus `/metrics` 有内存态 `IngestMetrics`（created/duplicate/degraded_level 等） | `crates/server/src/routes.rs:188-215`、`:1466-1469` |
| F19 | web 控制台把 `/api/v1/issues` 响应当**数组**直接赋值；events 同样当数组 | `web/src/App.vue:1664-1669`、`:1734-1738` |
| F20 | 控制台**不传** status/level/limit/offset（status/level 在浏览器侧过滤；服务端只承担 platform/project/environment） | `web/src/App.vue:1631-1667` |
| F21 | 存量测试基线全绿：`cargo test --workspace` exit 0、22 套件 | `.dev-team/regression-baseline-wave9.json` |
| F22 | `Issue` 结构体字面量共 8 处（core 内 6 处 + reindex 2 处），新增字段必须同步 row 映射 | `crates/core/src/pg_repository.rs:255,467,520`；`crates/core/src/sqlite_repository.rs:217,363,416`；`crates/core/src/reindex.rs:142,226` |
| F23 | **fingerprint v3 不含 `project`**：project 由首报决定并保存在 `issues.project`；同一错误跨项目会并入同一 issue（`amend v1.1 / B-12`） | Lead 裁决 B-12；`crates/core/src/models.rs:86-87`（project 为 issue 级快照），`migrations/postgres/0001_init.sql:2-14`（指纹唯一键不含 project） |
| F24 | SQLite `events.created_at` 为既有 ISO8601 文本存储，**本期不改格式**；分页游标必须携带库内原始串（`amend v1.1 / B-2`） | `crates/core/src/sqlite_repository.rs:557`（events 读路径既有 SQL）；C7.1 零变更清单 |

---

## C1 数据模型与迁移

### C1.1 PG 迁移 `migrations/postgres/0007_issue_archive.sql`（幂等，逐句执行）

```sql
ALTER TABLE issues ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ NULL;
-- amend v1.1 / A-1：手动恢复留痕（自动 TTL 归档的抑制依据）
ALTER TABLE issues ADD COLUMN IF NOT EXISTS archive_restored_at TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS idx_issues_archived_at ON issues(archived_at);
CREATE INDEX IF NOT EXISTS idx_issues_project_status_archived ON issues(project, status, archived_at);
-- amend v1.1 / B-7：治理读路径索引（分页 keyset / stats series / 统计时间窗）
CREATE INDEX IF NOT EXISTS idx_issues_first_seen_at ON issues(first_seen_at);
CREATE INDEX IF NOT EXISTS idx_events_issue_created_id ON events(issue_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_events_created_at ON events(created_at);

CREATE TABLE IF NOT EXISTS ingest_metrics (
    bucket_date DATE NOT NULL,
    project     TEXT NOT NULL DEFAULT '',
    outcome     TEXT NOT NULL,
    count       BIGINT NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_date, project, outcome)
);
CREATE INDEX IF NOT EXISTS idx_ingest_metrics_bucket ON ingest_metrics(bucket_date);
```

| 判据 | 值 |
| :--- | :--- |
| `issues.archived_at` 类型 / 可空 / 默认 | `TIMESTAMPTZ` / `NULL` 允许 / 无默认（NULL 即未归档） |
| `issues.archive_restored_at` 类型 / 可空 / 默认 | `TIMESTAMPTZ` / `NULL` 允许 / 无默认（amend v1.1 / A-1：最近一次手动恢复时间；NULL = 从未手动恢复） |
| 归档列索引 | `idx_issues_archived_at(archived_at)`、`idx_issues_project_status_archived(project,status,archived_at)` |
| 治理读路径索引（B-7） | `idx_issues_first_seen_at(first_seen_at)`、`idx_events_issue_created_id(issue_id, created_at, id)`、`idx_events_created_at(created_at)` |
| `ingest_metrics.outcome` 约束 | **不加 `CHECK` 约束**（PG 侧不加，避免扩容需迁移）；取值由应用层白名单写死（C1.5） |
| `ingest_metrics.project` 空值语义 | 无 project 维度落 **`''`**（非 NULL）；读接口把 `''` 映射为 JSON `null` |
| 主键 | `(bucket_date, project, outcome)` |
| 重复执行 | 全文件二次执行 exit 0、无 error（`IF NOT EXISTS` 全覆盖） |

### C1.2 SQLite 迁移 `migrations/0007_issue_archive.sql`（等价语义）

```sql
ALTER TABLE issues ADD COLUMN archived_at TEXT;
-- amend v1.1 / A-1：与 PG 同义，存 ISO8601 UTC
ALTER TABLE issues ADD COLUMN archive_restored_at TEXT;

CREATE TABLE IF NOT EXISTS ingest_metrics (
    bucket_date TEXT NOT NULL,
    project     TEXT NOT NULL DEFAULT '',
    outcome     TEXT NOT NULL,
    count       INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (bucket_date, project, outcome)
);
CREATE INDEX IF NOT EXISTS idx_ingest_metrics_bucket ON ingest_metrics(bucket_date);
-- amend v1.1 / B-7：治理读路径索引（与 PG 同名同构）
CREATE INDEX IF NOT EXISTS idx_issues_first_seen_at ON issues(first_seen_at);
CREATE INDEX IF NOT EXISTS idx_events_issue_created_id ON events(issue_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_events_created_at ON events(created_at);
```

- `archived_at` / `archive_restored_at` 为 `TEXT NULL`，存 **ISO8601 UTC**（`YYYY-MM-DDTHH:MM:SS(.ffffff)Z`，与既有 chrono 落库格式一致）；NULL = 未归档 / 从未手动恢复。
- SQLite 的 `ALTER TABLE ... ADD COLUMN` 无 `IF NOT EXISTS`，**必须**沿用「列探测 → 再 ALTER」范式（F13）；`CREATE TABLE/INDEX IF NOT EXISTS` 自身幂等。
- `idx_issues_archived_at`、`idx_issues_project_status_archived`、`idx_issues_first_seen_at`、`idx_events_issue_created_id`、`idx_events_created_at` 同构创建（`IF NOT EXISTS`）。

### C1.3 回滚

| 方言 | 文件 | 内容 | 判定 |
| :--- | :--- | :--- | :--- |
| PG | `migrations/postgres/0007_issue_archive.down.sql` | `DROP TABLE IF EXISTS ingest_metrics;`、`DROP INDEX IF EXISTS idx_ingest_metrics_bucket;`、`DROP INDEX IF EXISTS idx_issues_project_status_archived;`、`DROP INDEX IF EXISTS idx_issues_archived_at;`、`DROP INDEX IF EXISTS idx_issues_first_seen_at;`、`DROP INDEX IF EXISTS idx_events_issue_created_id;`、`DROP INDEX IF EXISTS idx_events_created_at;`、`ALTER TABLE issues DROP COLUMN IF EXISTS archive_restored_at;`、`ALTER TABLE issues DROP COLUMN IF EXISTS archived_at;` | 全部 `IF EXISTS`；二次执行 exit 0 |
| SQLite | **不提供 down 文件** | SQLite（当前 sqlx 绑定）不支持 `DROP COLUMN` | 明记「不支持回滚；需要回滚时走重建表流程（`CREATE TABLE issues_new ... ; INSERT INTO issues_new SELECT ...; DROP TABLE issues; ALTER TABLE issues_new RENAME TO issues;`）」，与 0005（SQLite 无 down）先例一致 |

**回滚顺序（amend v1.1 / B-9，契约必需项，顺序不可交换）**：

1. 停止服务（进程退出，确保没有写入方）；
2. **恢复上一版二进制**（回退到不认识新列的版本）；
3. 执行 `0007_issue_archive.down.sql`；
4. 启动服务；
5. 验证：`/healthz` 200、`GET /api/v1/issues` 200、`GET /api/v1/traces` 200。

> 反序（先 `DROP COLUMN` 再回退二进制）会让**仍在运行的旧二进制**读 `ISSUE_COLUMNS` 时命中"列不存在" → 全量 500。该顺序为门禁判据（测试/运维收据必须按 1→5 记录）。

### C1.4 迁移接线（**新迁移文件必须同时被两侧 `ensure_schema` 引用**）

| 侧 | 位置 | 要求 |
| :--- | :--- | :--- |
| PG | `crates/core/src/pg_repository.rs::migrate`（F11） | 在 0006 之后调用 `ensure_schema(include_str!("../../../migrations/postgres/0007_issue_archive.sql"))`；`ADD COLUMN` 走既有存在性探测特判（F12），索引走 `CREATE INDEX`/探测分支 |
| SQLite | `crates/core/src/sqlite_repository.rs::migrate`（F11/F13） | **两列分别探测、分别 ALTER**（部分迁移可自愈，范式同 0006）：`SELECT archived_at FROM issues LIMIT 0` 失败才 `ALTER TABLE issues ADD COLUMN archived_at TEXT;`；`SELECT archive_restored_at FROM issues LIMIT 0` 失败才 `ALTER TABLE issues ADD COLUMN archive_restored_at TEXT;`；`ingest_metrics` 建表与全部索引语句**每次启动都执行**（自身幂等） |

- 启动序列**不得**因 0007 失败而静默降级：迁移失败即启动失败（沿用既有 0005/0006 语义）。
- `Issue` 结构体新增两个字段：`pub archived_at: Option<DateTime<Utc>>`、`pub archive_restored_at: Option<DateTime<Utc>>`，均带 `#[serde(default)]`（未归档/未恢复序列化为 `null`）。
  **落地归属**：由 executor 在实现波一并补齐 **8 处**结构体字面量（F22）+ `ISSUE_COLUMNS` 投影与 row 映射；Protocol Agent 的纯空桩**不**改这些映射（改则越出写域且破坏 `cargo check`）。

### C1.5 `outcome` 取值白名单（写死，应用层枚举 `IngestMetricOutcome`）

| outcome | 语义 |
| :--- | :--- |
| `received` | 通过鉴权后的上报请求（每请求 +1） |
| `created` | 新建事件（`is_duplicate == false`） |
| `duplicate` | `event_key` 幂等命中（`is_duplicate == true`） |
| `suppressed_ignored` | 写入成功、但该 issue 当时 `status='ignored'` 的旁路分类 |
| `rejected_invalid` | 请求体不合法（400/413） |

---

## C2 归档语义（归档 ≠ 删除）

### C2.1 语义硬约束

| 项 | 判据 |
| :--- | :--- |
| 归档标记 | `issues.archived_at IS NOT NULL` 即归档 |
| 恢复留痕 | `issues.archive_restored_at` = **最近一次手动恢复**的时刻（未恢复为 NULL）；`amend v1.1 / A-1` |
| `status` | 归档**不改** `status` |
| `count` | 归档**不改** `count` |
| events 行 | 归档**不删**任何 events 行（行数前后相等） |
| `first_seen_at`/`last_seen_at` | 归档**不改** |
| 手动归档（单条/批量） | **不覆盖** `archive_restored_at`（保持原值，只置 `archived_at`） |
| 自动归档候选 | `status IN ('ignored','resolved') AND archived_at IS NULL AND last_seen_at < now() - TTL AND (archive_restored_at IS NULL OR archive_restored_at < last_seen_at)` — 末项为 `amend v1.1 / A-1`：手动恢复后且期间无新事件 → 不再被自动归档；恢复后来了新事件（`last_seen_at` 前移）→ TTL 到期后重新可归档 |
| **永不**自动归档 | `unresolved`、`in_progress`、`regression`（F14：`regression` 是活跃态） |

### C2.2 配置项（环境变量；非法值 → 回落默认并 `warn!`，**不得 panic**）

| 变量 | 类型 | 默认 | 边界/回落 |
| :--- | :--- | :--- | :--- |
| `PONY_ISSUE_ARCHIVE_ENABLED` | bool | `1` | `1/true/on` 启用；`0/false/off` 关闭自动作业（手动接口仍可用）；其他值 → 视为启用 + `warn` |
| `PONY_ISSUE_ARCHIVE_TTL_SECS` | u64 秒 | 未设 | **优先级最高**（测试/精细控制）；`0` 或非法 → 回落到 DAYS/默认 + `warn` |
| `PONY_ISSUE_ARCHIVE_TTL_DAYS` | u64 天 | `30` | 次优先；`0` 或非法 → 30 天 + `warn` |
| `PONY_ISSUE_ARCHIVE_INTERVAL_SECS` | u64 秒 | `3600` | **最小 1**（`<1` → 取 1 + `warn`）；**生产取值 3600**（默认值即生产值） |

- TTL 解析顺序写死：`TTL_SECS`（>0）> `TTL_DAYS`（>0）> 30 天。
- 归档作业：**启动后立即跑首轮**，且**不得阻塞启动**（后台 `tokio::spawn`，与 `axum::serve` 并行）；之后按 `INTERVAL_SECS` 固定周期。
- **首轮时序**（`amend v1.1 / B-10`）：首轮**必须**在 `0007` schema 迁移成功返回之后启动（迁移失败即启动失败，作业不得先于 schema 存在而报错刷日志）。
- **失败退避**（`amend v1.1 / B-10`）：单轮失败后的下次重试间隔 = `min(INTERVAL_SECS, 60)` 秒（成功轮之后回到 `INTERVAL_SECS`）。
- 作业不 per-ingest 触发（无惰性检查；避免 per-ingest 开销），仅「启动一轮 + 固定周期」。
- 单轮失败只 `warn!`（`failure_policy = warn_only`），不得终止作业循环、不得使进程退出。
- 首轮与每轮都在单事务内执行批量 `UPDATE`；返回归档行数写入日志字段 `archived`。
- 每轮的 `UPDATE ... WHERE` 必须包含 C2.1 的 `archive_restored_at` 抑制项（`A-1`），且**不覆盖** `archive_restored_at`。

### C2.3 手动接口（admin token，沿用 `admin_routes` 既有鉴权：静态 `admin_token`/`client_token` 回退、服务端会话 Cookie/Bearer + CSRF，见 `routes.rs:419-460`、`:879-882`）

| 方法 | 路径 | 请求体/参数 | 成功 | 失败（可判定全集） |
| :--- | :--- | :--- | :--- | :--- |
| POST | `/api/v1/issues/archive` | JSON `{project?, statuses?, older_than_days?, ids?, dry_run?}`；**`DefaultBodyLimit::max(64*1024)`**（超出 → **413**） | 200（结构见 C2.4） | 400 / 401 / 403（CSRF）/ 413 / 500 / 503（token 未配置） |
| POST | `/api/v1/issues/:id/archive` | 无体 | 200 issue JSON（含 `archived_at`，additive） | 400 / 401 / 403 / 404（issue 不存在）/ 500 / 503 |
| POST | `/api/v1/issues/:id/unarchive` | 无体 | 200 issue JSON（`archived_at: null`、`archive_restored_at` 非空） | 400 / 401 / 403 / 404 / 500 / 503 |

- **5xx 响应体统一 `Internal error`**（`amend v1.1 / B-6`）：复用既有 `internal_error()`，DB 原文只进日志，**禁止回吐**到响应体（PG/SQLite 错误文本、SQL 片段、表名/列名均不得出现）。
- 失败码语义（`amend v1.1 / B-8`）：`401` 无/错 token；`403` 会话 Cookie 缺/错 CSRF；`503` 未配置任何 admin/client token（fail-closed，既有语义）。

`IssueArchiveRequest` 字段表：

| 字段 | 类型 | 默认 | 合法性（越界全部 400，文案写死） |
| :--- | :--- | :--- | :--- |
| `project` | string? | 缺省 = 全项目 | 空串视同缺省 |
| `statuses` | string[]? | **`["ignored","resolved"]`** | 白名单仅 `ignored`/`resolved`；任一越界（含 `unresolved`/`in_progress`/`regression`/未知串）→ `Bad request: invalid statuses`；**元素数 > 2 或含重复元素** → `Bad request: invalid statuses` |
| `older_than_days` | int? | 缺省 = 不按时间限 | `<0` → `Bad request` |
| `ids` | string[]? | 缺省 = 不按 id 限 | 非字符串数组 → `Bad request`；**`dedup(ids).len() > 1000`** → **`Bad request: too many ids`**；任一 id 长度 **> 128 字符** → `Bad request: invalid ids` |
| `dry_run` | bool? | `false` | 非 bool → 400；`true` 只统计不改库 |

- **硬上限（`amend v1.1 / B-5`）**：`ids` 去重后最多 **1000** 个；单 id ≤ **128** 字符；`statuses` 最多 **2** 项且不得重复。全部绑定参数（`= ANY($1)` 形态，禁止字符串拼接）。
- **候选集与白名单（`amend v1.2`，改写 v1.1 的「statuses 被忽略」表述）**：`ids` 决定**候选集**——只处理这些 id，`project` 与 `older_than_days` **不参与筛选**；`statuses` **仍然作为归档白名单约束**（缺省 `["ignored","resolved"]`）。即 `{"ids":[A,B],"statuses":["ignored"]}` **只归档其中 `status='ignored'` 的行**，其余（含 `resolved`）计入 `skipped_not_archivable`。
- **`statuses`/`project` 模式**（`amend v1.1 / A-3`）：单条 `UPDATE issues SET archived_at = now() WHERE status = ANY($1) AND archived_at IS NULL [AND project = $2] [AND last_seen_at < now() - older_than_days] RETURNING id`；`matched ≡ archived ≡ RETURNING 行数`，`skipped_*` 恒 0（`archive_restored_at` 不参与条件、不被覆盖）。
- **`ids` 模式**（`amend v1.1 / A-3`，同一事务内两步）：
  1. `SELECT id, status, archived_at FROM issues WHERE id = ANY($1)` 分桶：`status ∉ 生效白名单` → `skipped_not_archivable`；`archived_at IS NOT NULL` → `skipped_already_archived`；库中不存在 → `skipped_missing`；
  2. `UPDATE issues SET archived_at = now() WHERE id = ANY($1) AND status = ANY($2) AND archived_at IS NULL RETURNING id` → `archived`；其中 `$2` = **生效白名单**（请求未给 `statuses` → `["ignored","resolved"]`，给了 → 原值去重后）；`$1` 只来自 `dedup(ids)`，`project`/`older_than_days` 在本模式**不参与** WHERE；
  3. `matched = dedup(ids).len()`（含不存在与不可归档）。
- `dry_run=true`：只执行第 1 步分桶与计数，**不执行**第 2 步；`archived == 0`、`matched` 与真实执行一致。
- 幂等：`/:id/archive` 对已归档 issue 返回 **200 且 `archived_at` 不变、`archive_restored_at` 不变**；`/:id/unarchive` 对未归档 issue 返回 **200 且 `archived_at` 仍为 null**，`archive_restored_at` 置为本次 now（`A-1`：恢复留痕单调前移）。
- `/:id/unarchive` 的成功语义（`amend v1.1 / A-1`）：`archived_at = NULL` 且 `archive_restored_at = now()`（同一语句写入）。
- 路径冲突说明（机器可判定）：`/api/v1/issues/archive` 为静态段，优先级高于 `/api/v1/issues/:id`；因此 `GET /api/v1/issues/archive` 与 `PATCH /api/v1/issues/archive` 均返回 **405**（不按 id 查）。issue id 为 UUID，无实际影响，测试不得以此为回归失败依据。

### C2.4 批量响应结构（字段名与类型写死）

```json
{"dry_run": false, "matched": 0, "archived": 0,
 "skipped_already_archived": 0, "skipped_not_archivable": 0, "skipped_missing": 0,
 "criteria": {"project": null, "statuses": ["ignored","resolved"],
              "older_than_days": null, "ids": null, "ids_take_priority": false}}
```

| 字段 | 类型 | 判据 |
| :--- | :--- | :--- |
| `dry_run` | bool | 回显请求值 |
| `matched` | int ≥ 0 | **按模式定义**：`statuses` 模式 ≡ `archived`（命中即归档）；`ids` 模式 = `dedup(ids).len()` |
| `archived` | int ≥ 0 | 本次实际置 `archived_at` 的行数；`dry_run=true` 时 **恒为 0** |
| `skipped_already_archived` | int ≥ 0 | 命中但已归档（仅 `ids` 模式非零） |
| `skipped_not_archivable` | int ≥ 0 | 命中但 `status ∉ 生效白名单`（含 `unresolved`/`in_progress`/`regression`；仅 `ids` 模式非零） |
| `skipped_missing` | int ≥ 0 | `ids` 在库中不存在（`amend v1.1 / A-3`；仅 `ids` 模式非零） |
| `criteria.project` | string\|null | 空串归一为 `null` |
| `criteria.statuses` | string[] | **实际生效白名单**（`amend v1.1 / A-4` + `v1.2`）：未给 `statuses` → `["ignored","resolved"]`；给了 → **原值去重后**（已过白名单校验）；`ids` 模式下**仍是归档约束** |
| `criteria.older_than_days` | int\|null | 回显 |
| `criteria.ids` | string[]\|null | 回显 |
| `criteria.ids_take_priority` | bool | `ids` 非空即 `true`；语义 = **候选集由 `ids` 决定（`project`/`older_than_days` 被忽略）**，**不表示** `statuses` 被忽略（`statuses` 恒为白名单约束，`amend v1.2`） |
| 不变式（**单写者/无并发**） | — | `archived + skipped_not_archivable + skipped_already_archived + skipped_missing == matched`；`dry_run=true` 时 `archived == 0` 且其余计数与真实执行一致 |
| 不变式（**并发下**） | — | 只保证 `archived <= matched` 且 `archived + skipped_* <= matched`（并发归档/恢复会改变分桶）；L2-AT 并发用例**只断言无 5xx 与归档幂等**，不得断言等式 |

### C2.5 活跃即解档（`amend v1.1 / A-2`，修 A-BLOCKING#3「活跃僵尸」）

- **触发**：`POST /api/v1/ingest` 写入成功后，若写入后 `status ∈ {unresolved, in_progress, regression}` **且** `archived_at IS NOT NULL` → 在 **ingest 的同一事务内**执行一条追加 `UPDATE`：`UPDATE issues SET archived_at = NULL, archive_restored_at = NULL WHERE id = $1`。
- **不解档**：写入后 `status ∈ {ignored, resolved}` → 保持归档状态不变（`ignored` 归档正是抑制噪声的目的；`resolved` 由 TTL 策略管理，不由 ingest 解档）。
- **零变更硬约束（与 C7.1 一致）**：该追加 UPDATE **不改** ingest 响应体字段、**不改** 状态码（201/200/400/401/413/410）、**不改** `issues.count`、**不改** fingerprint/`event_id` 幂等语义、**不改** events 行内容。仅追加一条 UPDATE，且与主写入同事务（失败则整体回滚，不得部分生效）。
- **可判定判据**：归档一个 `ignored` issue → 再上报同 issue 的新事件：ingest 返回 **200**（若 `ignored` 未触发 regression）或 **201**（若 `resolved` 触发 regression），响应体字段集合与未归档时**逐字段一致**；之后 `GET /api/v1/issues/:id` 的 `archived_at` 为 **null** 当且仅当写入后 status 属 `{unresolved, in_progress, regression}`。
- **与 C2.1 交互**：`regression` 属活跃态 → 解档后不会被自动归档（C2.1「永不自动归档」）；`archive_restored_at` 同时清空（解档后无需抑制项，行为回到"从未归档"）。

---

## C3 列表语义修正（`GET /api/v1/issues`）

### C3.1 响应体形态（**硬约束**）

- 默认响应体**保持 bare JSON array**（`[...]`），不得改成 `{items:...}` —— 消费者把响应当数组赋值（F19）。
- `?envelope=1` 时返回 C3.4 的对象形态；`envelope` 仅接受 `0`/`1`（空串视同缺省），其他值 → **400 `Bad request: invalid envelope`**。

### C3.2 Query 参数判据

| 参数 | 类型 | 默认 | 非法值判定 |
| :--- | :--- | :--- | :--- |
| `project` | string | 缺省或空串 ⇒ **不过滤（全项目）** | — |
| `platform` | string | 缺省 ⇒ 不过滤 | 自由串（不校验枚举） |
| `release` | string | 缺省 ⇒ 不过滤 | 自由串 |
| `environment` | string | 缺省 ⇒ 不过滤 | 空串 ⇒ 不过滤；非空自由串（`dev|development|preprod|production|<自由串>`） |
| `level` | string | 缺省 ⇒ 不过滤 | 非 `fatal|error|warning|info` → **400 `Bad request: invalid level`** |
| `status` | string | 缺省 ⇒ 不过滤 | 非 `unresolved|in_progress|resolved|ignored|regression` → **400 `Bad request: invalid status`**（修正 F5 静默忽略） |
| `archived` | enum | **`exclude`** | 非 `exclude|include|only` → **400 `Bad request: invalid archived`** |
| `limit` | int | `50` | 非整数/空串以外非法字面量 → 400；`<0` → **400 `Bad request: invalid limit`**；`0` → clamp 到 1；`>100` → clamp 到 100 |
| `offset` | int | `0` | 非整数 → 400；`<0` → **400 `Bad request: invalid offset`** |
| `envelope` | 0\|1 | 缺省（=裸数组） | 其他 → 400 |

- `archived` 三态语义（与 `archived_at` 绑定）：
  - `exclude`：`archived_at IS NULL`；
  - `only`：`archived_at IS NOT NULL`；
  - `include`：不加归档条件。
- 排序不变：`ORDER BY last_seen_at DESC`（F4 既有）。

### C3.3 响应头（**作用域：仅本列表接口**；events 侧响应头见 C4.3。全部小写名，值为 ASCII；每次 200 都带）

| 响应头 | 类型/取值 | 判据 |
| :--- | :--- | :--- |
| `X-Total-Count` | 十进制整数 | **同过滤条件**的总数（`count_issues`，不受 `limit/offset` 影响；先例 F9） |
| `X-Limit` | 十进制整数 | 生效 limit（clamp 后） |
| `X-Offset` | 十进制整数 | 生效 offset |
| `X-Has-More` | `true`\|`false` | `offset + items.len() < total` |
| `X-Archived-Mode` | `exclude`\|`include`\|`only` | 回显生效值 |
| `X-Project-Filter` | `all`\|`<project>` | 缺省/空串 → **`all`**；给定值 → 原值 |

- **GET 侧失败码（`amend v1.1 / B-8`）**：`401` 无/错 token；`503` 未配置任何 admin/client token（fail-closed）；GET 不设 CSRF，故无 403。
- **tailnet read-grant 旁路（既有语义，不新增）**：启用 `CONSOLE_TAILNET_TRUST` 时，受信网段/受信 Host 的 **GET 读请求**可按既有 `read_only_get` 语义匿名通过——该旁路天然覆盖本期新增的 `GET /api/v1/stats` 与 `GET /api/v1/ingest/stats`（不改 trust 实现、不放宽任何写接口，见 `.dev-team/contract-tailnet-trust.md`）。

### C3.4 `envelope=1` 响应结构

```json
{"items": [], "total": 0, "limit": 50, "offset": 0, "has_more": false,
 "archived": "exclude",
 "filters": {"project": null, "status": null, "level": null,
             "environment": null, "platform": null, "release": null}}
```

| 字段 | 类型 | 判据 |
| :--- | :--- | :--- |
| `items` | Issue[] | 长度 `<= limit` |
| `total` | int | 与 `X-Total-Count` 相等 |
| `limit`/`offset` | int | 与 `X-Limit`/`X-Offset` 相等 |
| `has_more` | bool | 与 `X-Has-More` 相等 |
| `archived` | string | 与 `X-Archived-Mode` 相等 |
| `filters.*` | string\|null | 生效过滤值，缺省 `null`（空串归一 `null`） |

- 响应头在 `envelope=1` 时同样下发（两处判据可交叉验证）。

---

## C4 单 issue 事件分页（`GET /api/v1/issues/:id/events`）

### C4.1 Query 参数

| 参数 | 类型 | 默认 | 非法值判定 |
| :--- | :--- | :--- | :--- |
| `limit` | int | **20** | `0` 或负数 → **400 `Bad request: invalid limit`**；`>100` → clamp 到 100；非整数 → 400 |
| `cursor` | base64url 串 | 缺省（首页） | 解码失败 / 非 UTF-8 / 非 JSON / 缺字段 / 字段类型不符 / 多出未知字段 / 长度 > 512 字符 → **400 `Bad request: invalid cursor`**（篡改必 400） |
| `order` | enum | **`desc`** | 非 `desc|asc` → 400 `Bad request: invalid order` |
| `envelope` | 0\|1 | 缺省（裸数组） | 其他 → 400 `Bad request: invalid envelope` |

### C4.2 排序与游标（keyset，**不是 offset**）

- 排序：`ORDER BY created_at DESC, id DESC`；`order=asc` 时同升序（`created_at ASC, id ASC`）。
- 游标载荷（解码后形态，写死；`amend v1.1 / B-2`）：`{"created_at": "<该行 created_at 的原始 RFC3339 序列化>", "id": "<id>"}`，UTF-8 JSON 经 **base64url（无 padding）** 编码即为 `cursor` 串。
  - `created_at` 必须是**库内该行 `created_at` 的原始串**（不做任何截断/量化/重新格式化）；**禁止把 `created_at` 截断到微秒**（A-BLOCKING#5 的根因）。
  - 比较按方言原生类型：SQLite 侧对 `TEXT` 做字典序比较（`(created_at, id) < ($1, $2)`）；PG 侧对 `timestamptz` 做时间比较。
  - **零变更项**：SQLite `events.created_at` 的存储格式**不改**（C7.1 清单内），故游标必须携带原始串而非重新生成的秒/微秒时间戳。
- 取页条件（`order=desc`）：`(created_at, id) < (cursor.created_at, cursor.id)` 的行（字典序元组比较）；`asc` 反向。
- 翻页**不重不漏**：对 N 行事件连续翻页，各页 id 集合两两不相交，并集大小 == N（同一 `created_at` 的多行也必须稳定不重叠——由 `id` 次级键保证）。

### C4.3 响应

- 默认响应体：bare JSON array（`[...]`，与 F19 的 events 消费点一致）。
- 响应头：

| 响应头 | 取值 | 判据 |
| :--- | :--- | :--- |
| `X-Next-Cursor` | 不透明串 | **仅当存在下一页**时下发；无下一页时**不得**出现该头 |
| `X-Has-More` | `true`\|`false` | 存在下一页为 `true` |
| `X-Limit` | 十进制整数 | 生效 limit（clamp 后） |
| `X-Total-Count` | 十进制整数 | 该 issue 的事件**总行数**（`amend v1.1 / B-4`），与 `cursor`/`limit` **无关**、稳定不变 |

- `envelope=1`：`{"items": [], "next_cursor": null, "has_more": false, "limit": 20, "total": 0}`（`items` 为 Event[]，`next_cursor` 为 string\|null，`total` 与 `X-Total-Count` 相等 —— `amend v1.1 / B-4`）。
- 交叉判据：任意一页的 `X-Total-Count` 与 `envelope=1` 的 `total` 相等，且等于 `count_events_for_issue(id)`（翻页不重不漏的断言基准，A4 补判据据此可编码）。

### C4.4 边界（明记）

| 场景 | 判据 |
| :--- | :--- |
| issue 不存在 | **保持现状 200 + `[]`**（F7；不改 404）；同时 `X-Total-Count: 0`、`X-Has-More: false`、无 `X-Next-Cursor` |
| issue 存在但无事件 | 200 + `[]`，`X-Total-Count: 0`，`X-Has-More: false`，无 `X-Next-Cursor` |
| `limit` 大于剩余行数 | 200 + 剩余行，`X-Has-More: false` |
| `cursor` 指向已不存在的行 | 200，按 keyset 条件继续；不得 404/500 |
| `X-Next-Cursor` 只在有下一页时出现 | 断言 `header is none` |

---

## C5 统计接口 `GET /api/v1/stats`

- 鉴权：挂在 `admin_routes`（F1），沿用 admin 鉴权口径（C2.3 同）。
- Query：

| 参数 | 类型 | 默认 | 非法值判定 |
| :--- | :--- | :--- | :--- |
| `project` | string | 缺省 = 全项目 | 空串视同缺省 |
| `environment` | string | 缺省 | 空串视同缺省 |
| `level` | string | 缺省 | 非 `fatal|error|warning|info` → 400 `Bad request: invalid level` |
| `from` | RFC3339 | `to - 30d` | 非法 → 400 `Bad request: invalid from` |
| `to` | RFC3339 | `now` | 非法 → 400 `Bad request: invalid to`；`from > to` → **400 `Bad request: invalid range`** |
| `bucket` | enum | `day` | 非 `day|hour` → 400 `Bad request: invalid bucket`；`hour` 且 `to-from > 7d` → **400 `Bad request: range too large for hour bucket`** |
| `include_archived` | 0\|1 | `0` | 其他 → 400 `Bad request: invalid include_archived` |

- 失败码（`amend v1.1 / B-6`+`B-8`）：`400`（非法 query）/ `401` / `503`（未配置 token）；`500` 响应体**固定 `Internal error`**（DB 原文只进日志，禁止回吐）；GET 无 CSRF 故无 403；tailnet read-grant 匿名读旁路见 C3.3（既有语义，不新增）。

- 200 响应（字段名与类型**写死**）：

```json
{"project": null,
 "range": {"from": "2026-09-25T00:00:00Z", "to": "2026-10-25T00:00:00Z", "bucket": "day"},
 "include_archived": false,
 "issues": {"total": 0, "by_status": {}, "by_level": {}, "by_environment": {}},
 "events": {"total": 0, "basis": "sum_issues_count", "stored_rows": 0,
            "by_level": {}, "by_environment": {}, "by_release": {}},
 "series": [{"bucket": "2026-10-25", "issues": 0, "events": 0}]}
```

| 字段 | 类型 | 判据（口径写死） |
| :--- | :--- | :--- |
| `project` | string\|null | 回显生效 project（空串/缺省 → `null`） |
| `range.from` / `range.to` | RFC3339 string | 回显生效区间（`to` 默认 `now`，`from` 默认 `to-30d`） |
| `range.bucket` | `day`\|`hour` | 回显 |
| `include_archived` | bool | 回显；`false` 时 issue 聚合只计 `archived_at IS NULL`，行口径只计「其 issue 未归档」的 events 行（C5.0） |
| `issues.total` | int | 过滤条件下 **issues 行数**（默认排除 archived；**不受 `from`/`to` 影响**，C5.0） |
| `issues.by_status` | `{string:int}` | 键 = `unresolved\|in_progress\|resolved\|ignored\|regression`；**木桶之和 == `issues.total`**；全时段口径 |
| `issues.by_level` | `{string:int}` | 木桶之和 == `issues.total`；全时段口径 |
| `issues.by_environment` | `{string:int}` | 木桶之和 == `issues.total`；全时段口径 |
| `events.total` | int | **`SUM(issues.count)`**（同 issues 过滤与 archived 口径；**不受 `from`/`to` 影响**）；`basis="sum_issues_count"`；生产 A8 值 185 |
| `events.basis` | 常量 string | **恒为 `"sum_issues_count"`**（不得出现其他值） |
| `events.stored_rows` | int | **`COUNT(*) FROM events`**（同过滤 + **受 `from`/`to`**，时间窗作用于 `events.created_at`；无过滤时 == events 表行数，生产 196） |
| `events.by_level` | `{string:int}` | **events 行口径** + `from`/`to`，按 `events.level`（NULL 归 `unknown`） |
| `events.by_environment` | `{string:int}` | events 行口径 + `from`/`to`，按 `events.environment`（NULL 归 `unknown`） |
| `events.by_release` | `{string:int}` | events 行口径 + `from`/`to`，按 `events.release`（NULL 归 `unknown`） |
| `series` | 数组 | 见下条（受 `from`/`to`） |

#### C5.0 作用域表（`amend v1.1 / B-1`，逐字段写死；与上表冲突时以本表为准）

| 字段 | 受 `project`/`environment`/`level` | 受 `include_archived` | 受 `from`/`to` | 取值键 / 口径 |
| :--- | :--- | :--- | :--- | :--- |
| `issues.total` / `issues.by_status` / `issues.by_level` / `issues.by_environment` | 是 | 是 | **否（全时段口径）** | issues 行 |
| `events.total`（`SUM(issues.count)`） | 是 | 是 | **否（全时段口径）** | `SUM(issues.count)`，`basis="sum_issues_count"` |
| `events.stored_rows`（`COUNT(*)`） | 是 | 是 | **是**（`events.created_at ∈ [from, to]`） | events 行 |
| `events.by_level` / `events.by_environment` / `events.by_release` | 是 | 是 | **是**（同 `stored_rows`） | events 行 + `events.created_at` |
| `series[].issues` | 是 | 是 | 是 | **`issues.first_seen_at`** |
| `series[].events` | 是 | 是 | 是 | **`events.created_at`** |

- `include_archived=false` 的精确语义（`B-1`）：issue 聚合只计 `archived_at IS NULL` 的 issue；**行口径**只计「其所属 issue 未归档」的 events 行（`events JOIN issues ON events.issue_id = issues.id` 且 `issues.archived_at IS NULL`）。
- 时间过滤只作用于 **`first_seen_at`（issues 侧）** 与 **`events.created_at`（events 侧）**；issues 行数/计数聚合**不随 `from`/`to` 变化**（否则 A8 生产值与默认 30 天窗口会互相矛盾）。
- 不变式（`B-1`，Test Agent 必断言）：
  1. `sum(series[].events) == events.stored_rows`（两者同受 `from`/`to` 约束，**恒等**；宽窗口时等于 events 全表行数）；
  2. `sum(series[].issues) <= issues.total`（前者限时间窗，后者全时段）；
  3. `sum(issues.by_status.values()) == issues.total`、同 `by_level`、`by_environment`。
- **生产验收硬值（固定）**：`project=pony-agent`、默认参数（`include_archived=0`、`from=to-30d`、`bucket=day`）→ `issues.total=11`、`events.total=185`、`events.stored_rows=196`、`by_status={ignored:7,resolved:2,unresolved:2}`、`by_level={error:11}`、`by_environment={dev:11}`。

- **口径差异必须显式保留，禁止混用**：`events.total`（聚合口径 = `SUM(issues.count)`）与 `events.stored_rows`（行口径 = `COUNT(*)`）在生产库分别为 **185** 与 **196**；差 11 行的成因是**历史重放写入的 events 行数 > 聚合计数**（重复/补投写行不递增 count）。两者不得相加、不得互相替代、不得用其中一个"修正"另一个。
- `series`：
  - `bucket` 键格式：`bucket=day` → `YYYY-MM-DD`；`bucket=hour` → `YYYY-MM-DDTHH:00:00Z`（UTC）；
  - 桶区间：`[from, to]` 闭区间对齐到桶起点，**空桶补 0**，桶数 == `series.len()`；
  - `series[].issues`：按 **`issues.first_seen_at`** 落桶计数（F15：issues 无 created_at）；
  - `series[].events`：按 **`events.created_at`** 落桶计数（行口径）；
  - 点数上限 `stats_series_max_points = 744`（见 NFR 基准表）：day 桶 30 天默认 → 31 点；hour 桶 7 天 → 168 点；任何情况下超 744 点 → 400。
- 不变式（Test Agent 必须断言）：`sum(issues.by_status.values()) == issues.total`、同 `by_level`、`by_environment`；`series.len() >= 1`；每个 `series[].bucket` 可被 `%Y-%m-%d`（day）或 RFC3339（hour）解析。

---

## C6 治理指标 `GET /api/v1/ingest/stats`

### C6.1 查询与响应

- 鉴权：挂 `admin_routes`（沿用 admin 口径：静态 `admin_token`，未设则回退 `client_token`；会话 Cookie/Bearer + CSRF）。
- 失败码（`amend v1.1 / B-6`+`B-8`）：`400`（非法 query）/ `401` / `503`（未配置 token）；`500` 响应体**固定 `Internal error`**（DB 原文只进日志）；GET 不设 CSRF 故无 403；tailnet read-grant 匿名读旁路见 C3.3（既有语义，不新增）。
- Query：

| 参数 | 类型 | 默认 | 非法值判定 |
| :--- | :--- | :--- | :--- |
| `window` | enum | `24h` | 非 `24h|7d|30d` → **400 `Bad request: invalid window`** |
| `project` | string | 缺省 = 全项目 | 空串视同缺省 |
| `bucket` | enum | `day` | 非 `day|hour` → 400 `Bad request: invalid bucket` |

- 200 结构（字段名/类型写死；`24h` 窗口用 `bucket=hour` 时 `series` 最多 24 点，`day` 时 1 点）：

```json
{"window": "24h", "from": "2026-10-24T00:00:00Z", "to": "2026-10-25T00:00:00Z", "bucket": "day",
 "totals": {"received": 0, "accepted": 0, "created": 0, "duplicate": 0,
            "suppressed_ignored": 0, "rejected_invalid": 0},
 "by_project": [{"project": null, "received": 0, "accepted": 0, "suppressed_ignored": 0}],
 "series": [{"bucket": "2026-10-25", "received": 0, "accepted": 0, "suppressed_ignored": 0}]}
```

| 字段 | 类型 | 判据 |
| :--- | :--- | :--- |
| `window` | `24h`\|`7d`\|`30d` | 回显 |
| `from` / `to` | RFC3339 | `to = now`，`from = to - window` |
| `bucket` | `day`\|`hour` | 回显 |
| `totals.received` | int | `outcome='received'` 之和 |
| `totals.created` | int | `outcome='created'` 之和 |
| `totals.duplicate` | int | `outcome='duplicate'` 之和 |
| `totals.accepted` | int | **恒等于 `created + duplicate`**（派生字段，不单独落库） |
| `totals.suppressed_ignored` | int | `outcome='suppressed_ignored'` 之和 |
| `totals.rejected_invalid` | int | `outcome='rejected_invalid'` 之和 |
| `by_project[].project` | string\|null | 库内 `''` → JSON **`null`** |
| `by_project[].received/accepted/suppressed_ignored` | int | 同口径按 project 分组 |
| `series[].bucket` | `YYYY-MM-DD`（day）\| `YYYY-MM-DDTHH:00:00Z`（hour） | 空桶补 0 |
| 排序 | `by_project` 按 `project` 升序（`null` 最后）；`series` 按时间升序 | 稳定可断言 |

### C6.2 计数语义（**不改变任何 ingest 现有行为/响应/状态码**，纯旁路）

| 计数 | 递增时机（精确） | 不计数 |
| :--- | :--- | :--- |
| `received` | `POST /api/v1/ingest` **通过鉴权之后立即 +1**（每次请求一次） | 鉴权失败 401 不计 |
| `created` | 写入成功且 `is_duplicate == false` | 幂等命中不计 |
| `duplicate` | 写入成功且 `is_duplicate == true` | 新建不计 |
| `accepted` | 派生 = `created + duplicate`（读接口计算，不落库） | — |
| `suppressed_ignored` | **写入成功**后：该 issue 在本次写入**之前**的 `status == 'ignored'` → 额外 +1 | `resolved`→`regression` 跃迁不计入本桶（走 `created`）；ingest 对 ignored issue **仍写 events 且 count+1，不拦截**（F17） |
| `rejected_invalid` | 请求体不合法导致的 **400 / 413** | 鉴权 401、服务端 5xx、410 不计 |

- 计数写入失败**只 `warn!` 不阻断** ingest（`ingest_metrics_failure_policy = warn_only_no_ingest_impact`）：ingest 的响应体与状态码（201/200/400/401/413/410）与 `received` 计数无关，**零变更**。
- 落库方式：每事件一次 UPSERT（单语句，两方言等价，`amend v1.1 / B-3`）：
  - **PG**：`INSERT INTO ingest_metrics (bucket_date, project, outcome, count, updated_at) VALUES ($1,$2,$3,1,now()) ON CONFLICT (bucket_date, project, outcome) DO UPDATE SET count = ingest_metrics.count + 1, updated_at = now()`
  - **SQLite**：`INSERT INTO ingest_metrics (bucket_date, project, outcome, count, updated_at) VALUES (?1,?2,?3,1,?4) ON CONFLICT(bucket_date, project, outcome) DO UPDATE SET count = count + 1, updated_at = excluded.updated_at`（`?4` = RFC3339 UTC 串；SQLite 的 `ON CONFLICT` 目标列表**必须**与主键列同序）
- `bucket_date` = 计数发生时刻的 **UTC 日期**（PG `DATE` / SQLite `YYYY-MM-DD` 文本）。
- 无 project 维度落 `project=''`（读接口把 `''` 映射为 `null`）。
- **计数不变式（`B-3`，单写者下必断言）**：
  1. `received == created + duplicate + rejected_invalid`（`suppressed_ignored` 是 `accepted` 的**子集**，不参与该等式）；
  2. `received >= created + duplicate` **恒成立**（含并发/丢计数场景）。
- **丢计数策略**：`SQLITE_BUSY`/锁超时等导致计数写入失败的场景**允许丢计数**，但必须 `warn!` 并带上 `outcome` 与本次丢失的计数（1）；不得因计数失败改变 ingest 结果。
- 计数写入独立超时 `ingest_metrics_write_timeout_ms = 1000`（见 NFR 基准表）：单次 UPSERT 超过该值即按失败处理（warn，不阻断）；`ingest_metrics_max_added_latency_ms = 3000` 为 ingest 请求因治理计数而增加的**总延迟上限**（超出即说明实现需把计数移出关键路径/降级）。
- 批量缓冲/聚合写合并**列为遗留风险**（非本次实现）：每事件一次 UPSERT 的写放大由后续波次处理，本文冻结为「每事件一次 UPSERT」。
- 与既有 `/metrics`（F18，进程内内存计数器）**并存且不冲突**：治理指标是 **DB 持久化的按日/按 project 聚合**；本契约**不改** `/metrics` 的任何输出（不加、不改指标名/标签/HELP）。

---

## C7 兼容性与回归（零变更清单 + breaking 登记）

### C7.1 零变更（禁止触碰）

| 项 | 判据 |
| :--- | :--- |
| fingerprint v3 | 指纹计算不改；同 fingerprint 不同 event_id → issue 数不变、`count` +1（A10） |
| **fingerprint v3 不含 `project`**（`amend v1.1 / B-12`，F23） | project 由**首报**决定并保存在 `issues.project`；同一错误跨项目会并入**同一 issue**。故 A1 的生产判据成立依赖既有数据分布；**测试造数必须靠栈帧/`error_type` 区分指纹**，不得假定 project 参与指纹 |
| `event_id` 幂等键 | 同 event_id 二次 POST → 201 后 200 duplicate，`count` 不增（A10） |
| `uq_events_event_key` 唯一约束 | 不删、不改 |
| ingest 响应字段与状态码 | `201/200/400/401/413/410` 与响应体字段（`result/status/issue_id/event_id/fingerprint/count/duplicate/warnings`）**零变更**（C2.5 的「活跃即解档」只追加一条同事务 UPDATE，不改响应） |
| `POST /api/v1/ingest` 路径与请求体 | 零变更（不新增必填字段，不改解析） |
| `events.created_at` 存储格式（SQLite） | **零变更**（`amend v1.1 / B-2`）：游标必须携带库内原始 RFC3339 串，禁止改存储格式或截断精度 |
| `/metrics` 输出 | 零变更（C6.2） |
| traces 全部契约 | `.dev-team/trace-fix-contract.md` v1.6 持续有效，本次不改 |

### C7.2 回归门禁

- `cargo test --workspace` → **exit 0**，套件数与基线一致（`.dev-team/regression-baseline-wave9.json`，22 套件，F21）。
- `cargo check --workspace --offline` → exit 0；`cargo clippy --workspace --all-targets --offline -- -D warnings` → exit 0（本期空桩交付收据）。
- 新增黑盒契约套件 `tests/acceptance_issue_governance.rs` 与 `tests/acceptance_issue_governance_adversarial.rs` 为红相基线，
  实现后转绿；两套件**必须**在 `tests/Cargo.toml` 的 `[[test]]` 中登记（该包 `autotests = false`，未登记则不会被执行）；
  **不得**用 `#[ignore]` 或吞错放行。

### C7.3 breaking 登记（显式，含消费者普查证据）

| # | breaking 项 | 旧行为 | 新行为 | 消费者普查结论 |
| :-- | :--- | :--- | :--- | :--- |
| B1 | `GET /api/v1/issues` 非法 `status` | 静默忽略（返回全集，F5：`routes.rs:1559`） | **400 `Bad request: invalid status`** | 控制台**不传** `status`/`level`/`limit`/`offset`（`web/src/App.vue:1631-1667`），status/level 在浏览器侧过滤 → **零现存调用方受影响**；`grep -rn "api/v1/issues" web/src` 仅 3 处：`App.vue:1664`（列表，只拼 platform/project/environment）、`App.vue:1704`（PATCH 单条）、`App.vue:1734`（events） |
| B2 | 列表默认新增排除 archived | 全部行 | 默认 `archived=exclude` | 数据未归档前结果集**逐行不变**（所有行 `archived_at IS NULL`）；`web/src/App.vue:1664-1669` 仍拿到裸数组，长度语义不变 |
| B3 | 事件排序次级键 | `ORDER BY created_at DESC`，同 `created_at` 相对顺序**不稳定**（F16） | `ORDER BY created_at DESC, id DESC`（asc 反向）→ 相对顺序**确定** | 可观察行为修正（非破坏）：同 `created_at` 事件的返回顺序由不确定变为确定；`web/src/App.vue:1734-1738` 只渲染不依赖顺序 |
| B4 | 列表 `limit`/`offset` 非法值 | 静默 clamp/忽略 | `limit<0`、`offset<0` → 400 | 控制台不传 `limit/offset`（F20）→ 无影响 |
| B5 | `/api/v1/issues/archive` 静态段抢占（`amend v1.1 / B-8`） | 该路径不存在 → `PATCH/GET /api/v1/issues/archive` 落入 `:id` 分支（404/405 语义混乱） | **`PATCH` 与 `GET` 均 405**（静态段优先，只有 `POST` 已注册）；列表/单条 PATCH 路径不受影响 | 控制台单条 PATCH 用真实 UUID（`web/src/App.vue:1704`），不撞保留字 `archive`；测试不得据此判红 |
| B6 | 新增接口失败码面（`amend v1.1 / B-8`） | 新接口此前 404 | 归档三接口：400/401/403/404/413/500/503；`GET /stats`、`GET /ingest/stats`：400/401/500/503（GET 无 403）；**5xx 响应体固定 `Internal error`** | 全新接口，无存量调用方；tailnet read-grant（`read_only_get`）对两个新 GET 同样生效（不新增旁路） |

- 回滚面与**回滚顺序**（`amend v1.1 / B-9`，契约必需项，见 C1.3）：`停止服务 → 恢复上一版二进制 → 执行 down.sql → 启动 → 验证`。归档**不删数据**，数据面无不可逆操作；SQLite 无 down，回滚走 C1.3 的重建表流程。**顺序反了会让旧二进制全量 500，属门禁失败。**

---

## A1–A10 验收判据（Test Agent 冻结红相口径，原文照抄）

- A1 列表过滤：`GET /api/v1/issues?project=pony-agent&status=unresolved` 只返回 unresolved，结果中无 ignored 项。
- A2 列表 total 语义：`X-Total-Count` == 同过滤条件总数且不随 limit/offset 变化；`?envelope=1` 的 items 长度 ≤ limit 且 total 不受 limit 影响；缺省 project 时 `X-Project-Filter: all`。
- A3 archived 过滤：归档后默认列表不含该 issue；`archived=only` 只含归档项；`archived=include` 两者都含。
- A4 单条归档/恢复：归档后 `archived_at` 非空且 `count`、events 行数与 status 均不变；重复归档幂等；unarchive 后 `archived_at=null`。
- A5 批量归档：`dry_run=1` 只返回 matched 不改库；真实执行返回 archived 数；`statuses=["unresolved"]` → 400；`ids` 中含 unresolved → 计入 skipped_not_archivable。
- A6 TTL 自动归档：`PONY_ISSUE_ARCHIVE_TTL_SECS=1` + `INTERVAL_SECS=1` 下，被置为 ignored 的 issue 在 ~3s 内自动归档；unresolved issue 永不被自动归档；`PONY_ISSUE_ARCHIVE_ENABLED=0` 时作业不跑。
- A7 事件分页：`/issues/:id/events?limit=5` 恰好 5 条；`envelope=1` 返回 `next_cursor`；用 cursor 翻页无重叠无遗漏且总数等于事件行数；`limit=0` → 400；篡改 cursor → 400。
- A8 统计：`/api/v1/stats?project=pony-agent` 在生产库返回 `issues.total=11`、`events.total=185`、`by_status={ignored:7,resolved:2,unresolved:2}`、`by_environment={dev:11}`、`by_level={error:11}`、`events.stored_rows=196`；测试库断言分组木桶之和 == total，series 桶可解析。
- A9 治理指标：新事件上报 → `received`、`created` 各 +1；同 event_id 重放 → `received`+1、`duplicate`+1、`created` 不变；对 ignored issue 上报 → `suppressed_ignored`+1。
- A10 幂等零回归：同 event_id 两次 POST → 201 created 后 200 duplicate 且 issue.count 不增；同 fingerprint 不同 event_id → issue.count +1 且 issue 数不变。

### A 条款补充判据（不覆盖上表，仅补机器断言细节）

| 条款 | 补充断言（可直接编码） | 依据 |
| :--- | :--- | :--- |
| A1 | 造数**必须**靠栈帧/`error_type` 区分指纹（fingerprint v3 **不含** project，F23/B-12）；不得假定 `project` 参与指纹 | C7.1 |
| A2 | 同一过滤条件下 `limit=1` 与 `limit=100` 两次请求的 `X-Total-Count` 相等；`X-Has-More` 与 `offset+len<total` 一致 | C3.3 |
| A3 | `archived=only` 结果每项 `archived_at != null`；`archived=include` 结果数 == `archived=exclude` 数 + `archived=only` 数 | C3.2 |
| A4 | 归档前后两次 `GET /issues/:id/events?limit=100` 的 **`X-Total-Count`**、`envelope=1` 的 `total`、以及翻页并集大小三者相等（B-4） | C2.1/C4.3 |
| A5 | `statuses=["regression"]` → 400；`statuses` 含重复元素或 >2 项 → 400；`ids` 与 `statuses` 同给时 `criteria.ids_take_priority=true`（= 候选集由 ids 决定），**`statuses` 仍作白名单**：`{"ids":[A,B],"statuses":["ignored"]}` 只归档 A/B 中 `ignored` 的行、其余计入 `skipped_not_archivable`（v1.2）；`criteria.statuses` == 生效值（未给 → `["ignored","resolved"]`，给了 → 原值去重后；A-4）；单写者下 `archived + skipped_not_archivable + skipped_already_archived + skipped_missing == matched` | C2.3/C2.4 |
| A6 | `INTERVAL_SECS=1`（最小 1，非 60）可在 ≤10s 窗口内确定性验证；`TTL_DAYS=0` → 回落 30 天且日志含 `warn`；手动 `unarchive` 且期间无新事件 → 自动作业**不再**归档（A-1） | C2.2/C2.1 |
| A7 | 无下一页时响应**不含** `X-Next-Cursor`；`cursor` 长度 > 512 → 400；`cursor` 载荷为原始 RFC3339 串（B-2），同 `created_at` 多行翻页不重叠 | C4.3 |
| A8 | `events.basis == "sum_issues_count"`；`sum(by_status)==total`、`sum(by_level)==total`、`sum(by_environment)==total`；`sum(series[].events) == events.stored_rows`（同受 from/to） | C5.0 |
| A9 | `totals.accepted == totals.created + totals.duplicate`；`received == created + duplicate + rejected_invalid`（单写者）；401 请求不使 `received` 增长；5xx 不使 `rejected_invalid` 增长 | C6.2 |
| C2.5 | 归档的 `ignored` issue 收到新事件后：ingest 状态码/响应字段集合与未归档时逐字段一致，`archived_at` 变为 `null` 且 `archive_restored_at` 为 `null` | C2.5 |

---

## C8 非目标（本期不做）

- 不做 issue 合并/去重策略变更、不改 fingerprint 方案、不引入 case 管理/webhook。
- 不做 stats 的缓存层、不做 ingest 计数批量缓冲（列为遗留风险，见 C6.2）。
- 不改 `/metrics` 输出、不改 traces 侧任何契约。
- 不提供 SQLite down 迁移（C1.3）。

## 交付物与门禁（本期协议层）

| 交付物 | 判据 |
| :--- | :--- |
| `.dev-team/contract-issue-governance.md` | 本文件（amend v1.1）；C1–C7 + A1–A10 齐备，全部判据已定值 |
| `.dev-team/nfr-baseline.json` | `python3 /home/dm/.agents/skills/dev-team/scripts/dev-nfr-lint.py .dev-team/nfr-baseline.json` → **Exit 0**；含 `issue_archive`、`issue_events_page_limit_max`、`issue_list_limit_max`、`ingest_metrics_failure_policy`、`stats_series_max_points`、`ingest_metrics_write_timeout_ms`、`ingest_metrics_max_added_latency_ms`、`archive_job_first_run_after_migrate`、`archive_job_failure_backoff_secs`、`archive_restore_persistence` |
| 空桩（`crates/core/src/models.rs`、`crates/core/src/repository.rs`、`crates/server/src/routes.rs`） | 仅类型/签名/路由占位，业务体一律 `unimplemented!("implemented by wave-9 executor")`；`cargo check --workspace --offline` → **Exit 0** |

**空桩纯度：Rust 不适用 dev-stub-lint.py，由 Lead 人工 review + `cargo check` Exit 0 替代。**

## amend 记录

- **v1.0（2026-10-25，Protocol Agent 落笔，Lead 已裁决）**：初版冻结。含 Lead 侦察事实 F1–F22（含 Lead 追加的 F14/F15/F16/F17/F18 与 F19/F20 消费者证据）。
- **Lead 裁决并入（同日，随 v1.0 一同冻结）**：
  1. `PONY_ISSUE_ARCHIVE_INTERVAL_SECS` **最小值 = 1 秒**（默认 3600；生产取值 3600）——理由：A6 需在黑盒门禁内确定性验证（TTL_SECS=1 + INTERVAL_SECS=1，≤10s 窗口）。
  2. **不**引入「每次 ingest 后惰性触发归档」；归档仅「启动立即一轮 + 固定周期」。
  3. `ingest_metrics.outcome` **不加 PG `CHECK` 约束**，取值由应用层白名单（C1.5）约束。
  4. 批量归档 `statuses` 默认 `["ignored","resolved"]`；`ids` 与 `statuses` 同给时 **`ids` 优先、`statuses` 被忽略**（C2.3/C2.4）。
  5. `regression` 显式列入 `never_auto_archive_statuses` 与 `skipped_not_archivable` 判据（C2.1/C2.3、NFR 基准表）。
  6. 本文 `Issue.archived_at` 字段的 **8 处结构体字面量补齐**归属 executor（Protocol Agent 写域外，见 C1.4/F22）。

- **v1.1（2026-10-25，Lead 依据 Gate-1 方案逆向对抗裁决；收据 `.dev-team/review-plan-a.json` / `review-plan-b.json` 均过 dev-review-lint Exit 0）**：
  本节为**判据级修订**，与 v1.0 冲突处以 v1.1 为准；裁决方向不得变更，实现侧若认为不可行只能在「实现风险登记」标注。

  | 裁决 | 对应 finding | 落的契约位置 |
  | :--- | :--- | :--- |
  | A-1 `archive_restored_at` + 自动归档抑制项 + 手动归档不覆盖 | A-BLOCKING#1（unarchive 无持久性） | C1.1/C1.2/C1.4、C2.1、C2.2、C2.3 |
  | A-2 活跃即解档（ingest 同事务追加 UPDATE） | A-BLOCKING#3（活跃僵尸） | C2.5、C7.1 |
  | A-3 批量归档计数口径重写（statuses 模式 `matched≡archived`；ids 模式先分桶） | A-BLOCKING#2（matched 不变式） | C2.3、C2.4 |
  | A-4 `criteria.statuses` = 实际生效白名单 | B-BLOCKING#2 | C2.4、A5 补判据 |
  | B-1 C5 作用域表（逐字段 from/to 与 archived 作用面）+ 生产硬值 | A-BLOCKING#4（C5 口径未闭合） | C5/C5.0 |
  | B-2 游标载荷改原始 RFC3339 串（禁止截断微秒） | A-BLOCKING#5（cursor 精度） | C4.2、C7.1、F24 |
  | B-3 双方言 UPSERT SQL + 计数不变式 + SQLITE_BUSY 丢计数 warn | A-BLOCKING#6（UPSERT 方言/不变式） | C6.2 |
  | B-4 事件 `X-Total-Count` + 信封 `total` | B-BLOCKING#1 | C4.3、A4 补判据 |
  | B-5 批量归档硬上限（ids≤1000、单 id≤128、statuses≤2 且去重、64KB body→413、全绑定参数） | B-BLOCKING#3 | C2.3 |
  | B-6 所有 5xx 响应体固定 `Internal error` | B-BLOCKING#4 | C2.3、C5、C6.1、C7.3(B6) |
  | B-7 治理读路径索引（`events(issue_id,created_at,id)`、`events(created_at)`、`issues(first_seen_at)`） | ADVISORY（索引补强） | C1.1/C1.2 |
  | B-8 表头作用域收敛 + `PATCH /issues/archive`→405 登记 + 403/503 失败码 + tailnet read-grant 旁路说明 | ADVISORY（契约闭合/安全面） | C3.3、C6.1、C7.3(B5/B6) |
  | B-9 回滚顺序升为契约必需项（停服→回退二进制→down.sql→启动→验证） | ADVISORY（安全/运维） | C1.3、C7.3 |
  | B-10 首轮在迁移成功返回后启动；失败退避 `min(INTERVAL_SECS, 60)` | A-ADVISORY#8 | C2.2 |
  | B-11 NFR 追加 5 键 | 裁决 | `.dev-team/nfr-baseline.json` |
  | B-12 fingerprint v3 不含 project（造数靠栈帧/error_type 区分） | 事实补充 | F23、C7.1、A1 补判据 |

  **实现风险登记（不改变任何判据，仅提示 executor/Test 侧注意）**：
  1. **B-2（风险最高）**：SQLite 对 `created_at` 文本做**字典序**比较，仅当库内格式统一（同一时区写法 `Z`、同一小数位）时才等价于时间序；存量行若存在变体格式（`+00:00`、不同精度）会导致分页错序/漏行。判据不变，但 executor 必须先核对存量格式一致性（可加一次性校验日志/脚本），Test 侧翻页用例须覆盖"同 created_at 多行 + 跨秒边界"。
  2. **B-5 / A-3（风险高）**：`ids` 上限 1000 且要求 `= ANY($1)` 全绑定参数；SQLite 绑定变量上限（默认 999）不足 → 必须分批（如每批 ≤500）或改用 JSON 数组展开，否则 A5 在大 ids 集合下会报错而非按契约分桶。
  3. **B-1（风险中）**：`sum(series[].events) == events.stored_rows` 要求 series 落桶与 `stored_rows` **共用完全相同**的 WHERE（含 `events JOIN issues` 的 archived 过滤与 from/to 边界取舍）；两侧若分别拼 SQL 极易出现 ±1 行差异（闭区间边界）。
  4. **A-2（风险中）**：追加 UPDATE 必须复用 ingest 既有事务；两种方言 repo 的事务边界不同，SQLite 下不得新开事务（会与既有 `BEGIN IMMEDIATE` 冲突）。
  5. **B-3 SQLITE_BUSY 丢计数（风险中）**：该路径难以确定性触发，黑盒不可测 → 靠单测注入 + review 收据，门禁不强制黑盒覆盖。

- **v1.2（2026-10-25，Lead 裁决；来源 = Test Agent 红相阶段发现的 C2.3/C2.4 内部语义冲突，Test Agent 拒绝自行裁决并上报）**：
  冲突原文：C2.3 写「`ids` 与 `statuses` 同给时 ids 优先、`statuses` 被忽略」，但 `ids` 模式第 2 步 UPDATE 的条件是 `AND status = ANY($2=生效白名单)`——两者在 `{"ids":[A,B],"statuses":["ignored"]}` 下给出不同实现。
  **裁决（写死）**：
  1. `ids` 决定**候选集**（只处理这些 id；`project`/`older_than_days` 不参与筛选）；`statuses` **仍然作为归档白名单约束**——`{"ids":[...],"statuses":["ignored"]}` 只归档其中 `status='ignored'` 的行，其余计入 `skipped_not_archivable`。
  2. `criteria.statuses` = 生效白名单（未给 → `["ignored","resolved"]`；给了 → 原值**去重后**）；`criteria.ids_take_priority=true` 的语义改为「候选集由 `ids` 决定（`project`/`older_than_days` 被忽略）」，**不再表述为 `statuses` 被忽略**。
  3. `ids` 模式单写者等式不变：`archived + skipped_not_archivable + skipped_already_archived + skipped_missing == matched`，其中 `archived` 只计「status ∈ 生效白名单 且未归档」的行。
  改动范围：仅 C2.3（优先级 bullet + `ids` 模式第 2 步说明）、C2.4（`criteria.statuses` / `criteria.ids_take_priority` 两行）、A 条款补充判据 A5 行、本 amend 记录；**未改**任何代码、NFR 基准与其它段落。
