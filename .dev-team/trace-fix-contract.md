# trace 修复实施契约 v1（FROZEN）

> 依据：`docs/release/trace-audit-cards-2026-10-23/report.md`（v4）+ `dsh-benchmark-review.md` 的四项修正。
> 范围：pony-sentry 消费者侧 Wave 0 + Wave B + 选取 D（用户已批准 DB 迁移）。
> 本文件是 Test Agent 冻结红相的**唯一口径**；实现不得偏离，偏离须走契约 amend。

## C1 数据模型

### C1.1 新表 `trace_turns`（每 turn 一行，权威轮次存储）
| 列 | 类型(SQLite/PG) | 说明 |
| :--- | :--- | :--- |
| session_id | TEXT / TEXT | 会话键（与 traces.session_id 同值） |
| turn_id | TEXT / TEXT | 轮次键；缺失时服务端生成 `turn_auto_<sha256(payload)[:16]>` |
| seq | INTEGER | 首次出现序号（会话内单调，从 0 起） |
| status | TEXT | 由 turn.status 归一：缺失→`unknown` |
| payload | TEXT(JSON) / JSONB | 该 turn 的完整 JSON |
| created_at | TIMESTAMP | 首次写入 |
| updated_at | TIMESTAMP | 最近一次合并写入 |
| PRIMARY KEY(session_id, turn_id)；索引 (session_id, seq)、(status) |

### C1.2 `traces` 新列（聚合行保持向后兼容）
- `turn_status TEXT`：最后一个 turn 的 status（会话级终态摘要），非空默认 `unknown`；
- `turns_count INTEGER NOT NULL DEFAULT 0`；
- `note TEXT`：评审备注（PATCH 落库）；
- 索引：`(run_id)`、`(turn_status)`、`(updated_at)`。

### C1.3 迁移与回填
- `migrations/0005_trace_turns.sql` 与 `migrations/postgres/0005_trace_turns.sql`；
- 回填：从存量 `traces.payload.turns[]` 逐元素写入 `trace_turns`（同 turn_id 以**首个**为准），
  `turns_count`=回填行数，`turn_status`=最后一个 turn 的 status（缺失 `unknown`）；
- 迁移必须幂等（`IF NOT EXISTS` / `ON CONFLICT DO NOTHING`）；down 迁移（PG）删除新表/列/索引，SQLite 以重建表实现或明确记录不支持回滚。

## C2 合并语义 v2（替代「旧轮优先」）

对同一 `(session_id, turn_id)`：

1. **终态优先（单调推进）**：`terminal = {completed, succeeded, failed, error, cancelled, interrupted, timed_out, max_tokens, aborted}`。
   - 旧终态 + 新非终态 → **保留旧**（禁止回退）；
   - 新终态 → **新覆盖**（含 `error`/`tool_calls` 的迟到补全）；
   - 双非终态或双终态 → 字段级合并（下条）。
2. **字段级深合并**：新值非 null、非空字符串、非空数组/对象时覆盖旧值；否则保留旧值。
3. **`tool_calls` 按 `tool_call_id` 合并**：旧条目保留；新条目同 id 时，以「非空 `error`/`output`/`status` 变化」覆盖旧条目；新 id 追加。
4. **`diagnostics` 字段由服务端生成**（见 C4），合并时保留双方合集。
5. 新 turn（无同 id）→ `seq = max(seq)+1` 追加。

会话级聚合（写回 `traces.payload`）：
- `turns` = 按 `seq` 排序的全部 turn；
- `total_input_tokens/total_output_tokens/total_duration_ms` = **按 turns 求和**（单一真相），无任何 turn 含该字段时为 null；
- 顶层 `extra`/`tags` 深合并（非空覆盖，缺失保留旧 —— 修 W-09）；
- 其余顶层标量取本次上报值；`payload.eval_status` **移除**（列是唯一真源，修 W-10）；
- `payload.session_id_source`：`"session_id"` 或 `"thread_id"`（C5）。

## C3 写入事务与并发（修 W-02）

- PG：`BEGIN` → `SELECT pg_advisory_xact_lock(hashtext($session_id))` → 逐 turn 合并/upsert → 聚合写回 → `COMMIT`；
- SQLite：`BEGIN IMMEDIATE` 同一流程；
- 任何一步失败整体回滚；禁止无锁读-改-写；
- `trace_turns` 的 upsert 用 `INSERT ... ON CONFLICT(session_id, turn_id) DO UPDATE`（在锁内执行，等价安全）。

## C4 诊断标注（修 M-01，服务端生成，不改原始数据）

对每个 turn 生成 `diagnostics` 对象（仅在有问题时出现）：
- `error_contract:"missing"`：`status ∈ {failed,error,cancelled,timed_out}` 且 turn 无 `error_type`/`error`，且所有 tool_calls 均无 `error`；
- `status_conflict:true`：某 tool_call `status=="success"` 且 `output` 命中失败特征（`失败|异常|解析失败|unable|空结果`，大小写不敏感）；
- `error_missing:true`：某 tool_call `status=="error"` 且 `error` 为 null/缺失；
- 同时累加指标（C7）。

## C5 入参与响应契约（修 W-07/W-08/I-10/W-10）

- `POST /api/v1/traces` 返回 **库内持久化行**（`RETURNING id, created_at, totals,...`）：响应 `id` 可直接 `GET` 200；
  响应 `total_*` 与库内一致（聚合后求和值）。
- `session_id` 缺失回退 `thread_id` 时，`payload.session_id_source="thread_id"`；两者都缺 → 400（纯文本 `Missing session_id or thread_id`）。
- `GET /api/v1/traces` 过滤新增：`run_id`、`turn_status`、`from`/`to`（RFC3339，按 `reported_at`）；未知参数仍忽略（保持兼容）。
- `limit`：clamp 到 `[1,100]`；`offset < 0` → 400；非数字 → 400；**任何 4xx/5xx 响应体不得包含数据库错误原文**（统一 `Bad request` / `Internal error`）。
- `GET/List` 返回体新增：`turns_count`、`turn_status`、`note`（additive）。

## C6 保留期（修 I-11）

- 删除条件由 `created_at` 改为 `COALESCE(updated_at, created_at) < cutoff`（PG/SQLite 同义）。
- 既有 `purge_traces` 批量删除语义保留（每批 500）。

## C7 指标（修 I-06，仅新增，不改既有指标）

`/metrics` 新增（Prometheus 文本）：
- `pony_sentry_traces_total{project, turn_status}` 计数器；
- `pony_sentry_trace_turns_total{status}` 计数器；
- `pony_sentry_trace_status_conflicts_total` 计数器；
- `pony_sentry_trace_ingest_rejected_total{reason}`（invalid_params/session_missing）。

## C8 控制台（修 I-05，纯前端）

- 列表行：新增 `turn_status` 徽标（failed/error/cancelled → 红；interrupted → 黄；succeeded/completed → 绿；unknown → 灰）；
- 抽屉：Turn 头显示 `status` 徽标 + `attempt_index` + `degraded`；
- 新「失败原因」区：展示 `diagnostics.error_contract`（红字「未上报错误详情」）、`status_conflict`（黄）、`error_missing`（红）；
- `tool_call.error` 为空时显示「未上报」而非隐藏；
- 所有字段缺失必须降级渲染，不得抛错（沿用既有范式）。

## C9 验收（Test Agent 冻结，红相必须失败）

1. 合并 v2：running→failed 终态覆盖且 `error` 保留；failed→running 不回退；tool_calls 按 id 合并；新 turn 追加。
2. 并发：PG 测试库上同一 session 20 并发 × 2 turn，全部 turn 入库（`turns_count=40`），无丢失。
3. 响应一致性：POST 响应 `id` GET=200；响应 `total_*` == DB `total_*` == turns 求和。
4. 参数：`limit=-5`/`limit=abc`/`offset=-1` → 400 且响应不含 `Database error`；`limit=100000` → 200 且返回 ≤100 行。
5. 诊断：`failed` 且无 error → `diagnostics.error_contract=="missing"`；success+failure 文案 → `status_conflict==true`；error 无详情 → `error_missing==true`。
6. `extra` 二次上报保留（不置 null）；`payload.eval_status` 不存在；`session_id_source` 正确。
7. `note` PATCH 落库并回显；`turn_status`/`turns_count` 随写入更新。
8. retention：`updated_at` 新、`created_at` 旧的行不被删除；反之为删除。
9. 指标：`/metrics` 含 C7 四个指标名且计数随上报增长。
10. 迁移回填：旧形态行（只有 payload.turns）迁移后 `trace_turns` 行数 = turns 数、`turns_count` 一致。
11. 前端：列表/抽屉徽标与失败原因区按 C8 渲染（vitest）；浏览器 Console Error=0。
12. 回归：既有测试全绿（cargo test --workspace、cargo clippy -D warnings、web pnpm test）。

## C10 非目标

- 不改 job_copilot 生产者字段（Wave A/C 的生产侧）；
- 不引入 case 管理/webhook；不做列表 payload 投影（Wave C 剩余项）；
- 不改控制台读超时预算（`.dev-team/contract-console-timeout.md` R1–R8 持续有效）。

## amend 记录

- **v1.1（2026-10-23，Lead 裁决）**：
  1) 允许 Protocol Agent 为接线修改 `crates/core/src/lib.rs` 一行 `pub mod trace_v2;`（豁免 task-5 写域限制；否则空桩不参与编译，`cargo check` 与 stub-lint 同时恒真）。
  2) 空桩纯度门禁采用 `rust-stub-purity`（自建负样本可失败），`dev-stub-lint.py` 对 `.rs` 恒真，不作为有效门禁；两条都记录在 `gates-t1.txt`。
  3) C9.8（retention）若黑盒不可触发，允许标注 SKIP 并由核心单测覆盖，但必须在红相收据写明原因。
## amend v1.2（Lead）: C2 per-turn token 字段口径 = turns[].input_tokens / output_tokens / duration_ms（与现有 trace_merge 求和口径一致），不在本轮改名。
- **v1.3（2026-10-23，Lead 裁决）**：
  1) 批准 `pub fn merge_top_level(old: &Value, incoming: &Value) -> Value`；写路径调用序为
     `aggregate_session(&turns, &merge_top_level(&old_payload, &incoming_payload))`（extra/tags 深合并在此完成）。
  2) 编译衍生豁免：`crates/core/src/{sqlite,pg}_repository.rs` 行映射字面量与 `crates/core/tests/**`
     允许为新增字段做**纯占位**补齐（不改变任何语义/断言）；语义与迁移由 task-7 统一落地。
  3) `crates/core/tests/**` 中依赖旧合并语义的用例，由 task-7 按 C2 新语义更新并在收据中逐条说明。
- **v1.4（2026-10-23，Lead 裁决，依据 L3-A/L3-B 审查）**：
  1) 迁移回填改由 Rust 执行，统一用运行时 `turn_key_of`（sha256），SQL 迁移只建 schema；hydration 以「行数 < payload.turns 条数」判覆盖度。
  2) 服务端生成 turn_id 前缀由 `turn_auto_` 改为 `turn_srv_`（防客户端冒充）；客户端显式 `turn_auto_*` 不特殊处理。
  3) `tool_calls` 缺 `tool_call_id` 时按 `(tool_name, arguments)` 作为合并键，禁止无界追加。
  4) 终态→终态且新终态为 success 类（completed/succeeded）时，清空旧 `error`/`error_type`/`diagnostics`。
  5) ingest **忽略**客户端 `eval_status`（一律 `unreviewed`）；评审仅 admin PATCH。
  6) 指标 label 归一：`project` 截断 64 字符 + 非 `[A-Za-z0-9_.:-]` 替换为 `_`；`turn_status` 仅受控集合，其余 `other`；series 上限 512，超出记 `overflow` 并只告警一次。
  7) 所有 HTTP 错误响应（含 issue 路径、PATCH rejection）统一 `Bad request`/`Internal error`，不得回显 DB/serde 原文。
  8) 提供 `migrations/postgres/0005_trace_turns.down.sql`；SQLite 不支持 down，需在 docs 明记。

- **v1.5（2026-10-23，Lead 裁决，覆盖审核报告剩余条目）**：
  1) **M-04**：`SECRET_PATTERNS` 兼容单引号（`'password': 'x'`/`"password": "x"`/`password=x`）与裸 `token=`；保留 `*tokens` 白名单；补 5 种写法的单测。
  2) **W-05**：`reported_at_ms` 与服务器时间偏差 > 24h → `400 Bad request: reported_at out of range`；列表默认 `ORDER BY reported_at DESC`（`?order=created_at` 可切）。
  3) **I-01**：ingest 未知顶层字段保留到 `payload._unmapped`（对象；以 `_` 开头的键不特殊处理），不再静默丢弃。
  4) **I-02**：`turns[]` 归一：非对象元素丢弃并记 `payload._warnings[]={"turn_index","reason":"invalid_turn"}`；`status` 非字符串→`unknown`；`tool_calls` 非数组→忽略。不拒绝整条请求。
  5) **I-07**：脱敏管道返回计数，routes 注入 `payload._redactions={"secrets":n,"paths":n}`；路径仍改写为 `[USER_HOME]`（导出副本语义，无 canonical）；文档写明。
  6) **W-04**：`trace_turns` 增 `run_id` 列（迁移 0006）+ 索引；`?run_id=` 同时匹配 `traces.run_id` 或任一轮次 run_id。
  7) **I-04**：`?fields=summary` 时返回不含 `payload` 的记录；响应头 `X-Total-Count`（与过滤条件一致的计数）。
  8) **I-06**：新增 `pony_sentry_trace_payload_bytes_total`、`pony_sentry_trace_merge_lost_turns_total`、`pony_sentry_trace_truncated_total`；单个 tool_call 的 `output`/`arguments_summary` 超过 20000 字符时截断并置 `truncated:true`。
  9) **I-09**：`phase/sequence/node/attempt_index/degraded/stats_incomplete` 在契约中标记为可选生产者字段，缺失时 UI 回退；不再视为死契约。
  10) **M-03**：新增可选 `TRACE_WEBHOOK_URL`（5s 超时；去重键 `project+tool_name+last_turn_status`，10 分钟内只发一次；失败只告警不重试）；UI 增加「待评审失败」筛选预设（`turn_status=failed&eval_status=unreviewed`）；不做 issue 建单。
  11) **M-02/M-05/M-06（生产者，job_copilot）**：`turns[].llm_calls[]`（≤50，含 model/status/duration_ms/tokens/attempt_index/error_type）、`turns[].sampled/sample_rate`、`turns[].input_summary={"chars","sha256_16","head"}`（不传原文）；对应 R 条款与单测在 job_copilot 内落。
