# Wave-9 交付：issue 治理与分析接口（归档 / 列表 / 事件分页 / 统计 / 治理指标）

- 分支：`feat/issue-governance`；提交：`a71a15c`（红相冻结）→ `35d7c5c`（实现）→ `5e96c82`（文档/收据）
  与 `c777727`（契约 v1.3）
- 人读契约：`docs/issue-governance.md`；冻结实施契约：`.dev-team/contract-issue-governance.md`（v1.3）
- 决策记录：`.agents/notes/implemented/feature/2026-10-25-issue-governance-archive-and-analytics.md`
- 生产实测时间：2026-10-25 20:13–20:17 (+08:00)

## 1. 变更清单

### 数据面

| 文件 | 变更 |
| :--- | :--- |
| `migrations/postgres/0007_issue_archive.sql` | `issues.archived_at`、`issues.archive_restored_at`；`ingest_metrics` 表；6 个索引；全部幂等 |
| `migrations/0007_issue_archive.sql` | SQLite 等价（两列文本、`ingest_metrics`、同名索引） |
| `migrations/postgres/0007_issue_archive.down.sql` | 回滚：DROP 表/索引/两列，全 `IF EXISTS`，可重复执行 |
| `crates/core/src/pg_repository.rs` / `sqlite_repository.rs` | 迁移接线、归档三态、`count_issues`、keyset 分页、事件计数、统计聚合、`ingest_metrics` UPSERT、活跃即解档、批量/过期归档 |
| `crates/core/src/repository.rs` | `IssueFilter.archived`；`IssueGovernanceRepository`（必需方法）并与 `IssueRepository` 组成超级 trait |
| `crates/core/src/models.rs` | `Issue` 增两列；治理 DTO/枚举 |
| `crates/core/src/db.rs` | SQLite 连接级 `busy_timeout=5s` |
| `crates/core/src/reindex.rs` | 新列在重索引路径的映射 |

### 服务面

| 文件 | 变更 |
| :--- | :--- |
| `crates/server/src/routes.rs` | 5 个新 handler（批量归档 / 单条归档 / 解档 / `/stats` / `/ingest/stats`）；列表与事件分页改造；ingest 旁路治理计数；统一 400/5xx 体 |
| `crates/server/src/main.rs` | `PONY_ISSUE_ARCHIVE_*` 解析（非法回落 + WARN）、TTL 归档作业（迁移后首轮 + 周期 + 退避） |
| `deploy/systemd/pony-sentry.env.example` | 归档治理四键模板与语义 |
| `deploy/scripts/deploy-console.sh` | trace 读自检按 `PONY_TRACE_INGEST_ENABLED` 取 410/200（修复停收配置下误判并回滚） |
| `crates/server/tests/api_test.rs` | 一条过时断言（负分页参数期望 200）与冻结 breaking B4 同步 |

### 测试

| 文件 | 内容 |
| :--- | :--- |
| `tests/acceptance_issue_governance.rs` | 12 用例（A1–A11 + a6b），红相 `EXIT 101` → 绿相 12/12 |
| `tests/acceptance_issue_governance_adversarial.rs` | 9 用例三族（畸形/并发 barrier/故障注入），红相 `EXIT 101` → 绿相 9/9 |
| `tests/Cargo.toml` | 两个 `[[test]]` target 注册 |

## 2. 迁移与回滚 runbook

### 2.1 升级（PG 生产）

```bash
# 0) 备份（沿用既有备份 timer 或手工）
pg_dump -Fc "$DATABASE_URL" | age -r <recipient> > /backup/ponysentry-$(date +%F).dump.age

# 1) 应用迁移（幂等，可重复执行；空库与现有库各跑一次均 Exit 0）
psql -v ON_ERROR_STOP=1 -f migrations/postgres/0007_issue_archive.sql

# 2) 部署二进制（脚本内含备份/自检/失败自动回滚）
bash deploy/scripts/deploy-console.sh

# 3) 验证
curl -fsS http://127.0.0.1:3000/healthz
#   issues 列表/统计/事件分页见 §3 收据
```

服务启动也会在 `migrate()` 内执行同一份 0007（列/索引探测，幂等），因此手工先行执行不是必需，
但推荐先手工执行以免启动阶段被 DDL 阻塞。

SQLite 部署：`migrations/0007_issue_archive.sql` 由 `SqliteIssueRepository::migrate()` 按列探测守卫执行；
升级前备份 `ponysentry.db`（SQLite 无 down）。

### 2.2 回滚（顺序不可交换）

```bash
# 1) 停服务        sudo systemctl stop pony-sentry
# 2) 回退二进制    sudo install -m 0750 -o root -g dm <上一版二进制> /usr/local/bin/pony-sentry-server
# 3) 执行 down     psql -v ON_ERROR_STOP=1 -f migrations/postgres/0007_issue_archive.down.sql
# 4) 启动          sudo systemctl start pony-sentry
# 5) 验证          curl -fsS http://127.0.0.1:3000/healthz && curl -H "x-admin-token: …" http://127.0.0.1:3000/api/v1/issues?limit=1
```

先 DROP COLUMN 再回退二进制会让仍在运行的旧二进制读 `ISSUE_COLUMNS` 时命中"列不存在"而全量 500。
down 脚本全 `IF EXISTS`，可重复执行（副本实测连跑两次 Exit 0）。
归档**不删任何数据**：只想撤销"归档"效果时，逐个 `POST /api/v1/issues/:id/unarchive` 即可，无需 down 迁移。

## 3. 验收收据（机器，2026-10-25）

原始输出：`.dev-team/receipts/curl-wave9.txt`（脚本 `.dev-team/receipts/run-wave9-curl-receipts.sh`，
凭据只经 shell 变量传递）。

| 验收项 | 实测（20:16 +08:00） |
| :--- | :--- |
| 列表不再被 ignored 噪声淹没 | `GET /api/v1/issues?project=pony-agent&status=unresolved` → HTTP 200、2 行、status 全为 `unresolved`、ignored 泄漏 0 |
| 总数语义 | `?project=pony-agent` → `X-Total-Count: 11`；`?limit=5` → `X-Total-Count: 112`、`X-Has-More: true`；`?limit=5&envelope=1` 返回信封 |
| stats 11 / 185 | `GET /api/v1/stats?project=pony-agent` → `issues.total=11`、`events.total=185`（`basis=sum_issues_count`）、`stored_rows=196` |
| stats 分组 | `by_status={ignored:7,resolved:2,unresolved:2}`、`by_level={error:11}`、`by_environment={dev:11}`、`series.len=31`、`sum(series.events)=196` |
| 事件分页 limit | `GET /api/v1/issues/<ignored-id>/events?limit=5` → **恰好 5 行**；`X-Total-Count: 46`；第 2 页游标无重叠 |
| 分页边界 | `limit=0` → 400；`limit=99999` → 200 且 `X-Limit: 100`；`cursor=!!!bad!!!` → 400 `invalid cursor` |
| 列表边界 | `status=bogus` → 400；`limit=-5&offset=-10` → 400 |
| 归档 ≠ 删除 | `dry_run=1` → `matched=9 / archived=0`、库归档行数仍 0；单条归档 → `archived_at` 非空且 `count=46`/`status=ignored`/事件 46 行不变；重复归档 `archived_at` 不变；`archived=only` 可查；`unarchive` → `archived_at=null`、`archive_restored_at` 留痕 |
| 治理指标 | `ingest/stats?window=24h` 初始全 0；两次冒烟后 `received=2, created=1, duplicate=1`；补一条 `tags.project` 冒烟后 `received=4/created=2/duplicate=2`，`by_project` 正确分组（`pony-sentry-governance-smoke` 与 `null`） |
| ingest 幂等（重启后冒烟） | 首次 `POST /api/v1/ingest` → **201 `created`**；同 `event_id` 重放 → **200 `duplicate`** 且 `count` 仍 1 |
| 405 语义 | `GET`/`PATCH /api/v1/issues/archive` → 405（静态段优先，`POST` 正常 200） |
| 冒烟产物清理 | 两条 smoke issue 置 `ignored` 并归档；此后默认列表 0 条含 smoke 项，`archived=only` 2 行；`pony-agent` 统计不变 |

其它门禁收据：`lead-gates.txt`（clippy `-D warnings` 0 / `cargo test --workspace` 0（22 套件）/ 功能 12/12 / 对抗 9/9）、
`migration-empty-db.txt`、`migration-existing-db.txt`（含 down 回滚与 re-up）、`prod-migration.txt`（生产两遍 Exit 0）、
`deploy-wave9.log`（部署自检 6/6 通过）、`sqlite-concurrency-fix.txt`（8×5 同 `event_id` 三轮：created=1/duplicate=39/0 个 5xx）。

## 4. 兼容性说明

- **fingerprint v3 未变**：指纹算法、输入分量（`platform‖error_type‖…‖culprit‖environment`）与
  `UNIQUE(fingerprint, environment)` 约束零改动；生产 `pony-agent` 的 11 条 issue 指纹前后一致。
  注意既有事实：指纹**不含 project**，project 由首报决定（跨项目同签名会并入同一 issue）。
- **`event_id` 幂等未变**：`events.event_key` + `uq_events_event_key` 部分唯一索引未动，重复投递仍走
  「不发新行、不加 count、不推 last_seen_at」；并发同键由唯一索引兜底归一为 duplicate。
  本波唯一的写路径变化是 SQLite 事务起点从 deferred 改为 `BEGIN IMMEDIATE` + 仅 BUSY 的整事务有界重试，
  语义结果不变（原本会 500 的并发路径现在排队后返回 200 duplicate）。
- **ingest 契约未变**：路径、请求体解析、响应字段（`result/status/issue_id/event_id/fingerprint/count/duplicate/warnings`）、
  状态码（201/200/400/401/413/410）、`/metrics` 输出、traces 侧契约全部零变更。
- **显式行为变更（已在契约 C7.3 登记 + 消费者普查）**：
  B1 列表 `status`/`level` 非法值从静默忽略改为 400；B2 列表默认排除归档项（数据未归档前结果集逐行不变）；
  B3 事件排序补 `id` 次级键（同 `created_at` 相对顺序由不确定变确定）；B4 `limit<0`/`offset<0` → 400；
  B5 `GET`/`PATCH /api/v1/issues/archive` 由 404 变 405；B7 新接口 400 体为 JSON 字符串编码。
  消费者普查：`web/src/App.vue` 三处调用点只消费数组/不解析错误体，且不传 `status/level/limit/offset`。
- **接口增量**：新增 `POST /api/v1/issues/archive`、`POST /api/v1/issues/:id/{archive,unarchive}`、
  `GET /api/v1/stats`、`GET /api/v1/ingest/stats`；issue JSON 增 `archived_at`/`archive_restored_at`（additive）。

## 5. 遗留风险

1. **治理计数写放大**：每个 ingest 最多 3 次 UPSERT 落在 `(bucket_date, project, outcome)` 单热行；
   当前量级无碍，高吞吐需批量聚合。
2. **归档作业单进程假设**：随进程启停、无分布式租约；多副本会重复扫描（归档幂等，只浪费扫描）。
3. **SQLite 游标依赖时间文本格式**：keyset 比较按 `events.created_at` 文本字典序，异构格式会错序。
4. **`/stats` 的 `totals` 是 additive 旁路**：按 range 跨度折算 `24h/7d/30d` 窗口（`ingest_metrics` 只按日桶存储），
   读失败回落零值并 WARN —— 不能与 `/ingest/stats` 的任意窗口逐值对齐。
5. **不提供全局 `GET /api/v1/events`**：本期非目标（见 README limitations）。
6. **归档与 ingest 的最终一致性**：归档瞬间到达的新事件会在同一 ingest 事务内触发「活跃即解档」；
   `ignored` 保持归档，因此 `ignored` issue 若持续上报会继续累积事件但不出现在默认列表——
   这是抑制噪声的有意代价，需要时用 `archived=only` 查看。
