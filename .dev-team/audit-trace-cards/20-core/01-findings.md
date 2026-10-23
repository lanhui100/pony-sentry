# T2 findings — pony-sentry 自身 trace 管线审核（auditor-core）

> 审核对象：ingest → 校验/脱敏 → 合并 → 存储 → 读接口 → 控制台（消费者侧全部代码）。
> 锚点：`00-baseline/00-brief.md`；样本：事故 trace `ca0b5494-ae1b-47db-8e00-c9e7abbb6779`。
> 机器收据：[02-receipts.txt](02-receipts.txt)（R1–R15，可逐条重放）。禁止改业务代码，只读 + 实验行（已清理，R14）。

## 0. 结论摘要

| 分级 | 计数 | ID |
| :--- | :--- | :--- |
| P0 阻断诊断 | 3 | W-01, W-02, M-01 |
| P1 显著削弱 | 10 | W-03, W-04, W-06, M-02, M-03, I-01, I-02, I-03, I-04, I-05 |
| P2 体验/效率 | 5 | W-05, I-06, I-07, I-08, I-09 |
| 合计 | 18 | 错 6 / 漏 3 / 优化 9 |

**一页结论**：这条 trace 之所以无法定位失败原因，**不是缺一个字段，而是三处链路缺口叠加**：
① 生产方把 `tool_calls[2].error/output` 留空，消费端**没有任何校验**把"缺失"识别成缺陷（M-01，实测 `status=error` 无 `error` 仍 201 落库）；
② 契约里**没有 LLM 调用级 span / 重试 / 超时**的位置，`turns` 是自由 JSON，服务端连 status 枚举都不校验（M-02、I-02，实测非法 status 与 `"not-an-object"` 均入库）；
③ 有了数据也**找不回、看不出**：`List` 无 run_id/时间窗/失败状态筛选且未知参数被**静默忽略**（I-03，实测 8 种参数 200 且结果集不变），控制台**完全不渲染 `turn.status`**、无"失败原因"区（I-05，`grep -c "turn\.status"` = 0）。
在此之上还有两枚**随时会炸的地雷**：同 turn 二次上报**终态被旧值吃掉**（W-01，实测 `running`→`failed` 后库里仍是 `running` 且错误详情消失），并发上报**19/20 丢轮次**（W-02，实测）。这两点本次事故侥幸未触发（单轮单次上报），但任何"先 running 后 failed"或"SSE 并发增量上报"的生产者都会踩。
另外这条 trace 自身**语义自相矛盾**且无人拦截：3 次 `format_jd_to_json` 里 2 次 `status=success` 而 `output` 正文是"职位解析失败"，控制台按 success 染成绿色（W-06）——"到底成功了没有"在 trace 内无法判定。

### 六个必答问题（速答）

| # | 问题 | 答案 | 证据 |
| :-- | :--- | :--- | :--- |
| 1 | 未知顶层字段是否被静默丢弃？turns 有 schema 校验吗？ | **会丢**（typed struct 反序列化后重新序列化，无 flatten/deny_unknown_fields）：实测 6 个未知顶层字段 POST→201，GET 全无；**turns 零校验**（任意 JSON 均入库）。本次实物字段恰好全在 struct 内，故本次未丢 | R2, R13 / I-01, I-02 |
| 2 | 同 turn 二次上报终态是否被旧值覆盖？ | **是**，且是设计如此（"重复轮次以旧为准"）。`running`→`failed` 后库里仍 `running`、`tool_calls` 被清空为 `[]`；而**行级 `total_*` 却被二报覆盖** → 数据自相矛盾 | R3 / W-01 |
| 3 | `total_*` 口径？会与 turns 不一致吗？ | 首报=客户端原值；二报（双方都有 turns）=按 turns 求和覆盖；二报不带 turns=保留客户端值。**三者可同时存在**：实测 `total_input_tokens=99999` 而 turns 求和 `10` | R4 / W-03 |
| 4 | List 能否按 run_id / turn_id / 时间窗 / 失败状态检索？卡在哪？ | **都不能**；未知参数被静默忽略（200 + 结果集不变），无分页总数、limit 不 clamp。排查会卡在"从 37 条里人肉找 + 逐条打开抽屉看 tool 状态" | R6, R7 / I-03, I-04 |
| 5 | 控制台展示 tool_call 的 error/output/arguments？turn.status？截断？失败原因区？ | arguments/output/error **都展示**（`error` 为空则整块不渲染）；**`turn.status` 完全不展示**、**无失败原因区**、`degraded`/`attempt_index` 不展示；turn 文本 500 字符折叠、tool 输出滚动不截断、payload 原样 JSON | R1 / I-05, I-09 |
| 6 | 服务侧有失败 trace → issue/告警/评审队列吗？ | **没有**。events 侧有指纹→issue→webhook→metrics 全链路（routes.rs:892/911/965），trace 侧只有 `record_trace`；`/metrics` 无 trace 指标；`PATCH` 的 `note` 被静默丢弃 | R8, R1 / M-03, I-06 |

---

## 1. 错（Wrong）

### W-01 「重复轮次以旧为准」使终态与错误详情永久丢失 — **P0**
- **证据**：`crates/core/src/trace_merge.rs:3`（契约注释"重复轮次以旧为准"）、`:69-83`（先遍历 old 插入 seen，new 只能追加新 key）、`:12-19`；`crates/core/src/pg_repository.rs:629-632`（读旧→merge→覆盖写）；单测 `trace_merge.rs:91-102` 与 `crates/core/tests/trace_repository_test.rs:248` 把该语义冻结为预期行为。**实验 R3**：首报 `running`、二报 `failed`（带 `error`/`tool_calls`）→ GET 仍是 `{"status":"running","tool_calls":[]}`；反向（先 failed 后 succeeded）也保持 failed ⇒ 是"先到先得"，不是"终态优先"。
- **因此无法回答**：「这个 turn 最后成功还是失败、失败在第几次 tool 调用、错误是什么」。任何两段式上报（running→failed、interrupted→resumed）都会把终态永久钉在第一帧。
- **建议**：合并语义改为按 `turn_id` **状态单调推进 + 字段级深合并**（非终态→终态可覆盖；`error`/`tool_calls` 非空优先；保留 `attempts[]` 历史）。成本 M（1–2 人日，含单测改写与回归）；**契约影响：是**——需与 T1 契约矩阵、生产者（job_copilot 的 terminal delivery 用例）同步，属语义破坏性变更，建议带 `merge_semantics: v2` 标记灰度。

### W-02 并发的同 session 上报丢轮次（read-modify-write 无事务/无锁） — **P0**
- **证据**：`pg_repository.rs:619-627`（`SELECT payload`）→ `:629-632`（内存 merge）→ `:659-695`（`INSERT … ON CONFLICT(session_id) DO UPDATE`），三步之间无事务、无行锁、无版本号；`sqlite_repository.rs:656-732` 同构。**实验 R11**：20 轮 × 2 并发（同 session，不同 turn_id）→ **19/20 会话只剩 1 个 turn**。
- **因此无法回答**：「这次 run 到底跑了几轮、哪一轮失败、总耗时/耗 token 是多少」——trace 可能系统性地少一半轮次（且不报错、不告警）。
- **建议**：① 止血：把"读旧 payload → merge → 写回"包进事务并 `SELECT … FOR UPDATE`（注意 jsonb 的 `||` 只能覆盖顶层键，**合并不了 turns 数组**，不能单靠一条 UPDATE 解决）；② 根治：`traces`（会话行）+ `trace_turns`（`UNIQUE(session_id, turn_id)` 行级 upsert），读接口聚合。成本 M（①0.5 人日，②1–2 人日 + migration）；**契约影响：是**（存储形态变更，读接口形状保持兼容）。**注意**：本项与 W-01 相互独立——修 W-01 不解决竞态，修竞态不解决终态覆盖。

### W-03 `total_*` 三套口径并存，且可与 turns 永久矛盾 — **P1**
- **证据**：`routes.rs:1230-1232`（首报原样存客户端值）；`trace_merge.rs:41-49`（双方都有 turns 时按 turns 求和**覆盖**客户端值）；`trace_merge.rs:52-55` + `pg_repository.rs:636-657`（二报不带 turns 时保留客户端值，并写入 `payload.total_*` 污染 payload）。**实验 R4**：C1 首报 `total=1000` 而 turns 求和 `10`（并存）；二报后变 `30`（客户端 7777 被丢）；C2 二报不带 turns → `total=99999` 而 turns 求和 `10`。
- **因此无法回答**：「这次 run 烧了多少 token、哪一轮最贵、成本回归了吗」——同一行在不同上报次数下语义不同，聚合看板（控制台 Tokens 列 `App.vue:665`）会给出假数。
- **建议**：定单一真相源：**服务端永远按 turns 求和**（并对缺 token 的 turn 置 `stats_incomplete`），客户端 `total_*` 仅作交叉校验（差异 >5% 记 `warnings` 回显）。成本 S（0.5 人日 + 单测）；**契约影响：是**（`total_*` 语义从"客户端上报值"变"派生值"，需与生产者对齐——其现值=`单 turn 值`）。

### W-04 `run_id` / `turn_id` 列与 payload turns 脱钩（session 单行 + 列被最后一次上报覆盖） — **P1**
- **证据**：`migrations/0003_add_traces.sql:4`（`session_id TEXT NOT NULL UNIQUE`）、`migrations/postgres/0003_add_traces.sql:4` 同；`pg_repository.rs:665-667`（`ON CONFLICT` 时 `run_id = EXCLUDED.run_id`, `turn_id = EXCLUDED.turn_id`）。**实验 R5**：同 session 先报 `run_A/A1(failed)` 再报 `run_B/B1(succeeded)` → 库里 1 行、列 `run_id=run_B`、`turns=[A1,B1]`。
- **因此无法回答**：「这条 trace 属于哪个 run / 哪个 turn」——列语义（最近一次）与 payload 语义（全部轮次）冲突；session_id 又是哈希（`thread_ref_v1_*`），事故复原时无法反查。
- **建议**：`run_id/turn_id` 下沉到 turn 粒度（turn 自带 `run_id`，行级列改为"最近一次"并在 OpenAPI/契约里写明），或加 `session_runs` 关联表；列表展示改为"轮次×run"。成本 M；**契约影响：是**（列语义需写入契约；读接口可保持兼容）。

### W-05 `reported_at_ms` 无校验（未来时间可入），列表按 `created_at` 排序而界面按 `reported_at` 显示 — **P2**
- **证据**：`routes.rs:1191-1194`（`from_timestamp_millis` 无上下界）→ `pg_repository.rs:751`（`ORDER BY created_at DESC`）；控制台 `App.vue:670`（`relTime(trace.reported_at || created_at)`）。**实验 R9**：`reported_at_ms=9999999999999` → 201，落库 `2286-11-20`。
- **因此无法回答**：「这条 trace 到底是什么时候发生的、事件先后顺序如何」——时钟漂移/恶意值会让时间线排序与显示互相矛盾。
- **建议**：`reported_at` 与 `created_at` 偏差 >24h 时拒绝（422）或落 `clock_skew: true` 标记；列表默认按 `reported_at` 排序并暴露 `order=reported_at|created_at`。成本 S；**契约影响：小**（新增可选校验/参数）。

### W-06 `status=success` 而 `output` 是失败文案的语义矛盾被原样接收并渲染为"成功" — **P1**
- **证据**：样本 `03-trace-payload.raw.json`：`tool_calls[0]`、`tool_calls[1]` 均 `status:"success"`，但 `output` 正文为「职位解析失败（原因: 模型返回空结果）」且 `error:null`（实际 3 次调用全部失败，仅第 3 次标了 `error`）；消费端无任何一致性 lint（`routes.rs:1196-1236` 只做脱敏）；控制台按 status 染色：`App.vue:1400-1406`（success → `text-emerald-400`）+ `:854` 渲染 `success (39.7s)`。
- **因此无法回答**：「3 次 `format_jd_to_json` 到底成功了几次、重试是否耗尽」——trace 内 status 与 output 互相打脸，机器可读的结论（2 成功 1 失败）与人工阅读的结论（3 次全失败）**相反**，任何按 status 的统计都会把失败计成成功。
- **建议**：① 服务端一致性 lint：`status=success` 且 `output` 命中失败特征（含 `error`/失败关键词/`status=error` 的兄弟调用同 tool）→ 落 `status_conflict: true` 并在控制台黄标；② 长期要求生产者由 tool 执行结果派生 status（属 T3 的生产方语义）。成本 S（0.5 人日，消费者侧可先行）；**契约影响：是**（payload 新增标记字段；生产者 status 语义需 T3 定稿）。

## 2. 漏（Missing）

### M-01 `status=error` 而 `error`/`output` 为空的失败证据被原样接受，且"没有错误"与"没上报"不可区分 — **P0**
- **证据**：样本 `03-trace-payload.raw.json` 的 `turns[0].tool_calls[2]` = `{"status":"error","error":null,"output":null,"arguments_summary":…}`；消费端 `routes.rs:362`（turns 自由 JSON）+ `:1196-1236`（无校验直接落库）；**实验 R2**（`status=error` 无 `error` 字段仍 201）；控制台 `App.vue:875-878`（`v-if="call.error"` → 空则整块不渲染，视觉上"没有错误"）。
- **因此无法回答**：「`format_jd_to_json` 第 3 次失败的直接原因是什么（异常类型/消息/堆栈）」。这正是本次事故的核心诊断缺口：库里 3 个 tool_call 有 1 个标记失败，但**失败原因字段是空的**，控制台看起来一切正常。
- **建议**：消费者侧独立可做——① 契约要求 `status∈{error,failed}` 时 `error` 必含 `{type,message}`；缺失时服务端落 `error_contract: "missing"` 并在控制台红标 + 列表加 `has_error_missing` 筛选；② 空 `error` 时前端显式渲染"未上报"而非隐藏区块。成本 S（0.5 人日，消费者侧可先行）；**契约影响：是**（新增校验/标记字段，向后兼容）。

### M-02 无 LLM 调用级 span、无重试/超时语义的位置 — **P1**
- **证据**：`routes.rs:351-369`（`AgentTraceIngestRequest` 只有 `turns: Vec<Value>`，无 span/llm_calls/attempts 约定）；`crates/ingest/src/validate.rs:28-37` 的字段级上限/归一**只服务 events**，trace 侧 `validate::` 零引用（R1）；样本 turn 实物字段仅 `step/model/status/degraded/attempt_index/duration_ms/*_tokens/cache_hit_tokens/tool_calls`，`attempt_index:1` 而生产方实际重试 3 次（简报 §1 事故取证，`/home/dm/job_copilot/.dev-team/evidence-cards-run/05-verdict.md`）。
- **因此无法回答**：「`ValueError: expected value at line 1 column 1` 来自哪次 LLM 调用、原始响应长什么样、3 次重试各耗时多少、120s 超时发生在 `embedded_runtime` 的哪个节点」。
- **建议**：定义可选 `turns[].llm_calls[]`（`{call_id, model, prompt_digest, finish_reason, parse_error, duration_ms}`）与 `turns[].attempts[]`（`{index, started_at, outcome, error}`），服务端无需建表（turns 已是自由 JSON）。成本 M（主要成本在生产方；消费者侧只需契约 + 渲染）；**契约影响：是**（新增可选数组，向后兼容）。

### M-03 失败 trace 既无处置通道也无归因聚合（note 被丢弃、无 issue 化、无告警、无指标） — **P1**
- **证据**：`routes.rs:383-386`（`UpdateTraceRequest.note`）+ `pg_repository.rs:771-791`（只写 `eval_status/updated_at`，**实验 R8** note 全文 0 命中）；`routes.rs:1171-1244`（trace 路径无指纹/issue/webhook）对比 `routes.rs:892`（`compute_issue_fingerprint`）→ `:911`（`record_event_and_upsert_issue`）→ `:965-1010`（webhook）；`routes.rs:735-780`（`/metrics` 只有 issues/events/ingest，无 trace）。
- **因此无法回答**：「这次失败有没有人处理、结论是什么、同类失败最近又犯了没」——失败 trace 只能靠人主动打开控制台翻，且标注无理由、无归属、无计数。
- **建议**：① `note` 落库（`traces.review_note` 或 `payload.review`）并回显；② 复用事件侧能力：`turn.status=failed` 时以 `project + tool_name + error.type` 计算指纹建 issue（或至少打 `trace.failed` webhook + `pony_sentry_traces_total{turn_status="failed"}` 指标）；③ 控制台加"待评审"队列（`eval_status=unreviewed && 存在 failed turn`）。成本 M（①S、②M、③S）；**契约影响：是**（新增 PATCH 字段语义、新 webhook 事件类型、新指标名）。

## 3. 优化（Improve）

### I-01 未知顶层字段被静默丢弃（无 flatten 兜底、无 deny_unknown_fields、无日志） — **P1**
- **证据**：`routes.rs:351-369`（typed struct，无 `#[serde(flatten)]`，全仓库无 `deny_unknown_fields`，R1）→ `routes.rs:1196`（`serde_json::to_value(&payload)` 用**反序列化后的 struct** 重新序列化，原始 JSON 未被保留）。**实验 R2**：POST 含 `error/spans/llm_calls/sdk_version/user_id/tenant` 6 个未知顶层字段 → 201，GET 后 6 个键全部不存在（`has_*` 全 false）。
- **因此无法回答**：「生产方到底有没有上报错误对象/span 维度」——上报了也看不见，且**无 4xx、无 warning、无日志**，双方都不知道丢了。注意不对称：**顶层字段丢，turn 内字段不丢**（`turns` 是 `Vec<Value>`）。
- **建议**：① `#[serde(flatten)] extra: Map<String,Value>` 保留未知顶层字段到 `payload._unknown`（并计数告警）；或 ② `deny_unknown_fields` + 400 让生产者立刻发现契约漂移。成本 S（0.5 人日）；**契约影响：是**（行为从"静默丢"变"保留/拒绝"，需与 T1 契约同步）。**本次未触发**（R13：事故实物的 13 个顶层键全在 struct 内），属潜在风险。

### I-02 `turns` 无任何 schema 校验（status 枚举、必填、类型均不校验） — **P1**
- **证据**：`routes.rs:362`；trace 路径不引用 `validate.rs`（R1）。**实验 R2**：`turns` 同时含 `status:"totally_bogus_status"`、`"not-an-object"`、`123` → 仍 201 并原样落库。
- **因此无法回答**：「这条 trace 的数据可信吗」——脏数据静默入库，控制台 `getTraceTurns`（`App.vue:1292-1301`）对非对象元素渲染成空白卡片，统计口径被污染；也没有任何"降级计数"可观测。
- **建议**：定义 `Turn`/`ToolCall` 强类型（status 枚举 + `unknown` 降级、`turn_id` 必填、`tool_calls[].error` 条件必填、数值非负），非法值**降级 + 回写 `warnings[]`**（复用 events 侧 `level` 降级策略，`validate.rs:51-61`）。成本 M（1 人日）；**契约影响：是**（新增校验与降级语义，需明确 4xx vs 降级）。

### I-03 List 缺 run_id / turn_id / 时间窗 / 失败状态筛选，未知参数静默忽略 — **P1**
- **证据**：`routes.rs:371-380`（`TraceQuery` 仅 session_id/eval_status/environment/release/project/limit/offset）、`pg_repository.rs:717-769`；**实验 R6**：`?run_id=` / `?turn_id=` / `?status=failed` / `?turns.status=failed` / `?since=` / `?from=` / `?created_after=` / `?has_error=` / `?nonsense=` **全部 HTTP 200 且结果集恒为 37 行**（参数被 serde 静默忽略）。
- **因此无法回答**：「本次事故的 trace 在哪、同期还有哪些失败 run、失败集中在哪个工具」。排查实际卡在：**列表 37 条无状态列 → 逐条打开抽屉 → 只能从 tool_call 颜色猜**（`App.vue:653-667` 不显示任何 status）。
- **建议**：① 增加 `run_id`/`turn_id` 精确过滤（turns 已含 run_id，可走 `payload #>` 表达式 + GIN 索引）；② 抽 `turn_status`/`failed` 布尔列（`turn_status` 取自 turns 汇总，随写入维护）支持 `status=failed`；③ `from/to` 时间窗（`reported_at`）；④ 未知参数返回 400 或 `warnings`。成本 M（①S 先上，②M 需 migration）；**契约影响：是**（查询参数扩展，向后兼容）。

### I-04 List 无 limit clamp、无 payload 投影、无总数/游标 — **P1**
- **证据**：`pg_repository.rs:753-755`（`filter.limit.unwrap_or(50)` 直接拼 SQL，**不 clamp**；`offset` 也未 `max(0)`）对比 issues 列表 `pg_repository.rs:395` 的 `.clamp(1, 100)` 与 `:396` 的 `.max(0)`；`pg_repository.rs:718-720`（Select 出完整 `payload::text`）；控制台 `App.vue:1410`（固定 `?limit=100`，无分页）。**实验 R7**：`?limit=100000` → HTTP 200、**2,012,669 bytes**；payload 实测 avg 29 KB / max 169 KB（业务行），单行上限 ≈2 MB（2.0 MB → 201，2.1 MB → 413）。
- **因此无法回答**：「失败 trace 的翻页与全量检索」——当前 37 行还能一眼看完，量级上去后控制台一次拉 100×29KB≈3MB、无总数、无游标，且 `limit` 可被任意放大（一次请求即可拉全库 payload）。
- **建议**：① `limit.clamp(1,100)`；② `?fields=summary` 投影（不返回 payload，只返回 id/session/totals/turn 计数/状态汇总）；③ 返回 `{items, total, next_offset}` 或游标。成本 S–M（0.5–1 人日）；**契约影响：是**（响应形态新增字段，建议新路由 `/api/v1/traces/summary` 或响应包一层，避免破坏 `TraceRecord[]` 兼容）。

### I-05 控制台不展示 `turn.status`、无「失败原因」区；空 error 整块隐藏；`degraded`/`attempt_index` 不可见 — **P1**
- **证据**：`web/src/App.vue:764-770`（turn 头只渲染 `phase/seq/model/duration`）、`:771-775`（tokens）、`:875-878`（`v-if="call.error"`）、`:653-667`（列表行无 status 徽标，只有 Turns/Tokens/耗时）、`:889`（payload 原始 JSON 兜底，需人肉读 2KB+）。**静态探针 R1**：`grep -c "turn\.status"` = **0**；`degraded` 5 处命中全是网关健康态，与 `turn.degraded` 无关；`attempt_index` 0 处。
- **因此无法回答**：「这次 run 失败了吗？为什么失败？」——列表看不出失败，抽屉里 `turn.status=failed` 不显示；本 trace 的 tool error 为空 ⇒ 错误区块被 `v-if` 隐藏，界面呈现"一切正常"。唯一的线索是第 3 个 tool_call 的红色 `error` 状态字样。
- **建议**：① 列表加 turn 状态汇总徽标（`failed/interrupted` 计数，红/黄）；② 抽屉顶部加「失败原因」区（`turn.status` + `turn.error` + tool 的 `error` + `degraded` + 空则显式「未上报」）；③ 展示 `attempt_index`（重试次数）与 `degraded`；④ payload JSON 折叠为默认收起。成本 S（0.5–1 人日，纯前端）；**契约影响：无**。

### I-06 服务端缺 trace 观测面：无 trace 指标、无字段级上限、retention 不可按项目分层 — **P2**
- **证据**：`routes.rs:735-780`（`/metrics` 只有 issues_total / events_total / ingest_requests_total / ingest_level_degraded_total 四组，**无任何 trace 指标**）；`server/src/main.rs:46`（`RETENTION_DAYS_TRACES=90`，全局固定）；`routes.rs:422`（`DefaultBodyLimit::max(2MB)` 是唯一硬限）；对比 events 侧 `validate.rs:28-37`（message 2048、extra 8KB、tags 32 条…）。**实验 R7**：单行 2.0 MB 被接受。
- **因此无法回答**：「trace 采集本身健康吗（失败率、丢弃率、合并丢弃轮次、体积增长）」——采集侧静默失效无人知（W-02 的丢轮就无任何计数），也无成本上限依据。
- **建议**：加 `pony_sentry_traces_total{project,turn_status}`、`trace_merge_lost_turns_total`、`trace_payload_bytes` 直方图；`record_trace` 记录 drop/merge 计数；payload 超阈值时截断 tool `output` 并打 `truncated: true`。成本 S–M；**契约影响：小**（新增指标名 + payload 内部标记字段）。

### I-07 脱敏管道对 trace **证物**无差别套用 events 规则：路径就地改写、无 redaction 计数回传 — **P2**
- **证据**：`crates/ingest/src/sanitizer.rs:167-176`（`sanitize_string` = 密钥正则 + `sanitize_path`）、`:116-143`（深度 32 上限，超限替换为 `"[MAX_DEPTH_EXCEEDED]"`）、`routes.rs:1200`（整包 payload 再扫一遍）。**实验 R10**：`File "/home/dm/job_copilot/api_v2/src/.../embedded_runtime.py"` → `File "[USER_HOME]/job_copilot/api_v2/.../embedded_runtime.py"`；`access_token` → `[REDACTED_SECRET]`（设计行为）；`token_count: 12` 保留（白名单正确）。
- **因此无法回答**：「异常发生在哪个部署路径/哪台机器」（`[USER_HOME]` 抹掉宿主用户名与绝对路径）；也无法判断"证物是否被改写"——trace 路径不返回任何 redaction 计数（events 侧有 `warnings`）。
- **说明**：脱敏是设计行为（简报 §8 已声明），此处**不判为错误**；可优化点是**范围与可观测性**：`/home/<user>` 对 trace 的 `tool_calls[].output`（代码/异常文本）破坏可复现性，建议改为保留仓库相对路径 + 只对"显式 secret 键"脱敏，并在 `payload._redactions` 记录计数。成本 S（0.5 人日）；**契约影响：是**（payload 新增内部字段；脱敏粒度变更需与安全评审）。

### I-08 trace ingest 契约没有文档之家（README 只有鉴权表，无 turns/tool_calls 字段契约） — **P2**
- **证据**：`README.md:131-133`（`/api/v1/traces` 只出现在鉴权表）；`docs/decisions/` 无 trace 相关 ADR；`docs/INDEX.md` 无 trace 条目；字段语义只存在于 `routes.rs:351-369` 代码里。
- **因此无法回答**：「谁应该发什么字段、哪个字段是必填、非法值怎么办」——这次"错误详情为空"双方都能各自解释成对方的问题，正是因为契约无原文可对。
- **建议**：`docs/` 新增 trace 契约页（对齐 T1 的字段矩阵 + 校验/合并/口径三节），并加 same-commit 维护约束（命约 4）；成本 S（0.5 人日，文档）；**契约影响：文档层，无代码**。

### I-09 `stats_incomplete` 徽标是死契约（前端渲染，服务端从无写入） — **P2**
- **证据**：`App.vue:702`（`v-if="selectedTrace.payload?.stats_incomplete === true"` → "部分轮次缺少 token 数据，统计不完整"）；全 Rust 侧 `grep stats_incomplete` **0 命中**（R1）。
- **因此无法回答**：「这份 trace 的 token 统计是否完整」——徽标永远不出现，而 W-03 证明统计确实可能不完整。
- **建议**：要么实现（merge/写入时对缺 `input_tokens/output_tokens` 的 turn 置 `stats_incomplete: true`，与 W-03 的"服务端求和"方案天然配套），要么按"删除前先搜生产者"立法删掉该徽标。成本 S；**契约影响：是**（payload 新增字段，需与 W-03 方案一并定义）。

---

## 4. 实验卫生附注（纪律与自报）
1. 首次清理误用 `delete … where session_id like 'audit-probe-%'` → `DELETE 3`（1 行本人 + 2 行先前已存在），已即时向 Lead 自报；业务行与事故行未受影响（R14 收据）。
2. 此后全部实验统一前缀 `audit-probe-core-*`，**入库前预检 0**，**清理只按精确 session_id**，且**删除前后各查一次 count**（R14）。
3. 全部实验写入共 31 行（fieldloss 1 / merge 2 / total 2 / run 1 / big 1 / patch 1 / noturns+future 2 / sanitize 1 / race 20），均已删除；终态：`audit-probe%` = 0，`project='job_copilot'` = 37，事故行 `ca0b5494` 完好。

## 5. 交付给 T1 / T3 的接口（需要对方确认或补位）
- **给 T1（契约矩阵）**：`turns[]`/`tool_calls[]` 目前是 **无契约自由 JSON**，请把"应有字段"作为建议契约列出；I-01/I-02/I-03/W-03 的修复都依赖该矩阵定稿。
- **给 T3（生产者语义）**：`tool_calls[2].error/output` 为空的**生产者成因**（`payload.py:294-296` 的 `str(tc.get("error") or "")[:500] if tc.get("error") else None` 与 500 字符截断）属生产方语义，本报告只判定消费端"不校验/不可见"（M-01）；`total_*` 的生产口径（`payload.py:351-375`）与 W-03 的服务端重算需两侧对齐。
- **给 T4（verifier）**：若你的在飞实验行曾被我在 R0.1 误删波及，请重跑；可直接重放 `02-receipts.txt` R2/R3/R4/R11 四个实验（脚本已在收据中给出），预期结果逐项标注；W-01/W-02 的判定必须用真实 HTTP + DB 计数复现，不接受静态推理。
