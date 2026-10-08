# Agent Note: add project dimension to issues and web filter

Status: implemented

## Problem

PonySentry 面向多个项目共用一套服务，但 issue 表只有 `platform` 一个来源维度，没有「这是哪个项目的缺陷」。
上报契约里其实已经带了工作区（`extra.project_path`，见 `.agents/skills/ponysentry-instrumentation/`），
但它此前只被 Webhook 分发器用来决定在哪个目录拉起 DSH 会话（`scripts/dsh_dispatcher.py`），
从未进入 issue 本体。结果是：一个接了多个项目的实例，控制台里所有缺陷混成一张表，
既看不出某个项目自身的健康度，也无法只盯某一个项目排查。

## Decision

把工作区提升为 issue 的一等维度，落在 `crates/` 与 `web/`（`crates/ingest/src/project.rs`、`crates/core/src/models.rs`、
`crates/core/src/{pg,sqlite}_repository.rs`、`crates/server/src/routes.rs`、`web/src/App.vue`）：

1. **项目名 = `extra.project_path` 的末段**（`project_name_from_extra`）。取末段而非整条绝对路径：
   脱敏管道把用户目录前缀替换成 `[USER_HOME]`（`SanitizationPipeline::sanitize_path`），
   前缀在入库时本就不可读也不可移植；而末段（项目目录名）在脱敏后完整保留，
   是跨机器、跨环境都稳定的唯一展示与筛选键。
2. **落为独立列** `issues.project`（迁移 `0002_add_project.sql`，Postgres 与 SQLite 各一份，带索引），
   而不是每次查询去 join events 的 JSONB。issue 是聚合实体，聚合属性就该有自己的列；
   否则筛选得扫 payload 文本，既慢又无法建索引。
3. **一个 issue 的 `project` 以首报为准**，后续同指纹上报不覆盖。issue 的指纹、标题、出错位置
   都诞生于首次上报它的那台机器的工作区；让标签随最后一次写入漂移，会让筛选结果变得不可解释。
4. **筛选与候选分离**：`GET /api/v1/issues?project=…` 负责过滤，
   `GET /api/v1/projects` 返回去重排序后的候选（排除 `NULL`），供 Web 端下拉使用。
   候选只在显式刷新时重取——筛选过程中重建下拉会把用户已选的项冲掉。
5. **Web 端**新增「项目」列与筛选下拉；未注入工作区的历史事件显示「未上报」而非空白。

## Alternatives considered

- **兼容 `extra.path` 与 `extra.project_path` 两个键**：上报端实际只用 `project_path`，
  `path` 是常见面包屑字段（`references/vue.md` 里就有 `from: to.path`），兼容它反而会把面包屑误认成工作区。
  落选：只认契约键 `project_path`。
- **展示完整工作区绝对路径**：入库时前缀已被脱敏成 `[USER_HOME]`，展示出来是残缺路径；
  且路径随机器而变，表格列会很宽、跨环境对不上。落选：只展示项目名，完整路径仍在事件 payload 里可查。
- **把 `project` 纳入指纹计算**（让跨项目同名错误各自成 issue）：这才是彻底的项目隔离，
  但指纹算法变更会让全部存量 issue 永不匹配、并按新维度重新裂变，
  属于破坏性的聚合语义变更，需单独立项与用户决策。本次不夹带。
  已知残留：两个不同项目若产生完全相同的 `(error_type, culprit, platform)` 指纹，仍会聚合成一条 issue，
  此时项目标签取首报方——如实记录，不掩饰。
- **从 events.payload JSONB 里现取项目名**：无需迁移，但筛选退化为 JSON 文本匹配，
  无法走 `idx_issues_project` 索引，且与 issue 聚合语义脱节。落选：独立列。
- **抽屉沿用卡片式底色边框**：已按要求改为纯排版（无底色、无边框，靠字号/字色/字重分级）。

## Consequences

- 存量 issue 的 `project` 为 `NULL`，需新事件上报才会回填；`0002` 迁移对存量库是纯增量、不回填历史。
- SQLite 的 `ALTER TABLE ADD COLUMN` 没有 `IF NOT EXISTS` 语法，重放会报 duplicate column，
  故 `SqliteIssueRepository::migrate()` 先查列再决定是否执行；Postgres 侧用原生 `ADD COLUMN IF NOT EXISTS`。
  两者都保证 migrate() 每次启动重放保持幂等。
- `IssueRepository::record_event_and_upsert_issue` 新增 `project` 参数，
  两个实现与测试调用点同步更新（跨包签名变更）。
