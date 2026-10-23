# 生产者语义审核（auditor-diagnostic / task-3）

- 生产者 = job_copilot（`api_v2/src/modules/agent/pony_sentry_trace/*` + 上游 sink/中间件/存储）
- 消费者 = pony-sentry（`crates/server/src/routes.rs` 等）
- 目标：判定 trace 里每个字段**语义是否自洽、是否有损、是否与本仓事实相符**；每条给 `path:line` 与本次实物值。
- 本次实物取数入口：`RCP-01`（DB 行）/`RCP-02`（详情 JSON）/`RCP-08`（job_copilot 本地 span+event 真值）。下称 `实物`。
- 只读：未改 job_copilot 与 pony-sentry 任何代码。

## 0. 速览（14 条判定）

| # | 主题 | 判定 | 一句话 |
| :-- | :--- | :--- | :--- |
| S1 | `turns[].status` | **自洽** | 白名单收敛后 `failed` 原样保留，与真值一致 |
| S2 | `tool_calls[].status` | **不自洽（错）** | 传输态冒充业务态：业务失败 2 次被记为 `success` |
| S3 | `cancelled → error` 收敛 | **有损（错）** | 取消/失败/超时在 tool 维度不可区分 |
| S4 | `tool_calls[].error` 恒 `null` | **有损（漏）** | 采集点拿到了 `error_type` 却硬编码丢弃 |
| S5 | turn 级无 error 字段 | **有损（漏）** | R1 只禁自由文本，`error_type` 受控词表早已存在却未外发 |
| S6 | `attempt_index=1` | **语义错配（漏）** | 它是运行级尝试序号，不表达工具 3 轮重试 |
| S7 | `degraded=false` | **误导** | 只表示「已降级」，不表示「该降级却无降级可用」 |
| S8 | `step="other"` | **有损（漏）** | 节点词表 7 值不含图节点，节点身份结构性丢失 |
| S9 | token 口径 | **有损（错）** | 只累加**成功**调用；失败调用 token 全 NULL；无 per-call |
| S10 | `total_duration_ms`/`duration_ms` | **口径漂移风险（优化）** | 单 turn 镜像；多 turn 时消费者合并会重算覆盖 |
| S11 | `reported_at` vs `created_at` | **需标注（优化）** | 生产者时钟，差 -19min ~ +16h，不可当服务端时间 |
| S12 | `thread_id=null` / HMAC `session_id` | **有损但不改（约束）** | null 是消费者 struct 归一化产物；HMAC 单向 |
| S13 | 上报时机（单次终态 + old-wins 合并） | **结构性（优化）** | 补报同 turn_id 会被静默丢弃，增量不可行 |
| S14 | `output`/`arguments_summary` 内容 | **语义错（错）** | 是 Python `repr`（含 `Command(...)` 内部类型），非工具返回值/JSON |

---

## 1. S1 `turns[].status`：自洽

- 生产者：`payload.py:239` `"status": sanitize_trace_string("status", status) or "other"`；词表 `metrics.py:141-155`（含 `failed`/`timed_out`/`failed_terminal`…）；上游 outcome 归一 `metrics.py:600-602`。
- 实物：`turns[0].status="failed"`；本地真值 `v2_agent_observability_span(status='error')`、`v2_agent_run_identity.status='failed'`（RCP-08）。
- 判定：**自洽**，白名单收敛未误伤。
- 但**同一 run 的终态字符串在三处不一致**（跨项目对账时会撞车）：`v2_agent_run_identity.status="failed"`（verdict §C）、
  本地 `run` span `status="error"`（RCP-08，`observability_store.py:164-173` 的 `_normalize_status` 把 `failure` 映射为 `error`）、
  导出 `turns[0].status="failed"`（`payload.py:239` 原样保留 outcome）→ 三套词表。
- 且记录粒度有损：`timed_out` 在 `service.py:2173` 的通用 `except` 分支被硬编码写成 `"failed"`（另见 verdict §C），
  于是 trace 永远拿不到 `timed_out`——`status` 只表达「没成功」，不表达「因超时没成功」。建议在 S5 的 `error_type` 通道里补偿，不改 status 词表。

## 2. S2 `tool_calls[].status`：不自洽（错）——本次事故的核心语义谎言

- 生产者：`exporter.py:225-235`
  ```python
  raw_status = str(status or "").lower()
  normalized_status = ("success" if raw_status in {"success", "ok"}
                       else ("rejected" if raw_status in {"rejected", "gated"} else "error"))
  ```
  `status` 来源 = 工具中间件 `payload.get("outcome")`（`tool_observability_middleware.py:111`），
  而 `outcome` 只反映**是否抛异常**，不反映工具返回内容。
- 实物（RCP-02/A4 + RCP-08）：
  | # | trace.status | trace.error | trace.output 摘要 | 本地 span/真值 |
  | :-- | :--- | :--- | :--- | :--- |
  | 1 | `success` | null | `Command(update=...)`，内含「**职位解析失败（原因: 模型返回空结果）**」 | tool span `completed` 39706ms |
  | 2 | `success` | null | 同上（第二次空结果） | tool span `completed` 39342ms |
  | 3 | `error` | null | 空 | tool span `cancelled`（error_type=`CancelledError`）19554ms |
- 判定：**错**。第 1、2 次在业务上都是「失败 + 请求用户重试」，trace 却记 `success`；
  与 brief §4.1 举的反例（`status=success` 但 output 是失败文案）**完全命中**。
- 影响：任何基于 `tool_calls.status` 的成功率/告警都是假绿。本事故「3 轮重试」在 trace 里成了「2 成功 + 1 错误」。
- 建议（见 `03-cross-project-contract.md` P3）：加 `business_status`（受控枚举 `ok/failed/retry_exhausted/empty_result`），
  保留 `status` 作传输态；或最省事：把 `output` 里的受控失败标记 `empty_result` 一并上报为 `result_kind`。

## 3. S3 `cancelled → error`：有损（错）

- 生产者：`exporter.py:234` 的 `else "error"` 把 `cancelled` / `timeout` / `failure` / 任意未知值**全部**塌成 `error`；
  `payload.py:276-279` 二次白名单又把同样的三态逻辑重复一遍（两处口径必须同步，否则漂移）。
- 实物：第 3 条 trace `status=error`，而本地是 `outcome=cancelled` + `error_type=CancelledError`（RCP-08 事件 seq 13）。
- 判定：**有损**。取消（客户端断连/超时回收）与真实失败混为一谈，正是本次事故 ④（120s 兜底杀节点）与 ②（真实 LLM 错）
  在工具维度上的**分界线被抹掉**。
- 影响：无法回答「这个 error 是模型错还是被超时取消」。建议直接用 `sanitize_trace_string("status", ...)` 的既有词表
  （`APPROVED_RUNTIME_OUTCOMES` 含 `cancelled`），把中间层三态扩为四态，或单独输出 `cancel_trigger`（该字段**已有受控词表**，
  见 `trace_safety.py:560-563`，且中间件已在采集，`tool_observability_middleware.py:296-299`）。

## 4. S4 `tool_calls[].error` 恒 `null`：有损（漏）——最便宜的修点

- 链路（三段都有据）：
  1. 中间件异常分支：`tool_observability_middleware.py:278-279`
     `"error_type": _safe_exception_type(exc),` + `"error_message": None,` ← **硬编码 None**
  2. 池写入：`:115` `error=payload.get("error_message")` ← 只取被硬编码为 None 的字段
  3. 序列化：`exporter.py:244` `"error": str(error) if error else None`；`payload.py:296` `"error": str(tc.get("error") or "")[:500] if tc.get("error") else None`
  - 取消分支（`:300-330`）连 `error_message` 键都不传。
- 旁证：`payload.py:284/288` 对 `arguments_summary`/`output` 调 `redact_text`，而 `:296` 的 `error` **不脱敏只截断** →
  说明设计上 error 被视为受控字段，但当前因恒 None 尚未暴露风险（修复时应补 `redact_text`）。
- 实物：274 条 tool_call 中 `error` 非空仅 **15** 条（5.5%），本次 3 条全 null（RCP-04）。
- 判定：**漏**。中间件**已经**拿到了 `error_type`（`_safe_exception_type` 覆盖 TimeoutError/ValueError/CancelledError…），
  只是没往 trace 池传。建议：`record_run_trace_tool_call` 增 `error_type` / `cancel_trigger` 形参（`exporter.py:205-220` 已在白名单
  `_TOOL_CALL_MERGE_FIELDS:141-152` 里预留了 `rejection_code`/`validator` 两个同类字段，扩展零风险）。

## 5. S5 turn 级 error 字段缺失：有损（漏），但**不是** R1 的本意

- 生产者：契约 `§3.2.1 R1`（`contracts-pony-sentry-trace.md:114-116`）只规定「`error_message` 接收但不外发，`extra` 不得含该键，
  也不得派生 `error_present`」；`payload.py:188` 保留 kwarg，`observability_sink.py:849-861` 确实把 `error_message` 传进 enqueue，
  `payload.py` 直接丢弃。
- 但 `error_type` **不在 R1 禁令内**，且受控词表**早已存在**：
  `trace_safety.py:287-320`（`_SAFE_ERROR_TYPES` 含 `ValueError`/`TimeoutError`/`CancelledError`/`RateLimitError`…）、
  `trace_safety.py:554-555`（未知值收敛为 `Exception`），当前已被本地存储与 Admin API 使用
  （`observability_store.py:1187`、`admin/thread_explorer.py:466`）。
- 实物：`turns[0]` 键集合 12 个，无任何 error 键（RCP-02/A3）。
- 判定：**漏**，而且是「设计已具备、接口未接通」型漏。这也是 §4/§5 里最便宜且 ROI 最高的一项（`03-...` P1）。
- 附带事实：真正跑出 `ValueError: expected value at line 1 column 1` 的地方（`tool_utils.py:401/476`）本地也没有写进任何
  span：本次 4 条 LLM error span 的 `error_type`/`error_message` **均为 NULL**（RCP-10）——即「就算全量外发本地 trace，
  也拿不到错误签名」。所以建议分两步：先接通 `error_type`（受控），再在 LLM 观测点补 `error_type` 采集。

## 6. S6 `attempt_index=1`：语义错配（漏）

- 生产者：`observability_store.py:686` `attempt_index=attempt_index if attempt_index is not None else 1`；`payload.py:225` `_coerce_int`。
  该值来自 runtime context（工具侧用法见 `resume_parser.py:817-830` 的多模型尝试），语义是**运行/节点级尝试序号**。
- 实物：`turns[0].attempt_index=1`，而同一 run 内工具实际经历 **3 轮**重试（本地 tool span 3 条、llm span 8 条，RCP-08）。
- 判定：**语义错配**。消费方极易把 `attempt_index=1` 读成「第一次尝试就失败，没有重试」。
- 建议：明确重命名为 `run_attempt_index`（契约层面），新增 `tool_calls[].attempt_round`（1..N）。改名有兼容成本 →
  若不想动则至少在文档声明语义（S 级，纯文档）。

## 7. S7 `degraded=false`：误导

- 生产者：`service.py:79/291`（`mark_degraded`）、`:2075-2077`（fallback 时打标）、`observability_sink.py:876-882`（转 `degraded`）。
  含义 = 「**已经**走了降级路径（备用模型/跳过非关键工具/熔断）」。
- 实物：`degraded=false`；而 verdict §F 指出本次 `ENABLE_NEW_FAILOVER=False`，**本该降级但没有降级可用** → 标记只能是 false。
- 判定：**误导**（不是错，是语义窄化）：「健康」与「想降级却无降级」在字段上等价。
- 建议：不改 `degraded`；在 `extra` 增 `failover_policy`/`failover_available`（中间件已在采，见 `observability_sink.py:200-202` 的
  `failover_policy/failover_phase/failover_mode`），成本 S。

## 8. S8 `step="other"`：节点身份结构性丢失（漏）

- 生产者：`payload.py:237` `"step": sanitize_trace_string("current_step", current_step)`；
  `trace_safety.py:517-521` 对 `current_step` 只放行 `APPROVED_CURRENT_STEPS ∪ {streaming, unknown, other}`；
  `metrics.py:19-29` 的 7 值是 `profiling/files_process/jd_analysis/resume_parser/interviewer/resume_diagnosis/modification`，
  **不含任何图节点名**（如 `jd_parse`/`format_jd_to_json` 所在节点）。
- 实物：`step="other"`；本地 event seq1 `current_step="profiling"`，seq14（终态）`current_step="other"`（RCP-08）
  → 终态口径本身就是 `other`，节点级身份在这个 run 里从头到尾没被记录过。
- 判定：**漏**。注意：这不是「导出时被收敛」，而是**生产侧根本没采到节点名**；只改 payload 无用，必须扩词表（M 级）。
- 影响：D2「失败发生在哪一层」不可答的主因之一。

## 9. S9 token 口径：有损（错）

- 生产者：`observability_sink.py:255-266` 在 **每次** LLM sink 都无条件累加 `record_run_trace_llm_tokens`；
  `exporter.py:188-207` 逐次 `+=`；`exporter.py:336-344` 汇总成 `input_tokens/total_input_tokens`。
- 实物（RCP-09 对账）：
  - trace：`input_tokens=36612`、`output_tokens=1368`、`cache_hit_tokens=23936`
  - 本地 8 条 llm span：`sum(prompt_tokens)=36612`、`sum(completion_tokens)=1368`、`sum(cached_input_tokens)=23936` **完全吻合**
  - **但 8 条里只有 3 条有 token**；4 条 `status=error` 与 1 条 `cancelled` 的 `prompt_tokens/completion_tokens` **全为 NULL**
- 判定：**错（口径）**。`total_input_tokens` 实际是「**成功调用**的计费口径」，不是「本次 run 的真实消耗」。
  按每次约 12k prompt 估算，失败 5 次至少另有 ~60k 输入未被计量（`[EST]`，不做承诺值）。
- 另：`cache_hit_tokens=23936` 是 `input_tokens` 的**子集**（11776 < 12205、12160 < 12572，RCP-09），
  但契约 R22（`contracts:241-243`）只说「严格映射自 cached_input_tokens」，**未声明子集关系** →
  `input+cache` 相加会重复计费，消费方无法自助算成本。
- 建议：`turns[0]` 内声明 `token_scope: "billing_success_only"`；失败调用补 `prompt_tokens` 或至少补 `calls_total/calls_failed`。

## 10. S10 `total_duration_ms` 与 `turns[].duration_ms`：口径漂移风险（优化）

- 单 turn：`payload.py:334` 规则 6 直接镜像 `duration_ms`；实物两者都是 120272（自洽，利于一眼看出 120s）。
- 多 turn：`payload.py:350-368` 会在 `len(turns)>1` 时**改写** `total_*` 为逐 turn 求和；消费者合并侧
  `trace_merge.rs:27-52` 又会对合并后的 turns **再算一遍 sum**。两侧各有一套求和逻辑 → 跨项目口径漂移的隐患（本次未触发）。
- 判定：**优化项**（当前实物无错）。建议单一真源：要么生产者不再写 `total_*`，要么消费者合并时不再重算。

## 11. S11 `reported_at` / `created_at`：需标注（优化）

- 生产者：`reported_at_ms` 由生产者填（`payload.py:337`；缺省 `int(time.time()*1000)`）；
  消费者：`routes.rs:1190-1194` 用它换算 `reported_at`，`created_at = now`。
- 实物：`reported_at=2026-10-23T11:29:20.002Z`、`created_at=11:29:20.029042Z`（+27ms，正常）；
  但全库 `reported_at-created_at` 分布为 **-00:19:28 ~ +16:30:05**（RCP-05）→ 生产者时钟/补报延迟差异巨大。
- 判定：**需标注**。`reported_at` 只能当「生产者视角的发生时间」，不能当服务端接收时间；用它做 SLA/积压判断会错。

## 12. S12 `thread_id=null` 与 HMAC `session_id`：有损但属既有隐私约束

- 生产者：`trace_safety.py:440-448`（已是 `thread_ref_*` 形态则原样返回，`verify_reference_ownership=True` 时重算）、
  `:451-459`（HMAC-SHA256(key, `"trace-thread-ref:"+thread_id`)[:24]）、`:467-487`（历史 key 候选）。
- **重要澄清（与 brief §8 不冲突，但需纠正一个可能的误读）**：trace payload 里出现的
  `"thread_id": null`、`"eval_status": null` **不是** job_copilot 写入的，而是 pony-sentry 消费者在
  `routes.rs:1196` 用 `serde_json::to_value(&payload)`（反序列化后的 `AgentTraceIngestRequest`）**重新序列化**产生的：
  该 struct 声明了这些字段（`routes.rs:351-369`），缺失即为 `null`。实测：任意 job_copilot trace 的 payload 键集合恒为这 15 个
  （RCP-07/Q2），且上报方无法自造 `thread_id`。
- 判定：**有损但属约束**（brief §8 已认定为隐私设计）。可追溯性代价 = 消费者无法反查业务 thread（HMAC 单向，无 key）；
  缓解：把回链能力放在生产者（它持 key，可自行算 `public_trace_thread_id(thread_id)` 去查 pony-sentry）。成本 S，见 `03-...` P4。

## 13. S13 上报时机：单次终态 + old-wins 合并 → 增量补报不可行（结构性）

- 生产者：唯一 hook `observability_sink.py:849-861`（run 终态，`should_retain_trace` 之前），**一次事件、一次投递**；
  进程内收集池 `exporter.py:300-350` 在投递时 `consume` 并清空。
- 消费者：同 `session_id` 二次上报按 `turn_id` 去重，**重复轮次以旧为准**（`trace_merge.rs:69-77`、`pg_repository.rs:614-632`）。
- 实物（RCP-06）：
  - P1 上报 `turn-audit-1{status,step,duration}` → P2 同 `turn_id` 追加 `error_type`/`tool_calls` → **补写被丢弃**（`P5=true`）；
  - P3 换新 `turn_id` 上报 → `error` 文本、`error_type`、`llm_calls` **原样入库**（`P6=true`）。
- 判定：**结构性（优化）**。结论有两条：①「先失败骨架、后富化同轮」的方案在当前合并语义下**无效**；
  ② 绕过它只需**换新 turn_id**（生产者改，S），无需改消费者。
- 另：`enqueue_run_trace_export` 调用点**没有传 `request_id` / `user_id`**（`observability_sink.py:849-861` 的入参里有两者，
  但 kwargs 未包含）——这是 D8 不可答的直接原因，且是纯生产者一行级改动。

## 14. S14 `output` / `arguments_summary`：语义错（错）——R24 意图落空

- 生产者：`tool_observability_middleware.py:98` `args_summary = str(input_payload)[:1000]`、
  `:114` `output=str(output_payload)[:5000]` → 对任意对象做 `str()`，得到 **Python repr**。
- 实物：`output = "Command(update={'messages': [{'role': 'tool', ...}]})"`（RCP-02/A4）——这是 LangGraph `Command`
  的 repr，不是工具返回值，也不是 JSON；`arguments_summary` 同理是 `{'jd_text': '...'}` 的单引号 repr（非 JSON，无法直接解析）。
- 判定：**错**。契约 R24（`contracts:247-249`）要求「工具返回值打通、使 tool_calls 能展示工具执行产生的具体消息」，
  现状是内部对象泄漏：既暴露实现类型名，又丢结构化（消费方只能当纯文本看）。
- 建议：`output` 改为「取 ToolMessage.content 或 `Command.update` 中受控抽取」+ 显式标注 `output_kind`；
  `arguments_summary` 若是 dict 则 `json.dumps(..., ensure_ascii=False)`（S 级、无契约破坏）。

---

## 15. 汇总：生产者侧「一句话可修」清单（细节与成本见 `03-cross-project-contract.md`）

| 判据 | 结论 |
| :--- | :--- |
| 语义自洽 | 仅 S1（status）、S10 单 turn 场景 |
| 语义不自洽/错 | S2（业务失败记 success）、S3（cancelled→error）、S9（token 只计成功）、S14（repr 冒充返回值） |
| 有损但**字段词表已现成**、只差接线 | S4（error_type 已在手丢弃）、S5（`_SAFE_ERROR_TYPES` 已存在）、S12（回链可在生产者侧做） |
| 结构性/需设计 | S6（attempt 语义）、S8（节点词表）、S13（合并语义）、S7（degraded 语义窄化）、S11（时间口径标注） |
