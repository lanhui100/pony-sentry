# Agent Note: trace 合并语义 v2 与轮次行级存储

Status: implemented

## Decision

Trace 写入从「单行读-改-写 + 旧轮优先」改为「`trace_turns` 行级权威存储 + 事务内合并 + 聚合视图」：

- **存储**：新增 `trace_turns(session_id, turn_id, seq, status, payload, …)`，`traces` 继续作为会话聚合视图
  （`payload.turns` 按 `seq` 全量重建、`total_*` 按轮次求和、`turn_status/turns_count/note` 为派生列）。
- **合并**（`crates/core/src/trace_v2.rs`）：终态单调（非终态→终态可覆盖，终态不被非终态回退）、
  字段级深合并、`tool_calls` 按 `tool_call_id` 合并、`diagnostics` 取合集；无 `turn_id` 时服务端生成
  `turn_auto_<sha256(payload)[:16]>`。
- **并发**：整条写入在单事务内，PG 用 64 位 `pg_advisory_xact_lock(hashtextextended(session_id,0))`、SQLite 用
  `BEGIN IMMEDIATE`；`INSERT … ON CONFLICT(session_id,turn_id) DO UPDATE` 代替读-改-写。retention 与 ingest **共用同一锁键**，
  且固定「先 advisory、后行锁」的顺序避免死锁。
- **契约面**：ingest 返回库内持久化行（响应 `id` 可 `GET`）；`total_*` 只由轮次求和派生；
  `payload.eval_status` 移除；`turns[].diagnostics` 由服务端在脱敏后标注。契约原文见 `docs/trace-contract.md`。
- **保留期**：`COALESCE(updated_at, created_at) < cutoff`，单事务内按 id 升序持锁删除聚合行与其轮次。
- **迁移**：`0005` 只建 schema；存量回填在 `migrate()` 后用运行时 sha256 口径执行（幂等、可重放）；PG DDL 拆成逐条
  autocommit + 列存在性跳过 + `CREATE INDEX CONCURRENTLY`（二次启动 0 ALTER/0 CREATE），并提供 down 迁移。
- **可观测面**：`/metrics` 可选鉴权（`METRICS_TOKEN`，空则保持免鉴权探针面）；trace 指标 label 归一 + 512 series 上限。
- **v1.5 补全**（覆盖审核报告剩余条目）：脱敏支持单引号与裸 `token=` 并回传 `_redactions` 计数；
  未知顶层字段进 `_unmapped`、turns 归一回 `_warnings`；`reported_at` 偏差 >24h 拒绝、列表默认按 `reported_at`；
  `?fields=summary` + `X-Total-Count`；tool_call 文本 >20k 截断并计数；`trace_turns.run_id` + 索引；
  `TRACE_WEBHOOK_URL` 失败通知（5s、10min 去重）；生产者侧新增 `llm_calls[]`/`input_summary`/`sampled`。

依据：`docs/release/trace-audit-cards-2026-10-23/report.md`（W-01/W-02/W-03/W-09/W-10/M-01/I-11）
与 `dsh-benchmark-review.md`（append-only 事件账本、稳定错误码、去重键）。

## Alternatives considered

1. **只修合并函数、保留单行读-改-写**：改动最小，但并发丢轮无法根治（`SELECT … FOR UPDATE` 对首次插入无效，
   行不存在时无锁可取），且仍无法按轮次检索；否决。
2. **完全事件溯源（每 turn 一行事件、聚合即读模型）**：最贴近 DSH，但需要重写全部读接口与前端取数，
   超出本轮范围且破坏 `TraceRecord[]` 兼容；否决（`trace_turns` 已保留未来演进空间）。
3. **保留「旧轮优先」并只加终态字段覆盖**：无法处理迟到补全的 `tool_calls`/`error` 字段合并；否决。
4. **外发完整 `error_message` 以替代诊断标注**：违反 job_copilot R1 隐私红线，且被 `sanitize_error_message`
   恒 `[REDACTED]` 清零；否决，改为受控诊断标注（`error_contract/status_conflict/error_missing`）。
5. **`error_type` 采用闭集枚举、未知值收敛为 `other`**：DSH 事故复盘把「用最近通用类覆盖下层结构化失败」
   列为归因反模式；否决，改为稳定码 + 保留未知码（本仓当前只做标注，不引入新错误字段）。

## Consequences

- `traces.payload` 由「客户端上报原文」变为「服务端聚合视图」：不再含 `eval_status`、含 `session_id_source`
  与 `turns[].seq`；依赖旧 shape 的外部消费者需按 `docs/trace-contract.md` 调整。
- 写入路径多一次事务与轮次回查，单次 ingest 延迟上升（实测解析/写入仍在毫秒级，未见 p95 恶化）；
  `trace_turns` 增加存储约等于原 payload 中 turns 的体量。
- 迁移 `0005` 对存量 37 行回填 57 个轮次，可重放幂等；生产库执行前需按常规迁移审批（索引非 CONCURRENTLY，
  会短暂持写锁）。
- 审核报告中未实施的部分（列表投影、Wave A 生产者字段、告警/issue 联动）仍留在
  `.agents/notes/proposed/process/2026-10-23-pony-sentry-trace-diagnosability-audit.md`。
