# Trace 契约（ingest / 读接口 / 合并 / 诊断 / 指标）

> 人读契约的家：本文件。实现锚点：`crates/server/src/routes.rs`、`crates/core/src/trace_v2.rs`、
> `crates/core/src/{pg,sqlite}_repository.rs`、`migrations/0005_trace_turns.sql`。
> 变更本契约的行为必须 same-commit 更新本文件（根 AGENTS.md 命约 4）。

## 1. ingest：`POST /api/v1/traces`

- 鉴权：`x-client-token` / `Authorization: Bearer`（未配置 `CLIENT_TOKEN` 时放行；先鉴权后解析体）。
- 请求体上限 2 MB（`DefaultBodyLimit`）；超限 `413 Payload too large`。
- 必需键：`session_id`（缺失时回退 `thread_id`，两者皆空/空白 → `400 Missing session_id or thread_id`）。
- 可选顶层键（其余键被忽略）：`project`、`run_id`、`turn_id`、`environment`、`release`、`eval_status`、
  `turns[]`、`tags`、`extra`、`total_input_tokens`、`total_output_tokens`、`total_duration_ms`、`reported_at_ms`。
- `turns[]` / `tool_calls[]` 是**不透明 JSON**：任意字段原样落库；服务端只读取
  `turn_id`、`status`、`input_tokens`、`output_tokens`、`duration_ms`、`tool_calls[].{tool_call_id,status,error,output,error_type}`
  用于合并、聚合与诊断。
- 客户端上报的 `eval_status` **被忽略**（一律落 `unreviewed`）；评审结论只能由 admin `PATCH` 写入。
- `reported_at_ms` 与服务端时间偏差超过 **24h** → `400 Bad request: reported_at out of range`。
- 未知顶层字段（含 `_sdk` 等 `_` 前缀键）保留到 `payload._unmapped`；仅服务端保留名
  `_redactions/_warnings/_unmapped` 由服务端注入覆盖，客户端同名键在任意层级都会被剥离（深度 ≤32）。
- `turns[]` 归一：非对象元素丢弃、`status` 非字符串→`unknown`、`tool_calls` 非数组忽略；
  归一告警写入 `payload._warnings[]`（`{"turn_index","reason":"invalid_turn"}`）。
- 成功响应：`201` + **库内持久化行**（`id` 可直接用于 `GET`；`total_*`/`turn_status`/`turns_count` 来自数据库，
  是 turns 的派生值，客户端上报的 `total_*` 不落库）。
- 失败响应：`400 Bad request` / `500 Internal error`（**不包含数据库错误原文**；原文只进服务端日志）。

## 2. 合并语义（同一 `(session_id, turn_id)`）

1. **终态单调**：终态集 `{completed,succeeded,failed,error,cancelled,interrupted,timed_out,max_tokens,aborted}`
   （大小写不敏感）。旧终态 + 新非终态 → 保留旧值；新终态 → 覆盖旧值（含迟到的 `error`/`tool_calls` 补全）。
2. **字段级深合并**：新值非 null / 非空字符串 / 非空数组或对象时覆盖；否则保留旧值。
3. **`tool_calls` 合并**：有 `tool_call_id` 按 id 合并（旧条目保序保留、同 id 覆盖字段、新 id 追加）；
   缺 `tool_call_id` 时按 `(tool_name, arguments)` 归一为 `sig:<tool_name>|<arguments>` 合并键，**不追加**，避免重报无界增长。
4. **`diagnostics` 为服务端生成字段**，合并时取双方合集，客户端同名字段会被覆盖。
5. 新 turn：`seq = max(seq)+1` 追加；缺 `turn_id` 时服务端生成 `turn_srv_<sha256(payload)[:16]>`
   （迁移回填使用同一 sha256 口径，重报可命中同一行）。
6. 终态→终态且新终态为 `completed`/`succeeded` 时，清空旧 `error`/`error_type`/`diagnostics`，避免「成功状态 + 残留错误」。

会话聚合（写回 `traces.payload`）：

- `turns` = 按 `seq` 排序的全部轮次；
- `total_input_tokens` / `total_output_tokens` / `total_duration_ms` = 按 turns 求和（单一真相）；
- `extra` / `tags` 深合并（缺失保留旧值）；
- `payload.eval_status` **不存在**（`traces.eval_status` 列是唯一真源）；
- `payload.session_id_source` ∈ `session_id|thread_id`。

## 3. 服务端诊断标注（`turns[].diagnostics`）

只在检出问题时出现，字段均为布尔或枚举：

| 字段 | 触发条件 |
| :--- | :--- |
| `error_contract: "missing"` | `turn.status ∈ {failed,error,cancelled,timed_out}` 且 turn 无 `error_type`/`error`，且所有 tool_calls 均无 `error` |
| `status_conflict: true` | 某 tool_call `status=="success"` 但其 `output` 命中失败特征（失败/异常/解析失败/unable/空结果） |
| `error_missing: true` | 某 tool_call `status=="error"` 但 `error` 为 null/缺失 |

服务端另注入：`payload._redactions={"secrets":n,"paths":n}`（脱敏命中计数）、
`payload._unmapped`（未知顶层字段）、`payload._warnings`（turns 归一告警）。
单个 `tool_call` 的 `output`/`arguments_summary` 超过 20000 字符时，**先脱敏后截断**并置服务端判定的
`truncated:true`（客户端预置值被忽略）；`turns[].llm_calls` 由服务端归一：≤50 条、11 键白名单、
`error_type` ≤64 字符且限 `[A-Za-z0-9_.:-]`，违规条目丢弃。

诊断在脱敏之后生成，不引入原始错误文本；指标 `pony_sentry_trace_status_conflicts_total` 随之递增。

## 4. 读接口

- `GET /api/v1/traces`：过滤 `session_id`、`run_id`（匹配聚合行或任一轮次）、`turn_status`、`eval_status`、
  `environment`、`release`、`project`、`from`/`to`（RFC3339，按 `reported_at`，含端点）；
  `limit` 默认 50、**clamp 到 [1,100]**；`offset<0` / 非数字 / 非法时间 / 非法 `order` → `400`；未知参数忽略。
  默认排序 `reported_at DESC`（`?order=created_at` 可切）。`?fields=summary` 返回不含 `payload` 与 `note` 的投影；
  响应头 `X-Total-Count` 为同过滤条件的总数（与分页无关）。
- `GET /api/v1/traces/:id`：返回单行；不存在 → `404 Trace not found`。
- `PATCH /api/v1/traces/:id`：`{eval_status, note?}`；`note` 落库并在后续读取回显；Cookie 会话需 `x-csrf-token`。
- `TraceRecord` 的 trace 维度字段：`id/session_id/run_id/turn_id/environment/release/project/eval_status/
  turn_status/turns_count/note/payload/total_*/reported_at/created_at/updated_at`。

## 5. 存储

- `traces`：每 `session_id` 一行（聚合视图，`UNIQUE(session_id)`）；索引 `run_id`、`turn_status`、`updated_at`。
- `trace_turns`：每 `(session_id, turn_id)` 一行（权威轮次存储），`seq` 单调；索引 `(session_id, seq)`、`status`。
- 写入在单事务内完成：PG 用 `pg_advisory_xact_lock(hashtextextended(session_id,0))`（64 位），SQLite 用 `BEGIN IMMEDIATE`；
  并发上报不会丢轮次。retention 与 ingest 共用同一锁键，锁序固定为「先 advisory、后行锁」。
- 回填：`0005` 只建 schema；存量轮次由 Rust 迁移步骤（`backfill_trace_turns`，启动时执行）按运行时同一
  `turn_keys_for`（sha256）口径回填（幂等；内容碰撞加 `_N` 后缀，保证两条缺 `turn_id` 的轮次不塌陷），
  hydration 在「轮次行数 < `payload.turns` 条数」时补齐。
- 保留期：`RETENTION_DAYS_TRACES`（默认 90）按 `COALESCE(updated_at, created_at)` 在同一事务内删除聚合行
  与其轮次行，不会误删仍存活会话的轮次。
- 回滚：PG 提供 `migrations/postgres/0005_trace_turns.down.sql` 与 `0006_trace_turn_run_id.down.sql`（幂等）；
  SQLite 不支持 `DROP COLUMN`，**无 down 迁移**，回滚需重建表——升级前必须备份 `ponysentry.db`。

## 6. 指标（`/metrics`，Prometheus 文本）

- `pony_sentry_traces_total{project,turn_status}`：每次成功 ingest +1（上报口径，重报累加）；label 归一：
  `project` 截断 64 字符且非 `[A-Za-z0-9_.:-]` 替换为 `_`，`turn_status` 只接受受控集合（其余 `other`），
  全进程 series 上限 512（超出归 `overflow` 并只告警一次）。
- `pony_sentry_trace_turns_total{status}`：本次上报中每个 turn 计数。
- `pony_sentry_trace_status_conflicts_total`：命中 `status_conflict` 的 ingest 次数。
- `pony_sentry_trace_ingest_rejected_total{reason}`：`session_missing` / `invalid_params`。
- `pony_sentry_trace_payload_bytes_total`：**上报体积**（客户端内容序列化字节数，非存储体积）。
- `pony_sentry_trace_merge_lost_turns_total`：合并被旧终态覆盖的轮次次数。
- `pony_sentry_trace_truncated_total`：被截断的 tool_call 文本条数。

可选失败通知：设置 `TRACE_WEBHOOK_URL` 后，终态 `{failed,error,cancelled,timed_out}` 触发一次异步 POST
（5s 超时、失败仅告警不重试）；去重键 `project|tool_name|turn_status`（各分量截断 64 字符），10 分钟内只发一次；
去重表 1024 条 / 256KB 双上限、满时淘汰最旧；并发 ≤8；日志只打印 URL host，不含 query/token。

## 7. 已知边界

- 顶层未知字段仍被静默忽略（`turns[]` 内不丢）；`turns[]` 无数量/体积上限；
- 列表接口不返回投影摘要（需要改进时走新路由，避免破坏 `TraceRecord[]`）；`/metrics` 不鉴权（探针面）；
- `0005` 的 `ALTER TABLE`/索引在 PG 上会短暂持写锁（非 `CONCURRENTLY`），大库需在维护窗口执行；
- 生产者侧错误文本默认不外发（job_copilot R1），诊断依赖 `error_type` 等受控字段。
