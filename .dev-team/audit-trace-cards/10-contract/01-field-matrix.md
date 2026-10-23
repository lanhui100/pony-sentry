# trace 字段契约矩阵（生产者承诺 / 消费者接受 / 实物实际）

> T1 · protocol-scribe · 2026-10-23。锚点：`00-baseline/00-brief.md`（Lead 冻结版 v1）。
> 本文件只做**事实对照**，不下缺陷判决（判决标准见 `02-audit-criteria.md`，由 T2/T3 执行）。
> 样本：`traces.id=ca0b5494-ae1b-47db-8e00-c9e7abbb6779`，`session_id=thread_ref_v1_e9b4ec395c96d8913c106ef8`，
> `run_id=run_83c1879e338d442f953a5dd4b8d1646a`，`turn_id=turn_5dd7639f928549dfb86d96cbe6cbf33a`。

---

## 0. 列定义、证据坐标与一处与任务书不一致的事实

### 0.1 三列口径

| 列 | 含义 | 权威来源 |
| :-- | :-- | :-- |
| **P（生产者承诺）** | job_copilot 实际写入 `POST /api/v1/traces` 的键与其取值规则 | 代码为真源（`PY`/`EX`），冻结条款为 `CT` 的 R 条款与 `CD` |
| **C（消费者接受）** | pony-sentry 反序列化 / 校验 / 脱敏 / 落库 / 对外返回的键 | `R`/`M`/`P`/`T`/`S`/`DB` |
| **A（实物实际）** | 本次事故 trace 的落库 payload 值 | `00-baseline/03-trace-payload.raw.json` + `00-baseline/01-db-truth.txt:5` + `00-baseline/02-api-get.json` |

路径缩写（全文统一）：

| 缩写 | 绝对路径 |
| :-- | :-- |
| `PY` | `/home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py` |
| `EX` | `/home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/exporter.py` |
| `CL` | `/home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/client.py` |
| `TS` | `/home/dm/job_copilot/api_v2/src/modules/agent/trace_safety.py` |
| `CT` | `/home/dm/job_copilot/api_v2/tests/acceptance/pony_sentry_trace/test_pony_sentry_trace_contract.py` |
| `CD` | `/home/dm/job_copilot/.dev-team/contracts-pony-sentry-trace.md` |
| `OS` | `/home/dm/job_copilot/api_v2/src/modules/agent/observability_store.py` |
| `R` | `/home/dm/pony-sentry/crates/server/src/routes.rs` |
| `M` | `/home/dm/pony-sentry/crates/core/src/models.rs` |
| `P` | `/home/dm/pony-sentry/crates/core/src/pg_repository.rs` |
| `SQ` | `/home/dm/pony-sentry/crates/core/src/sqlite_repository.rs` |
| `T` | `/home/dm/pony-sentry/crates/core/src/trace_merge.rs` |
| `S` | `/home/dm/pony-sentry/crates/ingest/src/sanitizer.rs` |
| `V` | `/home/dm/pony-sentry/crates/ingest/src/validate.rs` |
| `DB` | `/home/dm/pony-sentry/migrations/postgres/0003_add_traces.sql` |
| `UI` | `/home/dm/pony-sentry/web/src/App.vue` |

### 0.2 与任务书不一致的事实（先声明，避免 T2/T3 误引）

任务书把生产者依据写成 `OS`（observability_store.py）。**`OS` 不是 pony-sentry 的 trace 生产者**：

```bash
# 0 行命中 → observability_store.py 从未向 pony-sentry 发送任何请求
grep -nE "PonySentry|post_trace|pony_sentry" /home/dm/job_copilot/api_v2/src/modules/agent/observability_store.py
# 输出为空（exit 1）
```

`OS` 的自我定位是 job_copilot **自身**的 observability 存储（`OS:1`「api_v2 正式 observability trace/span/event 存储服务」），写自己的
`agent_observability_*` 表，模型含 `thread_id` / `span` / `partition` 等 pony-sentry 侧不存在的概念。

真正构造 wire payload 的是 `PY`（纯函数 `build_trace_payload`），搬运/采样/重试是 `EX`，HTTP 是 `CL`：

| 角色 | 代码位置 | 机器证据 |
| :-- | :-- | :-- |
| payload 构造 | `PY:165-387` | 单测/验收测试均直调该函数（`CT:232-234`） |
| 采集池 → payload | `EX:205-264`（tool_calls）、`EX:315-340`（consume）、`EX:964-984`（`_payload_kwargs`） | 唯一投递点 `EX:1069` |
| HTTP 投递 | `CL:35` 注释「成功码 201（创建）+ 200（幂等合并回显）」 | 探针实测 201（见 `03-probe-receipts.md` P1） |

因此本矩阵 **P 列以 `PY`/`EX` 为真源**，并以 `CT` 的 R 条款与 `CD` 作为冻结承诺交叉校验；`OS` 在缺失键行中以
「N/A + 理由」出现（它是另一套同名域模型，不影响本次采集）。

### 0.3 实物 payload 的两处结构性观察（贯穿全表）

1. **落库 payload ≠ 线格式切片，而是服务端结构体再序列化**：`R:1196` 用 `serde_json::to_value(&payload)`
   把 `AgentTraceIngestRequest` 结构体重新序列化，再 `R:1200` 脱敏后整体存 `payload`。后果：
   - 结构体里每个 `Option<_>` 都以 `null` 出现（无 `skip_serializing_if`，`R:353-368`），所以实物里有
     `eval_status: null`、`thread_id: null` 这两个**生产者从不发送**的键；
   - 结构体里没有的顶层键被**静默丢弃**（无 `deny_unknown_fields`）。探针 P1 实测：`schema_version` /
     `future_key` 发送后不落库（见 `03-probe-receipts.md` P1）。
2. **本样本脱敏为恒等变换**：服务端二次脱敏 `R:1200` 后 payload 与 Lead 抓取的 raw 逐键相等
   （`diff` 空、`turns` 深度相等），说明本样本没有被脱敏改写（`cache_hit_tokens` 因 `S:148-155` 的
   `*tokens` 白名单而未误伤）。该结论**不可外推**到含路径/密钥的样本。

---

## 1. 顶层键矩阵（实物 15 键 = 生产者 13 键 + 消费者注入 2 键）

| 字段路径 | P 生产者承诺 | C 消费者接受 | A 实物值与判定 |
| :-- | :-- | :-- | :-- |
| `session_id` | `PY:219` `public_trace_thread_id(thread_id, verify_reference_ownership=True)` 写入 `PY:332`；规则见 `CD:101`；断言 `CT:238-247` | `R:353` `Option<String>`；`R:1202-1211` 缺失时回退 `thread_id`；`R:1213-1218` 空白 400；落列 `DB:4`（`NOT NULL UNIQUE`）；`M:162` | `thread_ref_v1_e9b4ec395c96d8913c106ef8`。**OK**（隐私归一，符合简报 §8） |
| `project` | 常量 `"job_copilot"`（`PY:77`、`PY:333`），`CD:102` | `R:356` `Option<String>` → 列 `DB:9`（可空）；`M:169` `serde(default)` | `"job_copilot"`。**OK** |
| `turns` | `PY:229-312` 构造、`PY:334` 写入；元素契约见 §2 | `R:362` **`Option<Vec<serde_json::Value>>`：完全非结构化、零字段校验**，仅在 `R:1196` 重序列化、`R:1200` 递归脱敏（深度 32，`S:7`）；无 validate 调用（`V` 只服务 events，`R:800`） | 长度 1（`jq '.payload.turns\|length'` → `1`）。**OK（形状）**，元素内容判定见 §2 |
| `tags` | `PY:319-321` `{app, component, trace_id}`，值恒 `str`；`CT:283-296` 断言键集合精确三键 | `R:363` `Option<HashMap<String,String>>`：**非字符串值 → 整条 422**（探针 P7 实测）；原样入 payload | `{app:"api_v2", component:"agent_runtime", trace_id:"trace_f743ad606f93440eb836be7a5ee6c314"}`。**OK** |
| `extra` | `PY:324-329` `{trace_id, assistant_id}`，None 剔除；`CT:298-311` 断言不含 `error_message` | `R:364` `Option<serde_json::Value>` 任意 JSON，仅脱敏 | `{trace_id:"trace_f743...6c314", assistant_id:"job_copilot"}`。**OK**（trace_id 与 `tags.trace_id` 重复，见 §4-INV-1） |
| `total_duration_ms` | `PY:226` `_coerce_int()`；`PY:338` 写 wall-clock；**但 `PY:356-368` 多轮时改写为 `sum(turns.duration_ms)`** | `R:367` `Option<i64>`（字符串即 422，探针 P3）；列 `DB:14`；`P:636-657` **合并后用 payload 值反写列**；`T:20-49` 合并时重算 | `120272`；同值于列（`01-db-truth.txt:5`）与 `turns[0].duration_ms`。**口径歧义**（wall-clock vs 求和，见 §4-W2） |
| `total_input_tokens` | `PY:371-374` 仅在多轮或显式传入时写；规则 8 曾禁「臆造 0」(`CD:109`) | `R:365` `Option<i64>` → 列 `DB:12`；`P:640-645` 合并后以 payload 反写 | `36612`。**OK**（值来自 `EX` LLM 池聚合，`EX:333-340`） |
| `total_output_tokens` | `PY:376-379` | `R:366` → 列 `DB:13` | `1368`。**OK** |
| `reported_at_ms` | `PY:118-126` `_coerce_reported_at_ms`，非法回落当前毫秒；`PY:340`；`CT:328-343` | `R:368` `Option<u64>`；`R:1191-1194` 解析成 `reported_at`，非法/缺失用 `now()`；列 `DB:15` | `1792754960002` → `2026-10-23T11:29:20.002Z`（= 19:29:20.002 CST），`01-db-truth.txt:5` 一致。**OK（仅"上报时刻"语义，见 §4-W6）** |
| `run_id` | `PY:220` 经 `sanitize_trace_string("run_id")`；`PY:344-345` 条件写入 | `R:357` → 列 `DB:5`；**落列后被 upsert 覆盖**（`P:665-677` `run_id=EXCLUDED.run_id`）；payload 内保留 | `run_83c1879e338d442f953a5dd4b8d1646a`，列与 payload 一致（单次上报）。**合并后会失配**（§4-W3，探针 P5） |
| `turn_id` | `PY:222`、`PY:347-348`（**明令不用 run_id 兜底**，`CD` R10/G5） | `R:358` → 列 `DB:6`，同样被覆盖 | `turn_5dd7639f928549dfb86d96cbe6cbf33a`。**OK（单次）** |
| `environment` | 受控来源原样写（`PY:382-383`，`CD` R2 勘误），空串省略 | `R:359`；缺失默认 `"default"`（`R:1225`）；列 `DB:7 NOT NULL` | `"development"`。**OK**（简报 §8：Tilt dev 正常值） |
| `release` | 受控来源 `get_build_version()`（`PY:384-385`） | `R:360`；缺失默认 `"unknown"`（`R:1226`）；列 `DB:8 NOT NULL` | `"dev"`。**OK** |
| `eval_status` | **N/A — 生产者从不产出**：`PY` 全文无该键（`grep -n eval_status PY` 空），人工标注是 pony-sentry 侧概念 | `R:361` `Option<EvalStatus>`；非法枚举值 → 整条 422（探针 P2）；缺失默认 `Unreviewed`（`R:1228`）；列 `DB:10` | payload 内 `null`（`R:1196` 注入），列为 `unreviewed` → **同一事实两个家且不一致**（§4-W5） |
| `thread_id` | **N/A — 生产者从不外发**：原始业务 `thread_id` 属隐私，`PY:217-219` 只出 `thread_ref`（`CD` 规则 1） | `R:355` `Option<String>`，仅作 `session_id` 兜底（`R:1205`），**不单独落列** | payload 内 `null`（`R:1196` 注入）；走兜底路径时会造成 `session_id` 列与 `payload.session_id` 不一致（探针 P10，§4-W4） |

**顶层键覆盖计数：15 / 15。**

---

## 2. `turns[]` 键矩阵（实物 12 键；生产者可产出 15 键；其中 3 键本次未出现）

| 字段路径 | P 生产者承诺 | C 消费者接受 | A 实物值与判定 |
| :-- | :-- | :-- | :-- |
| `turns[].turn_id` | `PY:224`（空则 run_id 兜底）、`PY:234`；`CT:254-267` 断言存在 | 不校验；仅在合并键里用作去重（`T:62-67`） | `turn_5dd7639f928549dfb86d96cbe6cbf33a`。**OK** |
| `turns[].run_id` | `PY:235` | 不校验 | `run_83c1879e338d442f953a5dd4b8d1646a`。**OK** |
| `turns[].step` | `PY:237` `sanitize_trace_string("current_step")`；**非白名单 → `"other"`**（`TS:517-521`，白名单 `APPROVED_CURRENT_STEPS` = profiling/files_process/jd_analysis/resume_parser/interviewer/resume_diagnosis/modification，`metrics.py:19-29`） | 不校验；UI 仅在缺 `model` 时作为兜底显示（`UI:769`） | `"other"` → **真实节点名已不可恢复**（事故真因节点 `embedded_runtime` 在 trace 中无任何痕迹）。见 §4-W8 / M4 |
| `turns[].status` | `PY:239` 白名单收敛（`TS:538-541`，`APPROVED_RUNTIME_OUTCOMES` 含 `failed`/`failed_terminal` 等）；`CT:385-396` 断言无顶层 status | 不校验；**UI 不渲染 turn.status**（`UI:769` 只显示 model/step/duration） | `"failed"`。**OK（值保真）**，但消费者侧**不可见**（§5） |
| `turns[].duration_ms` | `PY:226/240` `_coerce_int` + i64 钳制（`CD` R9） | 不校验（`Value`）；合并时求和（`T:36-49`） | `120272`（=120.272s，与事故 120s 节点超时吻合）。**OK** |
| `turns[].degraded` | `PY:241` `bool(degraded)` | 不校验 | `false` |
| `turns[].attempt_index` | `PY:242`；`CD` 未定义其语义（run 级尝试 vs 工具重试）。样本 `1` 却存在同一工具 3 次调用 | 不校验 | `1`。**口径不明**（§4-W2 的姊妹项、M3） |
| `turns[].model` | `PY:253-254` 白名单校验，命中则原值（`TS:527-528`，`APPROVED_LLM_MODELS` 含 `workbuddy/deepseek-v4.1-flash`，`metrics.py:80`） | 不校验；UI 显示（`UI:769`） | `"workbuddy/deepseek-v4.1-flash"`。**OK** |
| `turns[].input_tokens` | `PY:255-256`（`EX:333-340` 聚合 prompt_tokens） | 不校验；UI 显示（`UI:771-774`） | `36612` |
| `turns[].output_tokens` | `PY:257-258` | 同上 | `1368` |
| `turns[].cache_hit_tokens` | `PY:259-260`；`CT:798-803` | 不校验；UI 显示 | `23936` |
| `turns[].tool_calls` | `PY:270-310` 数组，元素见 §3 | 不校验；合并/排序无；UI 折叠渲染（`UI:838-884`） | 3 元素（2 success / 1 error）。**OK（形状）**，内容见 §3 |
| `turns[].input_text` | **生产者承诺（R19）**：`PY:244-248`，strip 后非空才写，上限 `INPUT_TEXT_MAX_CHARS=10000`（`PY:82`）；`CT:754-769` 断言 `turns[0].input_text` 存在；`CD:225` | 不校验；UI 有专门文本块渲染（`UI:808-836`） | **缺失**。生产者侧 `record_run_trace_text`（`EX:294-313`）本 run 未收集到文本 → 诊断能力缺口 |
| `turns[].output_text` | 同上（`PY:249-252`，上限 20000，`PY:83`） | 同上 | **缺失** |
| `turns[].interrupt` | `PY:261-269`（`source/reason/query/user_choice`）；`CT:805-821` | 不校验；UI 有专门区块（`UI:777-806`） | **缺失**（本 run 无 HITL 中断，属条件键；不算缺陷，但消费者无法区分"无中断"与"未采集"） |

**`turns[]` 覆盖计数：12 实有 + 3 承诺未出现 = 15 / 15。**

---

## 3. `tool_calls[]` 键矩阵（实物 7 键 / 9 个生产者可产出键；`output` 在第 3 个调用上缺失）

| 字段路径 | P 生产者承诺 | C 消费者接受 | A 实物值与判定 |
| :-- | :-- | :-- | :-- |
| `tool_calls[].tool_name` | `PY:291-292` 白名单 `APPROVED_TOOL_NAMES`（含 `format_jd_to_json`，`metrics.py:105`），未命中 → `"other"`；池侧保留原文名（`EX:249-251` 合并白名单） | 不校验；UI `toolCallName()` 回退 `call.name`（`UI:1379`） | 3×`format_jd_to_json`。**OK** |
| `tool_calls[].status` | `PY:275-280` **仅三值**：`success`(success/ok) / `rejected`(rejected/gated) / **其余一律 `error`**；`CD:234` UI 期望 schema 同名字段 | 不校验 | `success, success, error`。**值域坍缩**（§4-W7）：`timeout` 等失败类型在 trace 内不可区分（机器复现见 `03-probe-receipts.md` R2：`status="timeout"` → `"error"`） |
| `tool_calls[].duration_ms` | `PY:294` `_coerce_int` | 不校验；UI 显示（`UI:854`） | `39706 / 39342 / 19554`（三者之和 98602 < turn 120272，差值 21670ms 为模型/编排耗时，无字段承载） |
| `tool_calls[].arguments_summary` | `PY:281-284`：`str(...)` **硬截断 1000 字符**后 `redact_text` | 不校验；UI 展开渲染（`UI:867-870`） | 517 / 513 / 334 字符（未触顶）。**入参如需完整复现将不可得**（截断语义无标记位） |
| `tool_calls[].error` | `PY:296` `str(tc["error"])[:500] if tc["error"] else None`；来源 `EX:205-260` 的 `error` 入参，上游 = 工具观测 payload 的 `error_message`（`tool_observability_middleware.py:115`） | 不校验；UI 仅 `v-if="call.error"` 显示（`UI:875-878`） | **3 条全 `null`**，其中第 3 条 `status="error"`。→ 失败调用零细节（§4-M1） |
| `tool_calls[].tool_call_id` | `PY:302-306` + `PY:152-162` 白名单 `^[A-Za-z0-9_.:-]{1,64}$`，不合法**省略键**（`CD` R29） | 不校验；UI 显示 `call_id`（`UI:879-881`） | `call_00_CyAJ7WXqZVicIHspuqeV0370` / `…aOhi15jI3OWYsASoJ3VK6299` / `…I6ga8z6pjyYo0rHfcn6r5154`。**OK** |
| `tool_calls[].output` | `PY:285-288/307-308`：`[:5000]`，**空值/假值直接不写键**（不是写 `null`） | 不校验；UI `v-if="toolCallOutput(call)"`（`UI:871-874`） | 调用 #1/#2 = 244 字符（同为"职位解析失败（原因: 模型返回空结果）。…"文案）；**调用 #3 无该键**。→ "无输出"与"未采集"不可区分（§4-W8） |
| `tool_calls[].rejection_code` | `PY:298-299`（中间件驳回分类）；契约级依据 **不存在**：`CD:233-237` 只定义 R21/R24，`R25` 在契约文档中无定义（`grep -n R25` 仅命中代码注释 `EX:219`）→ 该键属**代码级承诺** | 不校验；UI 徽章显示（`UI:849-851`） | **缺失**（本 run 无驳回，条件键） |
| `tool_calls[].validator` | `PY:300-301` | 不校验；UI 显示（`UI:863-866`） | **缺失**（条件键） |

**`tool_calls[]` 覆盖计数：7 实有 + 2 条件未出现 = 9 / 9。**

---

## 4. 跨字段 / 跨表不变式（单字段看不错，合起来会错的地方）

| ID | 不变式（应然） | 消费者的实际机制 | 实物 / 探针结果 |
| :-- | :-- | :-- | :-- |
| INV-1 | `trace = 一棵可关联的树`，`tags.trace_id` ≡ `extra.trace_id` ≡ `session_id` 可关联 | 无校验、无外键；三处独立写入（`PY:320-329`） | 两个 `trace_id` 相同（`trace_f743…6c314`），但 `session_id` 是 `thread_ref_v1_*`，二者无任何列承载映射 → 只能靠 payload 内字符串搜。**无索引**（`DB:23-27` 无 `tags/extra` 表达式索引） |
| INV-2 | 合并后 `run_id/turn_id` 列应表达"该行包含哪些 run/turn" | `P:665-677` upsert 直接 `= EXCLUDED.*` 覆盖；`turns` 只增不减（`T:69-83`） | 探针 P5：二次上报后列 `run_id=run_second`、`turn_id=NULL`，payload 内却有 `turns[0].turn_id=turn_p1`、`turns[1].turn_id=turn_p2` → **列与 payload 失配** |
| INV-3 | `payload.total_*` ≡ 列 `total_*` ≡ `sum(turns[*].*)` | `P:636-657` 合并时以 payload 值反写列；`T:20-49` 合并即重算 payload 值 | 目标行三者一致（120272/36612/1368）；探针 P5：旧值 `total_duration_ms=1` 被静默改写为 `500` → **历史值不可复现** |
| INV-4 | `session_id` 列 ≡ `payload.session_id` | `R:1202-1211` 允许 `thread_id` 兜底，但只改列不改 payload | 探针 P10：列 = `audit-probe-contract-*-p10`，`payload.session_id=null`、`payload.thread_id` 有值 |
| INV-5 | `eval_status` 列 ≡ `payload.eval_status` | `P:778` PATCH 只 `UPDATE traces SET eval_status`，不动 payload | 全库不一致：`payload.eval_status` 恒 `null`（结构体注入），列恒 `unreviewed` 等（`01-db-truth.txt:5`） |
| INV-6 | `status=error` 的 tool_call 至少有一个可诊断字段（`error` 或 `output`） | 无任何校验（`R:362`） | 第 3 条调用：`error=null` 且无 `output` → 零信息（M1） |
| INV-7 | `status=success` 的 tool_call 其 `output` 不应是失败文案 | 无任何校验 | 第 1、2 条：`status=success` + `output` 以「职位解析失败（原因: 模型返回空结果）」开头 → 语义自相矛盾（W1） |
| INV-8 | 未知/新增顶层键应被保留或显式拒绝 | `R:1196` 结构体重序列化 → 静默丢弃 | 探针 P1：`schema_version` / `future_key` 丢失且仍返回 201 |
| INV-9 | 同轮次补报应能补全（末次终态胜出或字段级 merge） | `T:69-83` **旧轮胜出**，新轮整条丢弃 | 探针 P6：先 POST `turn_t1` success，再 POST 同 `turn_id` 带 `error=ValueError…` → 库内仍是旧轮，**错误详情被吞** |
| INV-10 | 时间应能对齐外部证据（日志行 / DB 行 / 前端 sse 事件） | 仅 `reported_at`（上报时刻）与 `duration_ms`（相对量）；无 turn/tool 级时间戳 | payload 内无任何绝对时间键（`jq` 检出 `time\|ts\|at` 类键 = 0）；事故窗口 19:27–19:29 与 `/var/log/job-copilot-dev/api-v2-dev.log:187617,187692` 无法在 trace 内锚定 |

---

## 5. 消费者能力边界（检索 / 展示 / 保留 / 观测）

| 能力 | 代码事实 | 机器收据 |
| :-- | :-- | :-- |
| 检索维度 | `M:188-197` + `P:726-758` 只支持 `session_id/eval_status/environment/release/project/limit/offset`；**无 run_id、无 turn_id、无时间窗、无 turn.status 过滤、无全文** | `curl ".../traces?run_id=run_83c1…&limit=100"` → 返回 **43 行（= 全库行数）**，未知参数被静默忽略 |
| 索引 | `DB:23-27`：`session_id/project/eval_status/environment/created_at`；`run_id`/`turn_id` **无索引** | `\d traces` |
| 展示（抽屉） | `UI:698-708` 只显示 `session_id / eval_status / id / reported_at`；`UI:769` turn 行不显示 `status`；`UI:875` error 仅非空才显示 → 本样本第 3 条调用在 UI 上**看不到任何失败原因** | grep：`grep -n "turn\.status" UI` → 无命中 |
| 展示（空转字段） | `UI:702` 用 `payload.stats_incomplete` 显示"统计不完整"徽章，但**后端与生产者都不产出该键** | `grep -rn stats_incomplete crates web/src` → 仅 `UI:702` 一处 |
| 保留 | 90 天（`crates/server/src/main.rs:46` `RETENTION_DAYS_TRACES` 默认 90，分批 5000 行 `main.rs:23`） | **非缺陷**（简报 §8 同类），T2 不应判缺失 |
| trace 侧指标 | `/metrics` 只有 issues/events/ingest_requests/level_degraded（`R:746-778`），**零 trace 指标** | `curl -s localhost:3000/metrics \| grep -c trace` → `0` |
| 体量 | 请求硬限 2MB（`R:422`）；无 `payload_bytes` 列、无超限/丢弃计数 | 体积**必须注明表达式**：基线 `payload_bytes=3405`（`01-db-truth.txt:5`）与实测不一致——当前 `pg_column_size(payload)=2641`、`octet_length(payload::text)=5821`（TOAST 压缩态可变，差 2.2×）；`jq -c` 紧凑文本 5723 B |
| 体积口径 | 无列可查，只能现场算 | `$PSQL -c "select pg_column_size(payload), octet_length(payload::text) from traces where id='$TID';"` → `2641 \| 5821` |
| 错误面 | 类型/枚举失配一律 422 整条丢弃（P2/P3/P7）；空白 `session_id` 400（P9）；`turns` 任意形状/空数组/空 `tool_call` 均 201（P4/P11） | 见 `03-probe-receipts.md` |

---

## 6. 覆盖自检

| 面 | 数量 | 状态 |
| :-- | :-- | :-- |
| 顶层键 | 15（13 生产 + 2 消费者注入） | 全覆盖，逐格有 `path:line` 或 N/A 理由 |
| `turns[]` 键 | 15（12 实有 + `input_text`/`output_text`/`interrupt`） | 全覆盖 |
| `tool_calls[]` 键 | 9（7 实有 + `rejection_code`/`validator`） | 全覆盖 |
| 跨字段不变式 | 10 | 全部给出机器可查机制 + 实测/探针结果 |
| **字段格合计** | **39 字段行 + 10 不变式行** | — |

**未覆盖 / 无法机器验证项**

- `[NON-MACHINE]` `turns[].attempt_index` 的**应有语义**（run 重试 vs 工具重试）在 `CD` 与 `CT` 中均无定义，无法判定"1"是否错；
  需 Lead/生产者口径裁决（T3）。
- `[UNVERIFIED]` 第 3 条工具调用 `status=error` 的上游原始 outcome 字面值（`timeout` / `failed` / …）在 job_copilot 侧已被
  `tool_observability_middleware.py:111` 的 `outcome` 归一与 `EX:226-231` 二次归一覆盖，本次仅能从
  `/var/log/job-copilot-dev/api-v2-dev.log` 侧证（简报 §1 已冻结，本文件不复核）。
- `[UNVERIFIED]` 探针 P1/P4/P5 行在 11:48:43 创建后于 11:49:44 前被第三方删除（本卡未执行删除）；疑似并发成员以
  `like 'audit-probe-%'` 前缀做了过宽清理。成因未证实，但**收据以抓取时的 stdout 为准**，且本卡已复核自身 3 行删除
  （`select count(*)` → 0）。见 `03-probe-receipts.md` §4。

---

## 7. 复现命令（只读，可直接重放）

```bash
# 1) 实物 payload（与库/接口逐键相等）
jq -r '.turns[0] | to_entries | map("\(.key)=\(.value|type)") | .[]' \
  /home/dm/pony-sentry/.dev-team/audit-trace-cards/00-baseline/03-trace-payload.raw.json

# 2) 库真值 vs API 真值（payload 相等、列值一致）
jq -c '.payload' /home/dm/pony-sentry/.dev-team/audit-trace-cards/00-baseline/02-api-get.json | wc -c   # 5723（jq 紧凑形态；库内 jsonb 文本 5821 = 含空格）
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
psql -h 10.43.206.170 -U job_copilot -d ponysentry -c \
 "select run_id,turn_id,eval_status,payload->>'run_id' p_run,payload->>'turn_id' p_turn,
         payload->>'eval_status' p_eval, total_duration_ms
    from traces where id='ca0b5494-ae1b-47db-8e00-c9e7abbb6779';"

# 3) 消费者零校验 / 未知键丢弃 / 枚举硬 422（写探针，session_id 前缀 audit-probe-，用完即删，见 03-probe-receipts.md）
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:3000/api/v1/traces \
  -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
  -d '{"session_id":"audit-probe-contract-x","schema_version":"v1","turns":[{"tool_calls":[{}]}]}'   # → 201

# 4) 检索缺口
curl -s -H "x-client-token: $CLIENT" \
  "http://127.0.0.1:3000/api/v1/traces?run_id=run_83c1879e338d442f953a5dd4b8d1646a&limit=100" | jq length  # → 全库行数
```
