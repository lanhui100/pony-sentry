# pony-sentry Trace 采集质量审核报告（草案 v1）

> 样本：job_copilot Tilt dev 会话 `efd84b9f-1956-4660-be61-91cf1ab243d1`（cards 页面）的一次失败 run
> 审核对象：pony-sentry 对这次 run 采集到的 trace（库内行 `ca0b5494-ae1b-47db-8e00-c9e7abbb6779`）
> 审核日期：2026-10-23 · 状态：待独立验证与对抗审查 · 不修改任何业务代码，不修复被观测的故障

---

## 0. 一页结论

**这份 trace 足以回答「失败了」，不足以回答「为什么失败」。**

- **诊断覆盖率 37.5%**（8 个定位必答题：能答 1、部分 4、不能 3；严格可答率 12.5%）。事故真因链 5 环中，
  只有第 ⑤ 环（终态失败）能被字段直接指认；② 错误机制、③ 重试耗尽、④ 超时兜底三环完全不可定位。
- **共 18 条发现：错 6 / 漏 3 / 优化 9**，其中 **P0 3 条、P1 10 条、P2 5 条**。
- **根因不在存储能力，而在「跨项目契约供给 + 消费者缺护栏」两处**：
  1. 生产者按 R1 契约**不外发任何错误文本**，且**从不上报 LLM 调用级 span、turn 时间戳**——trace 天生看不到失败机制；
  2. 消费者对 `turns/tool_calls` **零 schema 校验**、合并语义是**「重复轮次以旧为准」**（终态可被旧值永久钉住）、
     并发上报**读-改-写无事务**（实测 20 轮并发丢 19 轮），且控制台**不渲染 `turn.status`**——即使数据来了也可能被吞或被藏。
- **最划算的改法**：先做 5 个纯生产者、零 schema 变更、零裁决的字段补全（约 2–2.5 人日，覆盖率 37.5% → ~75%），
  再做消费者侧 2 个 P0 止血（合并语义 + 并发事务），最后做控制台展示与检索增强。

| 分级 | 计数 | 条目 |
| :--- | :--- | :--- |
| P0 阻断诊断 | 3 | W-01 终态被旧值吞 · W-02 并发丢轮 · M-01 失败证据为空且不可区分 |
| P1 显著削弱 | 10 | W-03, W-04, W-06, M-02, M-03, I-01, I-02, I-03, I-04, I-05 |
| P2 体验/效率 | 5 | W-05, I-06, I-07, I-08, I-09 |

> 结论一句话：**pony-sentry 的 trace 目前是「评测语料通道」，不是「故障诊断通道」**——`eval_status` 词表
> （`triage_good/triage_bad/eval_dataset/optimized/wontfix`）也印证了这一定位。要它承担排障，必须补诊断字段 + 补消费者护栏，
> 而不是继续在现有字段上做展示优化。

---

## 1. 审核方法与证据基线

### 1.1 三层证据（每一层都可重放）

| 层 | 取法 | 落盘收据 | 关键事实 |
| :--- | :--- | :--- | :--- |
| L1 库内真值 | `psql`（Postgres `ponysentry.traces`） | `00-baseline/01-db-truth.txt` | 1 行 / 37 条 trace；`payload=3405B`；`environment=development`；`release=dev` |
| L2 HTTP 读接口 | `GET /api/v1/traces/:id`（admin token） | `00-baseline/02-api-get.json` | 返回体与库行逐键一致，**无服务端派生字段**（无 turn 状态汇总、无错误摘要） |
| L3 控制台产物 | `curl /`（`web/dist` 内嵌产物）+ 静态检查 | `40-verify/artifacts/console-index.html` | 列表行不显示任何 turn 状态；抽屉里 `turn.status` **零渲染**（`grep -c "turn\.status"`=0）；`error` 为空时整个错误块被 `v-if` 隐藏 |

### 1.2 被观测事故的真因链（只读引用，不重开调查）

`/home/dm/job_copilot/.dev-team/evidence-cards-run/05-verdict.md`、`03-error-signature.txt`：

```
① jd_v6 文本链路切到 workbuddy/deepseek-v4.1-flash
② 结构化输出首个 chunk 非法 → SDK from_json("") → ValueError: expected value at line 1 column 1
③ format_jd_to_json 3 轮工具重试（每轮 2 次 LLM 调用）全部空结果 → 重试耗尽（MAX_RETRIES=1）
④ embedded_runtime 节点 120s 兜底 → TimeoutError（19:27:19.70 + 120s ≈ 19:29:19.86 实测）
⑤ SSE run_error → 前端 sse-event-error（19:29:30.58）
```

### 1.3 审核判据与分工

| 角色 | 职责 | 产物 |
| :--- | :--- | :--- |
| protocol-scribe | 27 条判据（W1-W8/M1-M8/I1-I11）+ 三列字段矩阵（39 字段行） | `10-contract/` |
| auditor-core | 消费者侧 18 条结论 | `20-core/` |
| auditor-diagnostic | 诊断八问 + 生产者语义 + 跨项目建议 13 条 | `30-diagnostic/` |
| verifier | 独立复现 + 逐条裁决（CONFIRMED/REFUTED/UNVERIFIABLE） | `40-verify/` |
| lead | 基线收据、定点复核、汇总与裁决 | `00-baseline/`、本报告 |

### 1.4 声明边界

- 本报告不修复被观测故障，不改 job_copilot / pony-sentry 任何业务代码；
- 脱敏（`[REDACTED_SECRET]`/`[USER_HOME]`）、`session_id` HMAC 化是**已知设计**，不计为缺陷（仅评估其代价）；
- 标注 `[NON-MACHINE]` 的结论靠代码阅读与 review，其余均有可重放命令。

---

## 2. 样本 trace 事实卡

```jsonc
{ "session_id": "thread_ref_v1_e9b4ec395c96d8913c106ef8",   // HMAC(thread_id)，不可逆
  "run_id": "run_83c1879e338d442f953a5dd4b8d1646a",
  "turn_id": "turn_5dd7639f928549dfb86d96cbe6cbf33a",
  "thread_id": null,                                        // payload 里的 key 存在但值恒空
  "project": "job_copilot", "environment": "development", "release": "dev",
  "turns": [ { "status": "failed", "step": "other",
               "model": "workbuddy/deepseek-v4.1-flash",
               "duration_ms": 120272, "attempt_index": 1, "degraded": false,
               "input_tokens": 36612, "output_tokens": 1368, "cache_hit_tokens": 23936,
               "tool_calls": [
                 {"tool_name":"format_jd_to_json","status":"success","duration_ms":39706,
                  "error":null,"output":"…职位解析失败（原因: 模型返回空结果）…"},
                 {"tool_name":"format_jd_to_json","status":"success","duration_ms":39342,
                  "error":null,"output":"…职位解析失败（原因: 模型返回空结果）…"},
                 {"tool_name":"format_jd_to_json","status":"error","duration_ms":19554,
                  "error":null,"output":null}
               ] } ] }
```

六条直观观察：

1. **只有一个 turn 聚合**，事故中 8 次 LLM 调用 / 3 轮重试全部压平进这一个 turn；
2. **没有任何错误类型/消息/堆栈**：3 个 tool_call 的 `error` 全为 `null`，含 `status=error` 的那条；
3. **状态语义自相矛盾**：2 条 `status=success` 的 `output` 正文是「职位解析失败」；第 3 条实际是 **cancelled**（被 120s 超时杀掉），在 trace 里被收敛成 `error`；
4. **时间只有一个瞬时值**（`reported_at_ms`），无开始/结束、无每次调用耗时分布；
5. **没有用户输入**（`input_text` 缺失，尽管生产者契约 R19 要求包含）；
6. **`thread_id` 键存在但恒 `null`**，`session_id` 是单向 HMAC，跨项目回链只能靠 `run_id` 手工 grep 日志。

---

## 3. 错（Wrong）

> 判定口径：trace 内记录的内容与事实不符，或语义自相矛盾。每条给「证据 → 因此无法回答 → 建议」。

### W-01 「重复轮次以旧为准」让终态与错误详情永久丢失 · **P0** · 归属：消费者
- **证据**：`crates/core/src/trace_merge.rs:3,69-83`（旧轮先入 `seen`，新轮只能追加新 key）、`crates/core/src/pg_repository.rs:629-632`；
  单测 `trace_merge.rs:91-102`、`crates/core/tests/trace_repository_test.rs:248` 把该语义冻结为预期。
  实测（`20-core/02-receipts.txt` R3）：首报 `running` → 二报 `failed`（带 `error`）后，GET 仍是 `{"status":"running","tool_calls":[]}`。
- **因此无法回答**：这个 turn 最后成功还是失败、失败在第几次调用、错误是什么。
- **建议**：合并改为**状态单调推进 + 字段级深合并**（非终态→终态可覆盖；`error`/`tool_calls` 非空优先；保留 `attempts[]` 历史）。
  成本 M（1–2 人日）；**契约影响：是**（破坏性，建议 `merge_semantics:v2` 灰度）。

### W-02 并发上报丢轮次（读-改-写无事务/无锁） · **P0** · 归属：消费者
- **证据**：`crates/core/src/pg_repository.rs:619-632`（SELECT → merge → ON CONFLICT 三步无事务）、`sqlite_repository.rs:656-732` 同构；
  实测（R11）20 轮 × 2 并发同 session → **19/20 只剩 1 个 turn**，且不报错、不告警。
- **因此无法回答**：这次 run 到底几轮、哪轮失败、总耗时/总 token 是多少。
- **建议**：止血 `SELECT … FOR UPDATE` + 事务（注意 jsonb `||` 合不了数组）；根治拆 `trace_turns(session_id,turn_id UNIQUE)` 行级 upsert。
  成本 M；**契约影响：是**（存储形态，读接口形状兼容）。

### W-03 `total_*` 三套口径并存，可与 turns 永久矛盾 · **P1** · 归属：消费者 + 生产者
- **证据**：`routes.rs:1230-1232`（首报原样存客户端值）、`trace_merge.rs:41-49`（双方有 turns 时求和覆盖）、`trace_merge.rs:52-55`（二报无 turns 时保留客户端值）；
  实测（R4）：`total_input_tokens=99999` 而 turns 求和 `10` 可同时成立；Lead 复核见 `00-baseline/05-lead-probes.md` P4。
- **因此无法回答**：这次 run 烧了多少 token、哪一轮最贵、成本是否回归。
- **建议**：服务端**永远按 turns 求和**并对缺 token 轮次置 `stats_incomplete`；客户端值仅作交叉校验（差异 >5% 记 `warnings`）。成本 S；**契约影响：是**（`total_*` 语义改为派生值）。

### W-04 `run_id/turn_id` 列与 payload turns 脱钩 · **P1** · 归属：消费者
- **证据**：`migrations/0003_add_traces.sql:4`（`session_id UNIQUE` 单行/会话）、`pg_repository.rs:665-667`（冲突时列被最后一次上报覆盖）；
  实测（R5）：同 session 先报 `run_A/A1(failed)` 再报 `run_B/B1(succeeded)` → 1 行、列 `run_id=run_B`、`turns=[A1,B1]`。
- **因此无法回答**：这条 trace 属于哪个 run / 哪个 turn（列=最近一次，payload=全部）。
- **建议**：`run_id/turn_id` 下沉到 turn 粒度，列语义在契约里写明「最近一次」；列表按「轮次×run」展示。成本 M。

### W-05 `reported_at_ms` 无上下界，列表按 `created_at` 排序而界面按 `reported_at` 显示 · **P2** · 归属：消费者
- **证据**：`routes.rs:1191-1194`（`from_timestamp_millis` 无校验）→ 实测 `9999999999999` 入库为 `2286-11-20`（R9）；
  排序 `pg_repository.rs:751`（`ORDER BY created_at DESC`）vs 显示 `App.vue:670`（`reported_at || created_at`）。
- **因此无法回答**：trace 的真实发生时间与先后顺序（时钟漂移会自相矛盾）。
- **建议**：偏差 >24h 拒绝（422）或打 `clock_skew`；列表默认按 `reported_at` 并可切换。成本 S。

### W-06 `status=success` 而 `output` 是失败文案，被原样接收并渲染成"成功" · **P1** · 归属：生产者主因 + 消费者未拦截
- **证据**：样本 `tool_calls[0..1]` `status=success` + `output="…职位解析失败…"`；消费者无一致性 lint（`routes.rs:1196-1236` 只脱敏）；
  控制台按 status 染色 `App.vue:1400-1406`（success→绿）、`:854`。
- **因此无法回答**：3 次 `format_jd_to_json` 到底成功几次、重试是否耗尽——机器读法（2 成 1 败）与人工读法（3 次全败）相反。
- **建议**：① 消费者一致性 lint（`status=success` 且 `output` 命中失败特征 → `status_conflict:true` + 黄标）；
  ② 生产者由工具执行结果派生 status，并补 `business_status`。成本 S；**契约影响：是**。

---

## 4. 漏（Missing）—— 诊断充分性评估

### 4.1 八个定位必答题（判定口径：能=1 / 部分=0.5 / 不能=0）

| # | 定位必答问题 | 判定 | trace 中有的 | trace 中缺的 |
| :-- | :--- | :--- | :--- | :--- |
| D1 | 是否失败？哪个项目/环境/版本/模型？ | **能** 1.0 | `turns[0].status=failed`、project/env/release/model | — |
| D2 | 失败在哪一层/哪个节点（LLM/工具/网关/前端）？ | **不能** 0 | `step="other"`（未知节点收敛值）、3 条 tool_calls | 无 graph node、无 span 层级、无 `llm_calls[]`、无 `error.kind` |
| D3 | 失败的机制与错误签名（类型/消息/堆栈）？ | **不能** 0 | 无 | `error_type`/`error_message`/`stack`；契约 R1 明确不外发错误文本 |
| D4 | 哪个工具出问题？业务成功还是失败？为什么？ | **部分** 0.5 | tool_name、duration、arguments、失败文案 output | 无业务态字段；第 3 条 `error=null`；无重试轮次语义 |
| D5 | 是否重试？几轮？为何耗尽？ | **不能** 0 | 仅 `attempt_index=1`（运行级，不是工具/LLM 重试） | 无 tool/LLM 级 attempt、无预算/上限、无每次起止时刻 |
| D6 | 是否触发超时兜底？何时开始/何时被杀？ | **部分** 0.5 | `duration_ms=120272`（≈120s，强提示） | 无 `started_at/completed_at`、无 `error_type=TimeoutError`、无事件序列 |
| D7 | LLM 侧发生什么（几次/模型/耗时/状态/token）？ | **部分** 0.5 | 仅 turn 级 token 合计与 model | 无逐次调用、无失败调用计数、无每次耗时（实测 8 次、4 次 error 全不可见） |
| D8 | 能否跨项目回链（request_id/用户/会话/日志）？ | **部分** 0.5 | `run_id` 可 grep 日志、`tags.trace_id` | 无 `request_id`、无 `user_id`、`thread_id=null`、`session_id` 不可逆 |

**加权覆盖率 = 3.0/8 = 37.5%；严格可答率 = 1/8 = 12.5%。**
真因链 5 环中仅 ⑤ 可指认；②③④（错误机制 / 重试 / 超时）落在「不能」或「靠 120s 数字猜」。

### 4.2 三条结构性缺失（消费者视角）

### M-01 `status=error` 且 `error/output` 为空被原样接受，且"没有错误"与"没上报"不可区分 · **P0** · 归属：双方
- **证据**：样本 `tool_calls[2]={"status":"error","error":null,"output":null}`；`routes.rs:362`+`:1196-1236` 无校验直接落库（实测 R2：`status=error` 无 `error` 仍 201）；
  控制台 `App.vue:875-878` `v-if="call.error"` → 空则整块不渲染，视觉上「一切正常」。
- **因此无法回答**：第 3 次失败的直接原因（异常类型/消息/堆栈）——这正是本次事故的核心诊断缺口。
- **建议**：① 契约要求 `status∈{error,failed}` 时 `error` 必含 `{type,message}`（至少 `error_type`）；缺失落 `error_contract:"missing"` + 控制台红标 + 列表可筛；
  ② 空 `error` 时前端显式渲染「未上报」。成本 S（消费者可先行）；**契约影响：是**（向后兼容新增字段）。

### M-02 无 LLM 调用级 span、无重试/超时语义的位置 · **P1** · 归属：生产者为主
- **证据**：`routes.rs:351-369`（只 `turns: Vec<Value>`，无 spans 约定）；trace 路径不引用 `crates/ingest/src/validate.rs`。
  job_copilot 本地同一 run 有 **8 条 LLM span / 4 条 error**（`04-db-truth.txt` Q4），一条都没进 trace。
- **因此无法回答**：`ValueError: expected value at line 1 column 1` 来自哪次调用、原始响应是什么、8 次调用各耗时多少、120s 超时卡在哪个节点。
- **建议**：可选 `turns[].llm_calls[]`（`{call_id,model,provider,status,duration_ms,tokens,attempt,error_type,started_at_ms}`）与 `turns[].attempts[]`；
  消费者 `turns` 不透明存放，**零 schema 改动**即可承载。成本 M（主体在生产侧）。

### M-03 失败 trace 无处置通道：note 被丢弃、无 issue/告警/指标 · **P1** · 归属：消费者
- **证据**：`UpdateTraceRequest.note`（`routes.rs:383-386`）落库 SQL 只写 `eval_status/updated_at`（`pg_repository.rs:771-791`，实测 R8 note 0 命中）；
  trace 路径无指纹/issue/webhook（对比 events `routes.rs:892,911,965`）；`/metrics` 无 trace 指标（`routes.rs:735-780`）。
- **因此无法回答**：这次失败有没有人处理、结论是什么、同类失败最近又犯了没。
- **建议**：① `note` 落库并回显；② `turn.status=failed` 时按 `project+tool+error_type` 指纹建 issue 或打 `trace.failed` webhook + `pony_sentry_traces_total{turn_status}` 指标；
  ③ 控制台加「待评审」队列（`unreviewed` 且存在 failed turn）。成本 M。

---

## 5. 优化（Improve）

| ID | 问题 | 证据 | 建议 | 成本 | 严重度 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| I-01 | 未知**顶层**字段被静默丢弃（typed struct 反序列化后重新序列化；无 flatten/deny_unknown） | `routes.rs:351-369,1196`；实测 R2：6 个未知顶层键 201 后全无 | ①`#[serde(flatten)]` 保留到 `payload._unknown` 并计数告警；或②`deny_unknown_fields`+400 让生产者立刻发现漂移 | S | P1 |
| I-02 | `turns/tool_calls` 零 schema 校验（status 枚举、必填、类型都不校验） | `routes.rs:362`；实测 R2：`"not-an-object"`、`123`、非法 status 均入库 | 定义强类型 + 非法值降级 + `warnings[]` 回写 | M | P1 |
| I-03 | List 无 `run_id`/`turn_id`/时间窗/失败筛选，未知参数静默忽略 | `routes.rs:371-380`；实测 R6：9 种参数全 200 且结果集恒 37 行 | 先加 `run_id` 精确过滤（turns 已含 run_id，可 `payload #>` + GIN），再抽 `turn_status` 列支持 `status=failed`、`from/to` | M | P1 |
| I-04 | List 无 limit clamp、无 payload 投影、无总数/游标 | `pg_repository.rs:753-755`（不 clamp）；实测 R7：`?limit=100000` → **2,012,669 B** | `clamp(1,100)` + `?fields=summary` 投影 + `{items,total,next_offset}` 或游标 | S–M | P1 |
| I-05 | 控制台不渲染 `turn.status`、无「失败原因」区、空 error 整块隐藏、`degraded/attempt_index` 不可见 | `App.vue:653-667,764-775,875-878`；静态探针 `grep -c "turn\.status"`=0 | 列表加状态汇总徽标；抽屉顶部加「失败原因」区（空则显式「未上报」）；展示 attempt/degraded；payload JSON 默认收起 | S | P1 |
| I-06 | 缺 trace 观测面：无 trace 指标、无非 events 式字段上限、retention 全局固定 90d | `routes.rs:735-780`、`main.rs:46`、`routes.rs:422` | 加 `pony_sentry_traces_total`、`trace_merge_lost_turns_total`、`payload_bytes` 直方图；超阈值截断 tool output 并打 `truncated` | S–M | P2 |
| I-07 | 脱敏对 trace 证物无差别套 events 规则（路径就地改写、无 redaction 计数） | `sanitizer.rs:116-176`；实测 R10：`/home/dm/.../embedded_runtime.py` → `[USER_HOME]/...` | trace 保留仓库相对路径；仅在 `payload._redactions` 记录计数回传 | S | P2 |
| I-08 | trace ingest 契约无文档之家（README 只有鉴权表，无字段契约） | `README.md:131-133`；`docs/decisions/` 与 `docs/INDEX.md` 无 trace 条目 | `docs/` 新增 trace 契约页（字段矩阵 + 校验 + 合并 + 口径），same-commit 维护 | S | P2 |
| I-09 | `stats_incomplete` 徽标是死契约（前端渲染，服务端从不写） | `App.vue:702`；Rust 侧 `grep` 0 命中 | 与 W-03「服务端求和」配套实现，或按「删除前先搜消费者」删掉 | S | P2 |

---

## 6. 改进路线图（建议，本次不实施）

| 波次 | 内容 | 责任方 | 成本 | 收益 |
| :--- | :--- | :--- | :--- | :--- |
| **Wave A**（先做，零裁决） | 契约补字段：`error_type`（受控枚举）、`started_at/completed_at`、`request_id/user_id`、token 口径与调用计数、`output` 受控抽取 | 生产者（job_copilot） | ≈2–2.5 人日 | 诊断覆盖率 37.5% → **~75%**；消费者零改动（`turns` 不透明） |
| **Wave B**（止血护栏） | W-01 合并语义 v2（状态单调 + 字段级深合并）、W-02 事务/行锁（或 `trace_turns` 表）、M-01 空错误检测与红标 | 消费者（pony-sentry） | ≈2.5–4 人日 | 消除「终态丢失 / 并发丢轮 / 失败看起来正常」三类事故 |
| **Wave C**（可诊断体验） | `llm_calls[]`（生产者）、控制台失败原因区与 LLM 调用折叠、List `run_id`/状态/时间窗筛选 + 投影 + clamp | 双方 | ≈3–4 人日 | 覆盖率 → **~93.8%**；一键回链与检索 |
| **Wave D**（择机） | trace→issue/告警/指标、note 落库、脱敏粒度与 `_redactions`、契约文档化 | 消费者 | ≈1.5–2 人日 | 从「能看」到「有人管」 |

**不建议做**：外发 `error_message` 原文/traceback（违反 R1 且被脱敏清零，无信息量）；把 120s 等常量写进 trace；
消费者反解 `thread_ref_v1_*`（HMAC 单向）；绕过 `build_trace_payload` 直接 POST 自有 JSON（跳过全部脱敏与钳制）。

---

## 7. 跨项目契约提案（字段级，供后续排期）

> 全部落在 `turns[]` / `tool_calls[]` / `extra` 内，**不新增顶层键、不改 DB schema、不改合并语义**。

```jsonc
{ "extra": { "request_id": "request_…", "user_id": "510", "failover_policy": "disabled" },
  "turns": [{
    "status": "failed", "error_type": "TimeoutError",          // 受控词表，禁自由文本
    "started_at_ms": 1792754879720, "completed_at_ms": 1792754960002,
    "calls_total": 8, "calls_failed": 4, "token_scope": "billing_success_only",
    "llm_calls": [{ "model": "…", "status": "error", "duration_ms": 13240,
                    "attempt_index": 1, "error_type": "ValueError" }],
    "tool_calls": [{ "tool_name": "format_jd_to_json", "status": "cancelled",
                     "business_status": "retry_exhausted", "cancel_trigger": "node_timeout",
                     "error_type": "CancelledError", "attempt_round": 3 }]
  }] }
```

字段约束：`error_type ∈ _SAFE_ERROR_TYPES`（`trace_safety.py:287-320`），否则 `"Exception"`；
`*_at_ms` UTC 毫秒且单调；`llm_calls ≤ 50` 条；`attempt_round ∈ [1,10]` 且与运行级 `attempt_index` 语义并列声明。

---

## 8. 定位判断与残余风险

1. **定位判断（最重要的一条）**：现有 trace 的实际定位是**评测语料**（`EvalStatus` 六个值全是 triage/eval 语义），
   字段契约也按此裁剪（R1 不外发错误文本、每 run 一条终态摘要、无 span）。若要让 trace 承担排障，需要先明确目标，
   再按 Wave A/B/C 补齐；否则展示层做得再漂亮，也回答不了「为什么失败」。
2. **隐私与体积**：补字段须控体积（8 次 `llm_calls` ≈1.5–2KB，2MB 上限内）；`user_id` 建议仅 dev/preprod 外发。
3. **兼容风险**：W-01/W-03 属破坏性语义变更，需生产者同步与灰度标记；I-01 从「静默丢」变「保留/拒绝」也需双方对齐。
4. **无法机器验证项**：`[NON-MACHINE]`「合并语义改动对历史数据的迁移成本」需人工评估；`thread_ref` HMAC 等式未复算（缺 key）。

---

## 9. 附录

### 9.1 收据索引

| 收据 | 文件 |
| :--- | :--- |
| 库内真值 / payload 全集 | `00-baseline/01-db-truth.txt`、`03-trace-payload.raw.json` |
| HTTP 读接口返回 | `00-baseline/02-api-get.json` |
| Lead 定点复核（P1-P4） | `00-baseline/05-lead-probes.md` |
| 字段矩阵 / 判据 / 探针 | `10-contract/01-field-matrix.md`、`02-audit-criteria.md`、`03-probe-receipts.md` |
| 消费者侧结论与 R1-R15 | `20-core/01-findings.md`、`02-receipts.txt` |
| 诊断八问与跨项目建议 | `30-diagnostic/01..04` |
| 独立验证与裁决 | `40-verify/01-verdicts.json`、`02-independent-receipts.txt` |

### 9.2 团队与产物

Lead（基线/裁决/汇总）· protocol-scribe（契约）· auditor-core（消费者）· auditor-diagnostic（诊断/生产者）· verifier（独立验证）。

### 9.3 验证状态

> 【待 verifier 完成后回填：CONFIRMED / REFUTED / UNVERIFIABLE 计数与被推翻条目】
