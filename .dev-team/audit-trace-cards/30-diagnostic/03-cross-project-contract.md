# 跨项目契约衔接建议（job_copilot ⇄ pony-sentry）

- 定位：`01-diagnostic-sufficiency.md` 已量化「37.5% 覆盖率、3 个致命缺口」；本文件回答「**由谁改、改多少、契约上是否允许**」。
- 排序原则：**按「最小改动即可诊断本类事故」升序**——先纯生产者、复用现有词表/通道、零 schema 变更的项；再需裁决的项。
- 成本口径：`S` ≤ 0.5 人日；`M` 1–3 人日；`L` > 3 人日（含回归与验收）。
- 责任方：**生产者改**（job_copilot）/ **消费者改**（pony-sentry）/ **双方改**。
- 所有「消费者能否承载」的结论均有机器收据（RCP-06 / RCP-07），不是推测。

---

## 0. 消费者能力基线（决定成本的关键事实）

| # | 事实 | 证据 | 后果 |
| :-- | :--- | :--- | :--- |
| F1 | ingest 的 `turns` 是 `Option<Vec<serde_json::Value>>`，**整体不透明存放** | `crates/server/src/routes.rs:362`、`:1196`；RCP-07/Q2 | **turn / tool_call 内新增任意字段，消费者零改动即可持久化并原样返回** |
| F2 | 存库 payload 是「反序列化成 struct 再序列化」，未知**顶层**字段被静默丢弃 | 同 `routes.rs:1196`；RCP-07/Q2（`has_top_unknown=false`、`has_top_error_type=false`） | 顶层新增字段必须同步改 struct（`routes.rs:351-369`），否则静默丢失 |
| F3 | 同 `session_id` 二次上报按 `turn_id` 去重，**重复轮次以旧为准** | `crates/core/src/trace_merge.rs:69-77`、`crates/core/src/pg_repository.rs:614-632`；RCP-06/P5 | 「先骨架后富化**同一个** turn_id」不可行；换新 turn_id 才可行（RCP-06/P6 已验证新 turn 内容可带 `error`/`error_type`/`llm_calls` 入库） |
| F4 | 列表接口不支持 `run_id` 检索（`TraceQuery` 无该字段），DB 也无 `run_id` 索引 | `routes.rs:372-380`；`\d traces` 索引清单（RCP-01）；实测 `?run_id=__nope__` 仍返回全部 37 条（RCP-07） | 生产者在日志里拿到 run_id 后，**无法在控制台检索该 trace**，只能走 SQL |
| F5 | 控制台已渲染 `tool_calls.error` / `rejection_code` / `validator`，**未渲染** `error_type` / `llm_calls` / 时间戳 | `web/src/App.vue:838-880`（`call.error` 渲染块）、`:1344-1400` | 生产者补字段后需要一段**纯前端**展示改动（S），否则数据在库不可见 |
| F6 | 其余既有约束：2MB 上限、服务端二次脱敏、`traces.payload` JSONB 单列 | `routes.rs:419-423`、`:1200`；`\d traces` | 新字段需控体积；`error` 类字符串仍会被脱敏管线扫描 |

> F1+F2 是本次建议的核心杠杆：**诊断能力的 80% 可以靠「往 turns 里加东西」拿到，且不需要 DB 迁移、不改 wire 契约的顶层结构。**

---

## 1. 建议清单（13 条，按「最小改动即可诊断」排序）

| ID | 建议（最小改动） | 责任方 | 救回诊断题 | 改动锚点 | 成本 | 契约影响 |
| :-- | :--- | :--- | :--- | :--- | :--- | :--- |
| **P1** | `turns[0].error_type` + `tool_calls[].error_type`（+`cancel_trigger`）**外发受控枚举**（词表 `_SAFE_ERROR_TYPES` 已存在 22 类） | 生产者改 | D3 满分、D4 满分、D2 升部分 | `payload.py:233-310`、`exporter.py:205-244`、`tool_observability_middleware.py:111-116,259,278-279`、`trace_safety.py:287-320,554-555` | **S** | 扩展 R21 tool 字段 + turn 新键；**向后兼容**（新键，消费者按 F1 直接存）；不改 R1（R1 只禁 `error_message` 自由文本） |
| **P2** | `extra.request_id` + `extra.user_id` + `extra.failover_policy`（三者本地已有值，只是没传/没写） | 生产者改 | D8 满分 | `observability_sink.py:849-861`（enqueue kwargs 补 `request_id`/`user_id`）、`payload.py:324-337`（extra 白名单）、`observability_sink.py:200-202`（failover 入参） | **S** | `extra` 已是自由字典（`routes.rs:364`），**零消费者改动**；注意 `extra` 是顶层键之一，但内容不透明 → 不受 F2 丢弃影响 |
| **P4** | `turns[0].started_at_ms` / `completed_at_ms` + `tool_calls[].started_at_ms`（UTC ms，整数） | 生产者改 | D6 满分、D5 时序 | `payload.py:233-310`、`exporter.py:205-244`（中间件已有 `time.monotonic()` 起点，需换墙钟） | **S** | turn/tool 新键；F1 免改消费者；与 `reported_at_ms` 同一时钟域需写明 |
| **P11** | token 口径声明 + 失败调用计数：`turns[0].token_scope="billing_success_only"`、`calls_total`/`calls_failed` | 生产者改 | D7 提升、防成本误判 | `exporter.py:336-344`、`observability_sink.py:255-266` | **S** | turn 新键，向后兼容 |
| **P12** | `tool_calls[].output` 改为「ToolMessage.content 或 `Command.update` 受控抽取」+ `output_kind`；`arguments_summary` 从 `str()` 改 `json.dumps` | 生产者改 | D4 可信度（消除 S2/S14 误读） | `tool_observability_middleware.py:98,114` | **S** | 键名不变、类型不变（仍 str）；**无契约破坏** |
| **P3** | `turns[0].llm_calls[]`：每次 LLM 调用 `{model, provider, status, duration_ms, prompt_tokens, completion_tokens, cached_input_tokens, attempt_index, error_type, started_at_ms}` | 生产者改 | D5 满分、D7 满分 | `observability_sink.py:180-300`（3 处 sink 调用点收口）+ `exporter.py` 新增收集池（照 `record_run_trace_tool_call` 范式 `:205-260`） | **M**（1–2 人日） | turn 新键（F1 免消费者 schema）；**体积风险**：8 次调用 ≈ 1.5–2KB，在 2MB 内（`routes.rs:422`）；建议 `MAX_LLM_CALLS=50` 截断 |
| **P9** | 控制台展示：turn 头显示 `error_type`/时间戳；tool_call 内显示 `error_type`/`cancel_trigger`；新增「LLM 调用」折叠区 | 消费者改 | D2/D3/D5/D6/D7 可视化 | `web/src/App.vue:838-900`、`:1344-1400` | **S**（纯前端） | 无契约变更；**依赖 P1/P3/P4**；缺省容错已有范式（`:1344` 注释：任何字段缺失都不抛错） |
| **P6** | 取消/失败区分：`tool_calls[].status` 词表由三态扩四态（`success/rejected/error/cancelled`），或保留三态 + 外发 `cancel_trigger` | 生产者改 | D2/D4（②与④分界） | `exporter.py:231-235`、`payload.py:276-279`（两处口径需同步）、`tool_observability_middleware.py:296-330` | **S** | **需裁决**：消费者 `App.vue:1370-1372` 的 `TOOL_CALL_FAILURE_STATUSES=['error','failed']` 不含 `cancelled` → 显示为未着色；属展示兼容，不阻断 |
| **P5** | 业务态与传输态分离：`tool_calls[].business_status ∈ {ok, empty_result, retry_exhausted, failed}` | 生产者改 | D4 满分（本次「2 次业务失败记 success」） | 中间件 payload（`tool_observability_middleware.py:111`）+ `payload.py:270-310` | **S/M** | 扩展 tool 字段（F1 免 schema）；**需裁决是否动 status 语义**——本建议**不动** `status`，只加键，零破坏 |
| **P7** | 节点身份：`APPROVED_CURRENT_STEPS` 增图节点名（或 turn 增 `node` 字段并实现 `node_name` 词表外发） | 生产者改 | D2 满分 | `metrics.py:19-29`、`payload.py:237`、埋点侧 `current_step` 赋值点 | **M**（词表 + 埋点改造 + 回归） | 触及 R4/R6 的受控字段语义；**需 Lead 裁决**（扩词表会改变既有 `step=other` 分布，影响历史可比性） |
| **P8** | 列表按 `run_id` 检索（`TraceQuery` 加 `run_id` + `traces(run_id)` 索引） | 消费者改 | D8 效率（一键回链） | `routes.rs:372-380`、`:1246-1280`；DB 迁移（`\d traces` 无 run_id 索引） | **M**（含迁移审批/回滚） | **超出本任务 Non-Goal「不改 DB schema」** → 必须 Lead 裁决；替代降级：只做 SQL 手册（见 §4），成本 0 |
| **P10a** | 若需「失败骨架先报、富化后补」：生产者用**新 turn_id**（如 `turn_x:tool`/`turn_x:llm`）二次上报 | 生产者改 | —（容量/时效优化） | `payload.py:222-236` + exporter | **S** | 绕开 F3 的 old-wins；代价：`turns` 长度增加、控制台需合并展示（与 P9 同批） |
| **P10b** | 若需「同一 turn 可增量富化」：消费者实现**字段级 turn 合并**（现为整轮 old-wins） | 双方改 | —（架构优化） | `trace_merge.rs:69-77` + `pg_repository.rs:630` | **M** | **改已冻结的合并契约**（契约 §1：「重复轮次以旧为准」）→ 需 Lead 裁决；不推荐在本轮做 |

---

## 2. 契约提案（可直接落到 contracts 文档的字段清单）

> 全部落在 `turns[]` / `tool_calls[]` / `extra` 内，**不新增顶层键、不改 DB schema、不改合并语义**；
> 依据 F1（opaque turns）与 RCP-06/P6（`error_type`/`llm_calls` 实测可入库并原样返回）。

```jsonc
{
  "session_id": "thread_ref_v1_e9b4ec395c96d8913c106ef8",
  "extra": {
    "trace_id": "trace_f743...",
    "assistant_id": "job_copilot",
    "request_id": "request_454616d1607e465580d310a72e9f911f",   // P2 新增（受控：sanitize_trace_string("request_id") → opaque alias）
    "user_id": "510",                                             // P2 新增（整数转字符串；建议仅 dev/preprod 外发）
    "failover_policy": "disabled"                                 // P2 新增（受控枚举）
  },
  "turns": [{
    "turn_id": "turn_5dd7639f928549dfb86d96cbe6cbf33a",
    "status": "failed",
    "step": "other",                                              // P7 后应为真实节点名
    "error_type": "TimeoutError",                                 // P1 新增：_SAFE_ERROR_TYPES 受控词表
    "started_at_ms": 1792754879720,                               // P4 新增：UTC ms（node_started 19:27:19.72）
    "completed_at_ms": 1792754960002,                             // P4 新增
    "token_scope": "billing_success_only",                        // P11 新增
    "calls_total": 8, "calls_failed": 4,                          // P11 新增
    "llm_calls": [{                                               // P3 新增（上限 50 条）
      "model": "workbuddy/deepseek-v4.1-flash", "provider": "ponyllm",
      "status": "error", "duration_ms": 13240, "attempt_index": 1,
      "started_at_ms": 1792754878822,
      "prompt_tokens": null, "completion_tokens": null, "cached_input_tokens": null,
      "error_type": "ValueError"                                  // 需先补采集点，见 §3 风险
    }],
    "tool_calls": [{
      "tool_name": "format_jd_to_json", "status": "error",
      "business_status": "retry_exhausted",                       // P5 新增
      "error_type": "CancelledError", "cancel_trigger": "node_timeout", // P1/P6 新增
      "started_at_ms": 1792754880395, "duration_ms": 19554,        // P4 新增
      "attempt_round": 3                                          // P5/P6 新增（重试轮次）
    }]
  }]
}
```

字段级约束建议（写入契约文档，供 Test Agent 冻结）：
1. `error_type` ∈ `_SAFE_ERROR_TYPES`（`trace_safety.py:287-320`），否则 `"Exception"`；**禁止**放自由文本。
2. `*_at_ms` 为 UTC 毫秒整数，单调不减（`started_at_ms <= completed_at_ms`）。
3. `llm_calls` 长度 ≤ 50，单条 ≤ 512B；`tool_calls` 保持现有 1000/5000 字符截断。
4. `attempt_round` ∈ `[1, 10]`，与 `attempt_index`（运行级）**语义不同**，文档需并列声明。

---

## 3. 不建议做（避免"看起来更全"实则降质）

| 不建议 | 理由 |
| :--- | :--- |
| 外发 `error_message` 原文 / traceback | 违反 R1（`contracts:114-116`）与消费者二次脱敏（`routes.rs:1200`）；且 `sanitize_error_message` 恒返回 `[REDACTED]`，外发无信息量 |
| 把 `120s` 等常量写进 trace | 常量属实现细节，会随版本漂移；应由 `error_type=TimeoutError` + `timeout_ms`（若需要）表达 |
| 消费者侧尝试反解 `thread_ref_v1_*` | HMAC-SHA256 单向（`trace_safety.py:451-459`），无 key 不可逆；正确做法是把回链能力留在生产者 |
| 生产者侧绕过 `build_trace_payload` 直接 POST 自有 JSON | 会跳过 R1/R2/R4/R6/R9/R10 全部脱敏与类型钳制（`payload.py` 全量规则），等于自毁契约 |
| 为一个字段新增顶层键 | 触发 F2（struct 重序列化丢弃），须同步改消费者；同等效果放进 `turns`/`extra` 成本更低 |

---

## 4. 与 brief Non-Goals 的冲突点（须 Lead 裁决）

| 建议 | 冲突 | 若被否的降级方案 | 降级后成本 |
| :--- | :--- | :--- | :--- |
| **P8** 列表按 run_id 检索 + 索引 | brief §3「不改数据库 schema」 | 不改代码，只在运维手册给出可复制 SQL（本文件 §5 收据即模板），并在 `04-receipts.txt` 固化 `run_id` 查询命令 | 0 人日（文档） |
| **P10b** 字段级 turn 合并 | 改冻结合并契约（契约 §1） | 用 P10a「新 turn_id」完全规避，不动消费者 | S（生产者侧） |
| **P7** 扩 `APPROVED_CURRENT_STEPS` 词表 | 触及 R4/R6 受控字段语义，历史 `step` 分布改变 | 保留词表，仅新增 turn 内 `node` 字段（走 `node_name` 既有词表分支 `trace_safety.py:517-521`） | M→S |
| **P2** 外发 `user_id` | 隐私：消费者无用户维度聚合约定 | 仅在 `development`/`preprod` 外发（`trace_safety.py:462-465` 已有环境读策略） | 0（策略内） |

---

## 5. 责任方分布与总成本

| 责任方 | 条数 | 条目 | 成本合计（估算） |
| :--- | :--- | :--- | :--- |
| **生产者改**（job_copilot） | 10 | P1, P2, P4, P11, P12, P3, P6, P5, P7, P10a（P7 若降级为 `node` 字段亦在生产侧） | S×6 + M×2 + S×1 ≈ **3.5–5 人日** |
| **消费者改**（pony-sentry） | 2 | P9（前端展示）、P8（run_id 检索，待裁决） | S + M ≈ **1.5 人日**（P8 被否则 0.5） |
| **双方改** | 1 | P10b（字段级合并，待裁决，默认不做） | M ≈ 2 人日 |
| 合计 | **13** | — | **≈ 5–8.5 人日**；若只做「P1+P2+P4+P11+P12」（全 S、纯生产者、零裁决）≈ **2–2.5 人日把覆盖率 37.5% → ~75%**；再加 P3（llm_calls，M）可达 **~93.8%** |

> 排序复述：先做 **P1 → P2 → P4 → P11 → P12**（全部 S、全部生产者、零消费者改动、零裁决），
> 再做 **P3（llm_calls）** 与 **P9（展示）**，最后处理需裁决的 **P6/P5/P7/P8/P10b**。
> 该顺序同时满足「最小改动即可诊断本类事故」与 brief §3 的 Non-Goals。

---

## 6. 验收口径建议（给 verifier / Test Agent）

新增验收（黑盒、可机器判定，均在 job_copilot 侧 `api_v2/tests/acceptance/pony_sentry_trace/`）：
1. `payload.turns[0].error_type` 存在且 ∈ `_SAFE_ERROR_TYPES`（传入 `"ValueError"` 断言原样、传入自由文本断言收敛为 `Exception`）。
2. `payload.tool_calls[i].error_type` 在工具抛 `TimeoutError`/`CancelledError` 时非空（当前恒 null，属回归反证）。
3. `payload.extra.request_id` 非空且经 `sanitize_trace_string("request_id")`（opaque alias 形态）。
4. `payload.turns[0].llm_calls` 长度 == 本次 LLM 观测次数（错误场景下仍计数），单条 `status` 与本地 span 一致。
5. 体积回归：8 次 llm_calls 的 payload < 64KB。
6. 合并回归（消费者侧 e2e）：同 `session_id` 用**新** `turn_id` 二次上报后，`GET /api/v1/traces/:id` 同时可见两条 turn 及其 `error_type`（P10a 的收据化）。
