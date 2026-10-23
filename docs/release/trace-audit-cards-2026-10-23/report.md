# pony-sentry Trace 采集质量审核报告

> **样本**：job_copilot Tilt dev 会话 `efd84b9f-1956-4660-be61-91cf1ab243d1`（cards 页面）的一次失败 run（`run_83c1879e338d442f953a5dd4b8d1646a`）
> **审核对象**：pony-sentry 对该 run 采集到的 trace（库内行 `traces.id=ca0b5494-ae1b-47db-8e00-c9e7abbb6779`）
> **审核日期**：2026-10-23 · **状态**：审核完成（59 条独立裁决全部 CONFIRMED；2 路对抗审查的 28 条 finding（L3-A 14 / L3-B 14）已裁决并入本文 v3；v4 追加 §11 外部基准复核（DSH）并据此修正 3 处建议；v5 记录 Wave 0 + Wave B + 选取 D 的实施落地（§12））
> **边界**：不改任何业务代码、不修复被观测的故障；仅评估 trace 的错、漏与可优化点

---

## 0. 一页结论

**这份 trace 足以回答「失败了」，不足以回答「为什么失败」。**

| 指标 | 结果 |
| :--- | :--- |
| 诊断覆盖率（8 个定位必答题） | **37.5%**（能 1 / 部分 4 / 不能 3；严格可答率 12.5%） |
| 发现总数 | **27 条**：错 10 / 漏 6 / 优化 11（判据矩阵共 27 条，逐条映射见 §9.2） |
| 严重度分布 | **P0 2 · P1 16 · P2 9**（P0 判据见 §9.2：使任一必答题判「不能」且无替代证据） |
| 独立裁决 | verifier 59 条结论 **CONFIRMED 59 / REFUTED 0**；L3-A/L3-B 共 28 条 finding 全部裁决并入 |
| 外部基准复核 | 对照 DSH `dsh-v0.2.1-rc.2` 轨迹实现：27 条中 **✅18 印证 / 🟡7 需修正实现方式 / ⚪3 不可比 / ❌0 反驳**（见 §11） |
| 事故真因链可定位环数 | 5 环中仅 ⑤（终态失败）可指认；②③④（错误机制/重试耗尽/超时兜底）不可定位 |

**三条根因（按修复杠杆排序）**：

1. **跨项目契约供给不足**：R1 只禁 `error_message` 自由文本，**`error_type` 受控通道压根没接**（采集中件已拿到却硬编码丢弃），
   且从不上报 **LLM 调用级 span** 与 **turn 时间戳**；
   事故里 8 次 LLM 调用（4 次失败）在 trace 中完全不可见。消费者 `turns` 是不透明 JSON，**turn/tool 内补字段零改动即可承载**。
2. **消费者缺护栏**：`turns/tool_calls` 零 schema 校验（脏数据静默入库）；合并「重复轮次以旧为准」可钉住非终态；
   同 session 并发上报读-改-写无事务（实测 20 轮×2 并发 **18–19/20 丢轮**）；合并以新 payload 为基底导致
   **`extra` 在二次上报时被整体置 null**；控制台**不渲染 `turn.status`**，`error` 为空时错误区块整体隐藏。
3. **定位错配**：trace 目前是「**评测语料通道**」而非「故障诊断通道」——`eval_status` 六值全是评测语义，
   且生产者对 success 类 outcome 按采样率采样、失败恒导出，**语料天然偏失败**。要它承担排障，须先明确目标再补链条。

**最划算的一步**：5 个纯生产者、零 schema 变更、零裁决的字段补全（≈2–2.5 人日）可把覆盖率从 37.5% 提到 **~56%**；
再加 `llm_calls[]`（M）达 **~75%**，加时间戳 **~81%**，加节点词表 **~87.5%**（这是当前隐私不变量下的天花板，见 §4.2）。

---

## 1. 审核对象与方法

### 1.1 三层证据（全部可重放）

| 层 | 取法 | 收据 | 关键事实 |
| :--- | :--- | :--- | :--- |
| L1 库内真值 | `psql` → `traces` | `00-baseline/01-db-truth.txt` | 终态 37 条 trace；样本 `payload` **5821 bytes / 3405 chars**；`environment=development`、`release=dev` |
| L2 HTTP 读接口 | `GET /api/v1/traces/:id` | `00-baseline/02-api-get.json` | 返回体与库行逐键一致；**无服务端派生字段**（无 turn 状态汇总、无错误摘要） |
| L3 控制台 | agent-browser 截图 + `web/dist` 静态检查 | `40-verify/artifacts/console-trace-detail.png`、`console-index.html` | 列表无任何失败指示；抽屉里 `turn.status` **零渲染**（`grep -c "turn\.status"` = 0），`call.error` 为空时错误块被 `v-if` 隐藏 |

控制台实物（`console-trace-detail.png`）：抽屉显示 `Turn #1 · seq - · workbuddy/deepseek-v4.1-flash · 120.3s`、
输入 36612 / 输出 1368 / 缓存 23936，三个 tool 依次 `success (39.7s)` / `success (39.3s)` / `error (19.6s)`，
**没有 "failed" 字样、没有错误原因、没有重试与超时信息**，底部是原始 payload JSON。

### 1.2 被观测事故的真因链（只读引用，非本次要修的问题）

`/home/dm/job_copilot/.dev-team/evidence-cards-run/05-verdict.md`、`03-error-signature.txt`：

```
① jd_v6 文本链路切到 workbuddy/deepseek-v4.1-flash
② 结构化输出首个 chunk 非法 → SDK from_json("") → ValueError: expected value at line 1 column 1
③ format_jd_to_json 3 轮工具重试（每轮 2 次 LLM 调用）全部空结果 → 重试耗尽（MAX_RETRIES=1）
④ embedded_runtime 节点 120s 兜底 → TimeoutError（19:27:19.70 + 120s ≈ 19:29:19.86 实测）
⑤ SSE run_error → 前端 sse-event-error（19:29:30.58）
```

### 1.3 方法与分工

| 角色 | 职责 | 产物 |
| :--- | :--- | :--- |
| Lead | 基线收据、定点复核、DAG 编排、裁决与汇总 | `00-baseline/`、本报告、ADR |
| protocol-scribe | 27 条判据（W1-W8/M1-M8/I1-I11）+ 三列字段矩阵（39 字段行） | `10-contract/` |
| auditor-core | 消费者侧 18 条结论（W/M/I）+ R1–R15 收据 | `20-core/` |
| auditor-diagnostic | 诊断八问、生产者语义、跨项目建议 13 条 | `30-diagnostic/` |
| verifier | 独立复现 + 59 条裁决 + 新发现 G-01..G-08 | `40-verify/` |
| L3-A / L3-B | 契约完整性 / 安全运维 对抗审查（28 条 finding，全部裁决并入） | `60-review/` |

### 1.4 声明边界

- 脱敏**机制本身**（`[REDACTED_SECRET]`/`[USER_HOME]`）与 `session_id` HMAC 化是已知设计选择，不计为缺陷；
  但「**该挡没挡住**」的绕过面属缺陷（M-04），不因“设计行为”而豁免；
- 标注 `[NON-MACHINE]` 的结论靠代码阅读与 review，其余均有可重放命令；
- 成本（人日）为裁量估算，未经排期验证；覆盖率投影是单样本反事实，仅用于排序。

---

## 2. 样本 trace 事实卡

```jsonc
{ "session_id": "thread_ref_v1_e9b4ec395c96d8913c106ef8",   // HMAC(thread_id)，不可逆
  "run_id": "run_83c1879e338d442f953a5dd4b8d1646a",
  "turn_id": "turn_5dd7639f928549dfb86d96cbe6cbf33a",
  "thread_id": null,                                        // 键存在但值恒空
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
                  "error":null}                              // 无 output 键（不是 null）
               ] } ] }
```

六条直观观察：

1. **只有一个 turn 聚合**：事故中 8 次 LLM 调用 / 3 轮重试全部压平；
2. **没有任何错误类型/消息/堆栈**：3 个 tool_call 的 `error` 全为 `null`，含 `status=error` 的那条；
3. **状态语义自相矛盾**：2 条 `status=success` 的 `output` 正文是「职位解析失败」；第 3 条实际是 **cancelled**（被 120s 超时杀掉），在 trace 里被收敛成 `error`；
4. **时间只有一个瞬时值**（`reported_at_ms`），无起止、无每次调用耗时；
5. **没有用户输入**（`input_text` 缺失；该字段受隐私约束，是否补见 §4.2 与 L3-B 裁决）；
6. **`thread_id` 恒 `null`**、`session_id` 单向 HMAC，跨项目回链只能靠**明文 `run_id`** grep 日志。

---

## 3. 错（Wrong）—— 10 条

> trace 内记录的内容与事实不符，或语义自相矛盾。证据列给 `path:line` 或可重放收据。

### W-01 「重复轮次以旧为准」让终态与错误详情永久丢失 · **P1（潜在）** · 消费者
- **证据**：`crates/core/src/trace_merge.rs:3,69-83`（旧轮先入 `seen`，新轮只能追加新 key）、`crates/core/src/pg_repository.rs:629-632`；
  冻结该行为的测试是 `crates/core/tests/trace_repository_test.rs:323`（`test_trace_incremental_merge_upsert_dedup`）。
  实测：首报 `running` → 二报 `failed`（带 `error`）后，GET 仍是 `{"status":"running","tool_calls":[]}`（T2 R3 / T4 独立复现）。
- **为什么不是 P0**：当前生产者每 run 只在终态投递一次（`exporter.py` 单次投递、`client.py` 无重试），本次事故不存在
  `running→failed` 的二次上报；但对任何两段式/增量式生产者即刻升级为 P0。
- **因此无法回答（未来场景）**：这个 turn 最后成功还是失败、失败在第几次调用、错误是什么。
- **建议**：合并改为**状态单调推进 + 字段级深合并**（非终态→终态可覆盖；`error`/`tool_calls` 非空优先；保留 `attempts[]`）。成本 M；**契约影响：是**（破坏性，建议 `merge_semantics:v2` 灰度）。

### W-02 并发上报丢轮次（读-改-写无事务/无锁） · **P1（潜在）** · 消费者
- **证据**：`crates/core/src/pg_repository.rs:619-632`（SELECT → merge → ON CONFLICT 三步无事务）、`sqlite_repository.rs:656-732` 同构；
  实测：20 session × 2 并发（40 个 201）→ **18–19/20 只剩 1 个 turn**（比例随时序波动），不报错、不告警（T2 R11 / T4 §B5 / verifier 独立复现 18/20）。
- **为什么不是 P0**：生产者单次终态投递下需要「同会话并发上报」，本次事故未触发；但 SSE 重连/多副本场景可触发，且失败是**静默**的。
- **因此无法回答（触发时）**：这次 run 到底几轮、哪轮失败、总耗时/总 token 是多少。
- **建议**：**`SELECT … FOR UPDATE` 对首次上报无效**（行不存在 → 0 行无锁，L3-A F9），止血须用 `pg_advisory_xact_lock(hash(session_id))` 或单语句 upsert + `serializable` 失败重试；同时给锁预算 `lock_timeout`/`statement_timeout`、连接池上限、热点 session 串行化后的写放大（单次重写可达 MB 级 jsonb）；根治拆 `trace_turns(session_id,turn_id UNIQUE)` 行级 upsert（需行数估算、回填与 retention 方案）。无锁预算的止血本身会成为新故障源。成本 M；**契约影响：是**（存储形态，读接口形状兼容）。

### W-03 `total_*` 多套口径并存，可与 turns 永久矛盾 · **P1** · 消费者 + 生产者
- **证据**：`routes.rs:1230-1232`（首报原样存客户端值）、`trace_merge.rs:41-49`（双方有 turns 时求和覆盖）、`trace_merge.rs:52-55`（二报无 turns 时保留客户端值）；
  实测：`total_input_tokens=99999` 而 turns 求和 `10` 可同时成立（T2 R4、Lead P4）。
- **因此无法回答**：这次 run 烧了多少 token、哪一轮最贵、成本是否回归。
- **建议**：服务端**永远按 turns 求和**，缺 token 轮次置 `stats_incomplete`；客户端值仅作交叉校验（差异 >5% 记 `warnings`）。成本 S。

### W-04 `run_id/turn_id` 列与 payload turns 脱钩 · **P1** · 消费者
- **证据**：`migrations/0003_add_traces.sql:4`（`session_id UNIQUE` 单行/会话）、`pg_repository.rs:665-667`（冲突时列被最后一次上报覆盖）；
  实测：同 session 先报 `run_A/A1(failed)` 再报 `run_B/B1(succeeded)` → 1 行、列 `run_id=run_B`、`turns=[A1,B1]`（T2 R5）。
- **因此无法回答**：这条 trace 属于哪个 run / 哪个 turn（列=最近一次，payload=全部）；按 `run_id` 检索会漏掉历史失败 run（该不变式见
  `10-contract/01-field-matrix.md` 的 INV-2：列应能表达该行包含哪些 run/turn，而不是仅最后一次）。
- **建议**：`run_id/turn_id` 下沉到 turn 粒度（turn 自带 run_id），或在契约中明确「列=最近一次、检索走 payload」并同步 **INV-2** 修订。成本 M。

### W-05 `reported_at_ms` 无上下界；排序按 `created_at` 而界面显示 `reported_at` · **P2** · 消费者
- **证据**：`routes.rs:1191-1194`（无校验）→ 实测 `9999999999999` 入库为 `2286-11-20`（T2 R9）；排序 `pg_repository.rs:751` vs 显示 `App.vue:670`。
  全库 37 行最大偏差 `+16:30:05`（T4 复算）。
- **因此无法回答**：trace 的真实发生时间与先后顺序。
- **建议**：偏差 >24h 拒绝（422）或打 `clock_skew`；列表默认按 `reported_at` 并可切换。成本 S。

### W-06 `status=success` 而 `output` 是失败文案，被原样接收并渲染成"成功" · **P1** · 生产者主因 + 消费者未拦截
- **证据**：样本 `tool_calls[0..1]` `status=success` + `output="…职位解析失败…"`；消费者无一致性 lint（`routes.rs:1196-1236` 只脱敏）；
  控制台按 status 染色 `App.vue:1400-1406`（success→绿）。
- **因此无法回答**：3 次调用到底成功几次、重试是否耗尽——机器读法（2 成 1 败）与人工读法（3 次全败）相反。
  同源问题：tool status 只有三值，`cancelled` 被收敛成 `error`（判据 W7），「被超时杀掉」与「执行失败」不可区分。
- **建议**：①消费者一致性 lint（`status_conflict:true` + 黄标）；②生产者由工具执行结果派生 status 并补 `business_status`。成本 S。

### W-07 `POST /api/v1/traces` 返回「幻影 id」，二次上报后按响应 id `GET` 直接 404 · **P1** · 消费者（verifier G-01）
- **证据**：`pg_repository.rs:659-678`（`ON CONFLICT (session_id) DO UPDATE` 不更新 id）vs `:698-701`（返回本次新生成的 `Uuid::new_v4()`）。
  实测：`POST2` 响应 id `c8dcee71-…` → `GET` 404；库内真实 id `93139a38-…` → `GET` 200（`40-verify/02-independent-receipts.txt` §B3-E3；Lead 复现）。
- **因此无法回答/使用**：生产者若把 201 响应 id 当深链或回执（job_copilot `client.py` 把 201 当创建成功），二次上报后链接永久失效。
- **建议**：`DO UPDATE … RETURNING id` 回读真实 id，或冲突时把响应 id 替换为库内 id。成本 S。

### W-08 POST 响应体 `total_*` 与库内落盘值不一致（同一请求两个真值） · **P1** · 消费者（verifier G-02）
- **证据**：入库用 `effective_*`（`pg_repository.rs:636-657` 按 merge 后 payload 重算），返回却用未重算的 `trace.total_*`（`:698`）。
  实测：同 turn 二报响应 `total_in=999/dur=500`，库内 `100/120000`；追加轮响应 `777777`，库内 `8`（§B3-E3/E6）。
- **因此无法回答**：生产者按响应自检会误判「服务端认可了我的 total」，而库内已改。
- **建议**：返回前用 `effective_*` 回填，或让响应携带 `persisted_total_*`。成本 S。

### W-09 二次上报把 `extra` 整体置 `null`（跨轮字段被清空） · **P1** · 消费者（L3-B F4，Lead 复现）
- **证据**：`trace_merge.rs:13` 以 `new.clone()` 为基底，只显式合并 `turns/total_*`；`AgentTraceIngestRequest.extra` 缺省序列化为 `null`（`routes.rs:352-368`）。
  实测：POST1 写 `extra={request_id,failover_policy}` → POST2 同 session 新 turn（不带 extra）→ `payload.extra=null`，turns 两条仍在（Lead 复现；L3-B E1）。
- **因此无法回答**：任何放在 `extra` 的跨轮诊断字段（版本、采样率、请求关联）会在第二次上报后消失。
- **建议**：合并时对 `extra/tags` 做**字段级深合并**（非空优先），或规定生产者每次必发；在修好之前，**诊断字段一律放进 `turns[]`**。成本 S。

### W-10 列与 payload 双写不一致（`session_id` 兜底路径、`eval_status` 两处真值） · **P2** · 消费者
- **证据**：`routes.rs:1202-1217`（缺 `session_id` 时回退 `thread_id` 作为会话键）→ 实测（L3-B E3）`session_id=thread_id` 的请求使
  列与 payload 的会话键同值但语义不同（后续 `thread_id` 再上报会命中同一行）；`routes.rs:1231` 列 `eval_status` 缺省写 `unreviewed`，
  而 `payload.eval_status` 序列化为 `null`（样本实测 `eval_status` 列=`unreviewed`、`payload.eval_status`=`null`）。
- **因此无法回答**：同一行里的「会话标识」与「评估状态」到底以哪份为准；审计读列还是读 payload 会得到不同结论。
- **建议**：`session_id` 兜底时同时记录 `session_id_source:"thread_id"`；`eval_status` 以列为唯一真源并从 payload 删除（或反序列化时回填）。成本 S。


---

## 4. 漏（Missing）—— 诊断充分性评估 + 6 条结构性缺失

### 4.1 八个定位必答题（口径：能=1 / 部分=0.5 / 不能=0）

| # | 定位必答问题 | 判定 | trace 中有的 | trace 中缺的 |
| :-- | :--- | :--- | :--- | :--- |
| D1 | 是否失败？哪个项目/环境/版本/模型？ | **能** 1.0 | `turns[0].status=failed`、project/env/release/model | — |
| D2 | 失败在哪一层/哪个节点？ | **不能** 0 | `step="other"`、3 条 tool_calls | 无 graph node、无 span 层级、无 `llm_calls[]`、无 `error.kind` |
| D3 | 失败的机制与错误签名？ | **不能** 0 | 无 | `error_type`/`error_message`/`stack`；R1 明确不外发错误文本 |
| D4 | 哪个工具出问题？业务成功还是失败？为什么？ | **部分** 0.5 | tool_name、duration、arguments、失败文案 output | 无业务态字段；第 3 条 `error=null` 且无 `output`；无重试轮次语义 |
| D5 | 是否重试？几轮？为何耗尽？ | **不能** 0 | 仅 `attempt_index=1`（运行级） | 无 tool/LLM 级 attempt、无预算/上限、无每次起止时刻 |
| D6 | 是否触发超时兜底？何时开始/被杀？ | **部分** 0.5 | `duration_ms=120272`（≈120s，强提示） | 无 `started_at/completed_at`、无 `error_type=TimeoutError`、无事件序列 |
| D7 | LLM 侧发生什么（几次/模型/耗时/状态/token）？ | **部分** 0.5 | 仅 turn 级 token 合计与 model | 无逐次调用、无失败计数、无每次耗时（实测 8 次、4 次 error 全不可见） |
| D8 | 能否跨项目回链（request_id/用户/会话/日志）？ | **部分** 0.5 | **明文 `run_id`**（可直接 grep 生产者日志） | `request_id` 被 HMAC alias 化（不可 grep）、`user_id` 未外发、`thread_id=null`、`session_id` 不可逆 |

**加权覆盖率 = 3.0/8 = 37.5%；严格可答率 = 1/8 = 12.5%。**
真因链 5 环中仅 ⑤ 可指认；②③④（错误机制 / 重试 / 超时）落在「不能」或「靠 120s 数字猜」。

### 4.2 反事实增益（每补一组生产者字段能救回多少覆盖率；单样本投影，仅用于排序）

| 补丁（全在生产侧） | 覆盖率 | 主要救回 |
| :--- | :--- | :--- |
| 基线 | 37.5% | D1 |
| + `error_type`（受控枚举，turn/tool 级） | **56.3%** | D4 满分、D3 升部分（**类型可答、消息/堆栈仍不可答**）、D2 升部分 |
| + `llm_calls[]` | **75.0%** | D5 满分、D7 满分 |
| + turn/tool 起止时间戳 | **81.3%** | D6 满分 |
| +（改节点词表）真实 `step` | **87.5%** | D2 满分 |
| 跨项目回链满分 | 不可达（当前隐私不变量下） | D8 需明文关联字段，与 R1/INV-2 冲突，须先做隐私裁决 |

> 天花板说明：`error_message` 被 R1 与 `sanitize_error_message`（恒 `[REDACTED]`）双重禁止，故 D3 永远只能到「部分」；
> `request_id` 被 HMAC alias 化（`trace_safety.py:507-508`），无法用于 grep 日志。此前 T3 的 93.8% 投影基于
> 「request_id 可回链」，经 L3-B F7 裁决**修正为不可达**。
> 关键杠杆仍成立：消费者 `turns` 是不透明 `Vec<Value>`（`routes.rs:362`），**turn/tool 内新增任意字段零改动即可入库并原样返回**。

### 4.3 结构性缺失

### M-01 `status=error` 且 `error` 为空被原样接受，"没有错误"与"没上报"不可区分 · **P0** · 双方
- **证据**：样本 `tool_calls[2]` 只有 `{status:"error","error":null}`，**无 `output` 键**；`routes.rs:362`+`:1196-1236` 无校验直接落库（T2 R2：`status=error` 无 `error` 仍 201）；
  控制台 `App.vue:875-878` `v-if="call.error"` → 空则整块不渲染。
- **因此无法回答**：第 3 次失败的直接原因——本次事故的核心诊断缺口（唯一 P0）。
- **建议**：①契约要求 `status∈{error,failed}` 时至少含 **`error_type`（受控枚举）**；**不要求也不允许 `error_message`**（与 R1 一致）；
  缺失落 `error_contract:"missing"` + 红标 + 可筛；②空 `error` 时前端显式渲染「未上报」。成本 S。

### M-02 无 LLM 调用级 span、无重试/超时语义的位置 · **P0** · 生产者为主
- **证据**：`routes.rs:351-369`（只 `turns: Vec<Value>`，无 spans 约定）；trace 路径不引用 `crates/ingest/src/validate.rs`。
  job_copilot 本地同一 run 有 **8 条 LLM span / 4 条 error**，一条都没进 trace（`30-diagnostic/02-producer-semantics.md` §S3）。
- **因此无法回答**：`ValueError: expected value at line 1 column 1` 来自哪次调用、原始响应是什么、8 次调用各耗时多少、120s 超时卡在哪个节点。
- **建议**：可选 `turns[].llm_calls[]` 与 `turns[].attempts[]`；消费者零 schema 改动即可承载。成本 M（主体在生产侧）。

### M-03 失败 trace 无处置通道：note 被丢弃、无 issue/告警/指标 · **P1** · 消费者
- **证据**：`UpdateTraceRequest.note`（`routes.rs:383-386`）落库 SQL 只写 `eval_status/updated_at`（`pg_repository.rs:771-791`，T2 R8 note 0 命中）；
  trace 路径无指纹/issue/webhook（对比 events `routes.rs:892,911,965`）；`/metrics` 无 trace 指标（`routes.rs:735-780`）。
- **因此无法回答**：这次失败有没有人处理、结论是什么、同类失败最近又犯了没。
- **建议**（按 L3-B F6 修正顺序）：**先做 metrics + 待评审队列 + 日报摘要**；`note` 落库并回显；
  **webhook/issue 延后**到 W-01/W-02 修复且指纹稳定后，并复用 events 侧「仅 new/regression 通知」的去重思路，否则会告警风暴。成本 M。

### M-04 脱敏在 turn/tool 文本层存在绕过面（该挡没挡住） · **P1** · 双方（verifier G-04、Lead P5）
- **证据**：同一请求内五种写法实测（`40-verify/02-independent-receipts.txt` §A8b；Lead P5）：
  `{"password": …}` ✅、`"password": "…"` ✅、**`'password': 'hunter2'`（Python repr 单引号）原样入库 ❌**、
  **裸 `token=abc123def` 原样入库 ❌**、`auth_token=…` ✅。
  根因：`crates/ingest/src/sanitizer.rs:30-41` 的 `SECRET_PATTERNS` 只容双引号、不含裸 `token=`。
- **因此无法回答/风险**：本次事故的 `arguments_summary`/`output` **正是 Python repr**——最容易泄露的文本恰好是绕过的那一类；`error` 字段（`payload.py:296`）连 `redact_text` 都不做。
- **建议**：正则支持单引号/JSON5 引号；裸 `token` 入词表但保留 `*tokens` 白名单；生产者对 `error` 加 `redact_text`；补一组对抗用例固定这五种形态。成本 S。

### M-05 trace 采集的采样不可自描述，语料天然偏失败且不可辨识 · **P2** · 生产者 + 消费者（L3-A F10，经 DSH 基准修正）
- **证据**：生产者 `exporter.py` 对 error/failed/cancelled/timed_out **恒导出**，success 按 `pony_sentry_trace_sample_rate` 采样（默认按环境配置）；
  trace payload 不含 `sampled`/`sample_rate`，消费者无法区分「没发生」与「未采样」（本地存储有 `sampled` 字段但未外发）。
  DSH 基准对照：其 ledger 通道**完全不采样**（全仓 `sample` 0 命中），靠有界队列 + 去重键承担完整性代价——
- **因此无法回答**：某类成功路径的失败率/耗时分布，以及「样本偏倚」对评测结论的影响。
- **建议**：二选一——**要么**像 DSH 一样不采样、全量入账（成本取决于容量治理）；**要么**任何采样都必须自描述：
  把 `sampled`/`sample_rate` 放进 `turns[]`（非 `extra`，见 W-09）并在列表标注。成本 S。

### M-06 输入上下文缺失（`input_text`/`output_text` 未外发，且补前须隐私裁决） · **P2** · 生产者（隐私约束）
- **证据**：样本 turn 无 `input_text`/`output_text`；生产者 payload 模板支持这两个键（`payload.py:244-252`）但 run 终态 sink 未传 `input_text`；
  控制台 `App.vue:814-846` 有渲染分支，永远为空。
- **因此无法回答**：这次失败是由哪条用户输入触发的（复现所需的最小上下文）。
- **建议**：**先做隐私裁决再补**——该字段会携带 JD/简历正文（上限 10k/20k 字符），与 `sanitize_error_message` 恒 `[REDACTED]` 的取向冲突；
  若补，建议只外发**长度 + 摘要哈希 + 首 200 字符经 redact_text**，绝不整段落库（L3-B F1）。


---

## 5. 优化（Improve）—— 11 条

| ID | 问题 | 证据 | 建议 | 成本 | 级别 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| I-01 | 未知**顶层**字段被静默丢弃（typed struct 反序列化后重新序列化；无 flatten/deny_unknown） | `routes.rs:351-369,1196`；T2 R2：6 个未知顶层键 201 后全无 | ①`#[serde(flatten)]` 保留到 `payload._unknown` 并计数告警；或②`deny_unknown_fields`+400 | S | P1 |
| I-02 | `turns/tool_calls` 零 schema 校验（status 枚举、必填、类型都不校验） | `routes.rs:362`；T2 R2：`"not-an-object"`、`123`、非法 status 均入库 | 强类型 + 非法值降级 + `warnings[]` 回写 | M | P1 |
| I-03 | List 无 `run_id`/`turn_id`/时间窗/失败筛选，未知参数静默忽略 | `routes.rs:371-380`；T2 R6：9 种参数全 200 且结果集不变 | 先加 `run_id` 过滤（`payload #>` + GIN），再抽 `turn_status` 列与 `from/to` | M | P1 |
| I-04 | List 无 limit clamp、无 payload 投影、无总数/游标；**列表体积与实际渲染字段严重不匹配**；**无稳定去重键** | `pg_repository.rs:753-755`；`?limit=100000` → 2,016,030 B；单行 avg≈49KB / max≈325KB，前端只用约 100 字节 | `clamp(1,100)` + `?fields=summary` 投影 + `{items,total,next_offset}`；**引入稳定去重键 `(session_id,turn_id,report_seq)`**（对标 DSH 的 `(session.id,format_version,seq)`）；**行级字节上限 + 超限截断并落 `truncated`** | S–M | P1 |
| I-05 | 控制台不渲染 `turn.status`、无「失败原因」区、空 error 整块隐藏、`degraded/attempt_index` 不可见 | `App.vue:653-667,764-775,875-878`；`grep -c "turn\.status"`=0；截图 `console-trace-detail.png` | 列表加状态汇总徽标；抽屉顶部「失败原因」区（空则显式「未上报」）；展示 attempt/degraded | S | P1 |
| I-06 | 缺 trace 观测面：无 trace 指标、无字段/行级上限、retention 全局固定 90d | `routes.rs:735-780`、`main.rs:46`、`routes.rs:422`；DSH 对照：有界队列 2048/4MB/导出超时 30s，但同样无丢弃计数 | `pony_sentry_traces_total`、`trace_merge_lost_turns_total`、`payload_bytes` 直方图；行级字节上限 + 截断标记；丢弃/合并丢失计数 | S–M | P2 |
| I-07 | 脱敏对 trace 证物无差别套 events 规则（路径就地改写、无 redaction 计数） | `sanitizer.rs:116-176`；T2 R10：`/home/dm/…/embedded_runtime.py` → `[USER_HOME]/…` | **不要取消自由文本密钥扫描**（L3-B F5）；改法：trace 保留仓库相对路径（需消费者二次脱敏同步放行，否则会被改回）、redaction 计数只进**聚合指标**而非 payload（顶层键会被 I-01 丢弃、`extra` 会被 W-09 清空） | S | P2 |
| I-08 | trace ingest 契约无文档之家（README 只有鉴权表，无字段契约） | `README.md:131-133`；`docs/decisions/`、`docs/INDEX.md` 无 trace 条目 | 新增 trace 契约页（字段矩阵 + 校验 + 合并 + 口径），same-commit 维护 | S | P2 |
| I-09 | 死契约字段组：`stats_incomplete` 徽标、`turn.phase`、`turn.sequence`、`turn.node` 均「控制台渲染、服务端/生产者从不产出」 | `App.vue:702,766-768,1344`；Rust 侧 grep 0 命中；样本 turn 仅有 `step` | 与 W-03「服务端求和」配套实现 `stats_incomplete`；`phase/sequence/node` 要么接生产者，要么按「删除前先搜消费者」清理渲染分支 | S | P2 |
| I-10 | 非法 `limit/offset` → HTTP 500 且回显数据库错误（信息泄露 + 把参数错误伪装成 5xx，污染错误预算与告警；`format!` 拼接 LIMIT/OFFSET） | `pg_repository.rs:753-755`、`routes.rs:1276-1278`（`e.to_string()`）；实测 `limit=-5&offset=-3` → 500 `OFFSET must not be negative`（T4 §B4、Lead P6） | `clamp(1,100)` + `offset.max(0)`；负值→400；改参数绑定；5xx 响应不回显 DB 文案 | S | P1 |
| I-11 | retention 按 `created_at` 删除，活跃会话在第 90 天被整行删除（更新不前移删除线） | `pg_repository.rs:815`（`DELETE … WHERE created_at < $1`）；`ON CONFLICT`（`:665-677`）不更新 `created_at` | 按 `updated_at` 或 `max(turn 时间)` 续期；长会话按行/项目设上限 | S | P2 |

> 容量补充（L3-B F3）：**2MB 只是单次 POST body 上限**（`routes.rs:422`），不是行级/响应级上限。
> 实测同 session 两次各 1.2MB turn 均 201，合并后单行 `payload` ≈2.4MB，`GET /traces/:id` 返回 2.4MB，
> `GET /traces?limit=100000` 返回 4.4MB。合并行的无界增长是独立风险，应纳入 I-04/I-06 一并治理。

---

## 6. 改进路线图（建议，本次不实施）

| 波次 | 内容 | 责任方 | 成本 | 收益 |
| :--- | :--- | :--- | :--- | :--- |
| **Wave 0**（立即，消费者 S 级，满足「P0 立即」） | M-01 空错误检测与红标/可筛、W-07 id 回读、W-08 响应 total 一致、I-10 参数校验与 5xx 文案回退 | 消费者 | ≈1 人日 | 本次事故「失败看起来正常」的最小止血；不改 schema、不动合并语义 |
| **Wave A**（零裁决，紧随） | `turns[]` 内补：`error_type`（受控枚举，turn/tool）、`started_at/completed_at`、`calls_total/calls_failed`、`token_scope`、`output` 受控抽取、采样元数据 | 生产者 | ≈2–2.5 人日 | 覆盖率 37.5% → **~56%**；消费者零改动（turn 内新增，不受 I-01/W-09 影响） |
| **Wave B**（结构性止血） | W-01 合并语义 v2、W-02 advisory lock（或 `trace_turns`）、W-09 `extra/tags` 深合并、W-10 双写口径 | 消费者 | ≈3 人日 | 消除「终态丢失 / 并发丢轮 / extra 清空 / 双写矛盾」 |
| **Wave C**（可诊断体验） | `llm_calls[]`（生产者）、控制台失败原因区与 LLM 调用折叠、List `run_id`/状态/时间窗 + 投影 + clamp | 双方 | ≈3–4 人日 | 覆盖率 → **~75–81%**；一键回链与检索 |
| **Wave D**（择机） | trace → metrics/待评审队列（webhook 延后）、note 落库、脱敏对抗用例、retention 续期、契约文档化 | 消费者 | ≈1.5–2 人日 | 从「能看」到「有人管」 |

**跨波次前置约束**：
- **DB schema 变更需显式授权**：`trace_turns` 新表、`turn_status` 新列、`run_id` 索引均越出本审核 brief 的
  「不改数据库 schema」非目标，属 Wave B/C 的待裁决项；未经授权时改用 advisory lock + `payload #>`/GIN 查询（零 schema 变更）。
- 任何改 `App.vue` 或控制台读路径的波次，必须重走 `.dev-team/contract-console-timeout.md` 的冻结流程并复核
  `API_TIMEOUT_MS > acquire_timeout + 2000` 不变式（R1–R8）。该契约 R6 的「最小闭包」是 wave-6 那次变更的**当时**约束，
  不是对 trace 功能的长期禁令；但 R8 的 acquire_timeout 漂移哨兵与读超时预算**持续有效**。
- 隐私边界：**不新增** `error_message`/traceback/`user_id`/明文 `thread_id`；`error_type` 只走受控词表；
  `request_id` 已被 HMAC alias 化，不能用于日志 grep，回链仍依赖明文 `run_id`（若需更强关联，先做隐私裁决）。

**不建议做**：外发 `error_message` 原文/traceback（违反 R1 且被脱敏清零）；把 120s 等常量写进 trace；
消费者反解 `thread_ref_v1_*`（HMAC 单向）；绕过 `build_trace_payload` 直接 POST 自有 JSON（跳过全部脱敏与钳制）。

---

## 7. 跨项目契约提案（字段级）

> 全部落在 `turns[]` / `tool_calls[]` 内，**不新增顶层键、不改 DB schema、不改合并语义**；
> 刻意**不用 `extra`**（W-09：二次上报会被置 null），也**不放 `user_id`/明文 `thread_id`**（隐私）。

```jsonc
{ "turns": [{
    "status": "failed",
    "error_type": "TimeoutError",                  // 受控词表 _SAFE_ERROR_TYPES，禁自由文本
    "started_at_ms": 1792754879720, "completed_at_ms": 1792754960002,
    "calls_total": 8, "calls_failed": 4, "token_scope": "billing_success_only",
    "sampled": true, "sample_rate": 1.0,           // 采样元数据（M-05）
    "llm_calls": [{ "model": "workbuddy/deepseek-v4.1-flash", "provider": "ponyllm",
                    "status": "error", "duration_ms": 13240, "attempt_index": 1,
                    "error_type": "ValueError", "started_at_ms": 1792754878822 }],
    "tool_calls": [{ "tool_name": "format_jd_to_json", "status": "cancelled",
                     "business_status": "retry_exhausted", "cancel_trigger": "node_timeout",
                     "error_type": "CancelledError", "attempt_round": 3 }]
  }] }
```

字段约束（经 DSH 基准复核修正）：`error_type` 采**稳定字符串码**——已知码保留原值、未知码原样透传并打 `unknown_code:true`，
**禁止**收敛为 `other/Exception`（DSH 事故复盘把「用最近通用类覆盖下层结构化失败」列为归因反模式，
`docs/postmortem/0004-landlock-partial-notice-misclassified-child-failures.md:52-54`）；同时保留 `error.name`；`*_at_ms` UTC 毫秒且单调；
`llm_calls ≤ 50` 条、单条 ≤512B；`attempt_round ∈ [1,10]`，与运行级 `attempt_index` 并列声明语义；
**新增无界维度必须同时给上限**：`turns ≤ 64`、单 turn ≤256KB、单 payload 合并后 ≤2MB，超限截断并打 `truncated:true`
（当前 `turns[]` 无条数上限、行体积无上界）。
注意：**tool 侧 `error_type` 已在中件里拿到、只是被硬编码丢弃**（`tool_observability_middleware.py:259,278-279`），
而 **LLM 侧本地 span 的 `error_type` 目前也是 NULL**——需先接采集点，再谈外发。

---

## 8. 定位判断与残余风险

1. **定位判断（最重要）**：现有 trace 的实际定位是**评测语料**——`EvalStatus` 六值（`triage_good/triage_bad/eval_dataset/
   optimized/wontfix`）全是评测语义，字段契约按此裁剪（R1 不外发错误文本、每 run 一条终态摘要、无 span），
   且 success 类 outcome 按采样率采样（M-05），语料本就偏失败。要让 trace 承担排障，需先明确目标再按 Wave A→D 补齐。
2. **隐私与体积**：`error_type` 走受控词表；不引入 `user_id`/明文 `thread_id`（消费者 `AgentTraceIngestRequest.thread_id`
   是普通 String，无 HMAC 兜底）；`llm_calls` 控量（8 次 ≈1.5–2KB）；单行体积无上界需 treat as 独立风险。
3. **兼容风险**：W-01/W-03 属破坏性语义变更，需生产者同步与灰度标记；I-01 由「静默丢」变「保留/拒绝」需双方对齐；
   W-04 若要改列语义，必须同步修订 INV-2 并说明 `run_id` 的历史检索缺口。
4. **告警风险**：trace→issue 必须复用 events 侧「仅 new/regression 通知」的去重思路，否则每次失败都建 issue 会造成告警风暴；
   在 W-01/W-02 修复前不应上线 webhook（失败 turn 可能压根没入库）。
5. **无法机器验证项**：`[NON-MACHINE]` 合并语义迁移成本、成本估算（人日）、D1/D6 评分口径属裁量；
   `thread_ref` HMAC 等式未复算（缺 key）；SQLite 后端并发行为仅做代码对照未实测。

---

## 9. 验证与审查记录

| 项 | 结果 | 收据 |
| :--- | :--- | :--- |
| verifier 独立裁决 | **59 CONFIRMED / 0 REFUTED / 0 UNVERIFIABLE** | `40-verify/01-verdicts.json`（`jq -e` Exit 0） |
| 独立复现 W-01 / W-02 | 复现（18–19/20 丢轮） | `40-verify/02-independent-receipts.txt` §B3/§B5 |
| 独立新发现 | G-01 幻影 id、G-02 响应 total 不一致、G-03 负数分页 500、G-04 脱敏绕过 → 并入 W-07/W-08/I-10/M-04 | `40-verify/03-gap-report.md` §A |
| L3-A 契约完整性审查 | FAIL（14 条，MAJOR 10 / MINOR 4）→ **全部裁决**：F1/F2/F3/F4/F5/F14 已在 v2 并入（验证回填、字节口径、幻影 id、响应分叉、脱敏绕过、测试锚点）；F6 严重度重排说明见 §9.2；F7 路线图新增 Wave 0；F8 判据映射见 §9.2；F9 改写 W-02 止血方案；F10 见 §6 前置约束；F11 并入 I-09；F12 见 M-01（只要求 `error_type`）；F13 修正根因表述 | `60-review/review-l3a.json`、`review-l3a.md` |
| L3-B 安全运维审查 | FAIL（14 条：BLOCKER 3 / MAJOR 8 / MINOR 3）→ **全部裁决**：F1 撤 `user_id`、F2 只要求 `error_type`、F3 补 2MB 口径、F4 新增 W-09、F5 改写 I-07、F6 告警降级为 metrics 先行、F7 D8 修正、F8 新增 I-11、F9 补 INV-2、F10 升 M-04 为独立缺陷、F11 I-10 升 P1、F12 吸收 G-01..G-04（G-06/G-08 为口径与覆盖提示，已并入 §1.1/§5）、F13 W-02 补锁预算、F14 §7 补无界维度上限 | `60-review/review-l3b.json`、`review-l3b.md` |
| 终审关闭裁决 | **PASS**（28 条 prior finding：CLOSED 26 / PARTIAL 2 / OPEN 0；两项 PARTIAL 已在 v3 关闭：§9.2 增 P0 阈值并把 M-02 升 P0、附录 A 归档 39 行矩阵） | `60-review/adjudication.json`、`adjudication.md` |
| 实验卫生 | 全部实验行（`audit-probe-*`）已删除，终态 `traces=37`、`audit-probe%=0`、事故行 md5 前后一致 | `40-verify/02-independent-receipts.txt` §B3-clean、`60-review/l3b-receipts.txt` |

### 9.1 L3 裁决摘要（Lead）

- **L3-A（契约完整性，14 条）全部接受**：F1/F2（验证回填、字节口径）、F3/F4（幻影 id、响应分叉 → W-07/W-08）、
  F5（脱敏绕过 → M-04）、F6（严重度重排 → §9.2 增可复核阈值）、F7（P0 立即 → 新增 Wave 0）、F8（27 判据映射 + 附录 A）、
  F9（`FOR UPDATE` 首插无效 → 改 advisory lock）、F10（schema/console-timeout 双前置）、F11（死契约扩展 → I-09）、
  F12（M-01 只要求 `error_type`）、F13（根因表述纠正）、F14（锚点与 `output` 键缺失）。
- **L3-B（安全/隐私/容量，14 条）全部接受**：F1/F2（撤 `user_id`、只留 `error_type`）、F3（2MB 只是单 POST body）、
  F4（`extra` 置 null → W-09）、F5（I-07 不得取消自由文本扫描）、F6（告警降级为 metrics 先行）、F7（`request_id` 为 alias，回链只靠明文 `run_id`）、
  F8（retention 按 `created_at` → I-11）、F9（W-04 与 INV-2）、F10/F11（M-04、I-10 升 P1）、F12（吸收 G-01..G-04）、
  F13（锁预算与写放大）、F14（无界维度上限）。
- **不接受**：L3-A 曾把 `contract-console-timeout.md` R6 解读为长期禁止改 `App.vue`——R6 的「最小闭包」是 wave-6 当时的范围约束，
  已在 §6 改为「重走冻结流程 + 复核超时预算」的持续约束。
- **未采纳的建议**：L3B-F1 要求的字段级分类/读侧权限矩阵已记录为 Wave D 前置，但超出本次「只审核」范围。

### 9.2 判据 → finding 映射与严重度裁决

**P0 的可复核阈值**：某条 finding 若使 §4.1 任一必答题判「不能」，且不存在可替代的现存证据通道，则记 P0；
否则按「显著削弱（P1）/ 效率与体验（P2）」归类。据此：M-01（D3 不能）、M-02（D2/D5/D7 不能）为 P0；
W-01/W-02 因当前生产者单次终态投递、本次事故链未触发而记 P1（潜在）。

判据矩阵（`10-contract/02-audit-criteria.md`，27 条）的 P0 集是**finding 落盘前的预判**（W1,W7,M1,M2,M4,I1,I3,I4）；
最终分级按上述阈值重排，变化如下，均在此声明依据：

| 判据 | 判据预判 | 最终 | 对应 finding | 裁决依据 |
| :--- | :--- | :--- | :--- | :--- |
| W1 | P0 | P1（W-06） | W-06 | 本次能看出「工具被调用 3 次」，只是业务成败不可读 → 不阻断诊断 |
| W2 | — | P1（W-03） | W-03 | token 口径矛盾不阻断「为什么失败」 |
| W3 | — | P1（W-04） | W-04 | 列/轮次脱钩影响检索，不影响本次判读 |
| W4 | — | **P2（W-10）** | W-10 | 新增：`session_id` 兜底与 `eval_status` 双写 |
| W5 | — | **P2（W-10）** | W-10 | 并入 W-10 |
| W6 | — | 已并入 D6/M-02 | D6、M-02 | 绝对时间戳缺失属 LLM/时序缺口的一部分 |
| W7 | P0 | P1（W-06 附） | W-06 | status 三值坍缩，单独不阻断；与 W-06 同源 |
| W8 | — | P0（M-01） | M-01 | 键缺失/显式 null 不可区分，正是本次核心缺口 |
| M1 | P0 | **P0（M-01）** | M-01 | 唯一直接阻断本次诊断的条目 |
| M2 | P0 | **P0（M-02）** | M-02 | 缺 LLM span 使 D2/D5/D7 判「不能」，无替代通道 → 维持 P0 |
| M3/M4 | P0 | P1（M-02 附） | M-02 | 重试/超时语义属同一缺口的子项 |
| M5 | — | P1（M-02 附） | M-02 | 父子层级缺失并入 M-02 |
| M6 | — | **P2（M-06）** | M-06 | 新增：输入上下文缺失（需隐私裁决） |
| M7 | — | 已并入 D8 | D8 | 关联键缺失在 D8 量化 |
| M8 | — | 已并入 M-05/M-03 | M-05、M-03 | 采样不可辨识、丢弃无计数 |
| I1 | P0 | P1（I-03/I-04） | I-03、I-04 | 检索能力不足抑制排查效率，非阻断 |
| I2 | P1 | P1（I-01） | I-01 | 一致 |
| I3 | P0 | P1（I-02） | I-02 | 脏数据静默入库，但本次数据形状合法 → 潜在 |
| I4 | P0 | P1（W-01） | W-01 | 当前生产者单次终态投递未触发 → 潜在 |
| I5–I11 | P1/P2 | 见 §3–§5 | I-05..I-11、M-03 | 按上述 P0 阈值逐条重排 |
| （无对应判据） | — | P1（W-09） | W-09 | 审查新增：二次上报把 `extra` 整体置 null |
| W4/W5 | — | P2（W-10） | W-10 | 审查新增：`session_id` 兜底与 `eval_status` 双写 |

> 逐字段对照矩阵（39 字段行）已作为**附录 A** 归档于本报告同目录 `appendix-field-matrix.md`（与 `.dev-team/audit-trace-cards/10-contract/01-field-matrix.md` 同源，brief §69 的“入报告”要求由此落地）；
> 本报告 §2/§3/§4 只引用与结论直接相关的字段行。

---

## 10. 附录：收据索引与复现入口

| 收据 | 文件 |
| :--- | :--- |
| 库内真值 / payload 全集 | `.dev-team/audit-trace-cards/00-baseline/01-db-truth.txt`、`03-trace-payload.raw.json` |
| HTTP 读接口返回 | `00-baseline/02-api-get.json` |
| Lead 定点复核（P1–P6） | `00-baseline/05-lead-probes.md` |
| 字段矩阵 / 判据 / 探针 | `10-contract/01-field-matrix.md`、`02-audit-criteria.md`、`03-probe-receipts.md` |
| 消费者侧结论与 R1–R15 | `20-core/01-findings.md`、`02-receipts.txt` |
| 诊断八问 / 生产者语义 / 跨项目建议 | `30-diagnostic/01..04` |
| 独立验证与裁决 | `40-verify/01-verdicts.json`、`02-independent-receipts.txt`、`03-gap-report.md` |
| 控制台实物截图 | `40-verify/artifacts/console-trace-detail.png` |
| 对抗审查 | `60-review/review-l3a.json`、`review-l3b.json`、`adjudication.json`、`l3b-receipts.txt`、`receipts-l3a.txt` |
| 附录 A：39 行字段矩阵 | `docs/release/trace-audit-cards-2026-10-23/appendix-field-matrix.md` |
| 附录 B：DSH 外部基准复核 | `docs/release/trace-audit-cards-2026-10-23/dsh-benchmark-review.md` |

复现入口：

```bash
cd /home/dm/pony-sentry/.dev-team/audit-trace-cards/40-verify
bash run-independent-receipts.sh   > /tmp/a.txt
bash run-independent-receipts-2.sh >> /tmp/a.txt
bash run-independent-receipts-3.sh >> /tmp/a.txt
bash run-verify-t2-findings.sh     >> /tmp/a.txt
jq -e 'type=="array" and all(.[]; .verdict|test("^(CONFIRMED|REFUTED|UNVERIFIABLE)$"))' 01-verdicts.json
```

---

## 11. 外部基准复核（DSH 轨迹实现）

完整对照见同目录 [`dsh-benchmark-review.md`](dsh-benchmark-review.md)（基准：`/home/dm/deepseek-harness @ dsh-v0.2.1-rc.2-1-g5ad609c9ea`）。

**DSH 的轨迹模型**：会话 = append-only 事件账本（62 种事件，逐事件 `seq`+`time`，消息历史由日志派生）；
`turn/end.reason` 为受控判别式（`completed/aborted(reason)/blocked/error/max-tokens/interrupted/forked`）；
`tool/result.error = {name,code,reason?}`；失败/重试/取消的模型尝试单独记 `assistant/attempt`/`llm/retry`；
错误码为**开集字符串**（`HarnessError.code`，路由只看 code）；脱敏只改**导出副本**、canonical 本地日志永不重写、
监听器抛错即 fail-closed；**无采样**，靠有界队列（2048 条/4MB 单请求/导出超时 30s）+ 接收端按 `(session.id,format_version,seq)` 去重。

**两个补充事实**：① DSH **没有 trace/span**，LLM 调用级视图由 `step/start`↔`assistant/message` 的 `time` 差 + 流内 `time0/dt` **派生**（TTFT/decode/TPS），不落额外 span；
② 其 UI 对失败与缺失都**显式呈现**（`turn-error` 行显示 message+code；缺时序时渲染「not recorded / first token unavailable」），直接印证 I-05/M-01 的展示主张。

**验证结论**：27 条 findings 中 **✅18 印证 / 🟡7 方向成立但实现方式需修正 / ⚪3 不可比（DSH 无 REST 上报）/ ❌0 被反驳**。
外部基准没有推翻任何结论，反而印证了核心判断：**把轨迹当「可合并聚合行」而非「逐事件账本」，是「看得出失败、看不出原因」的结构性根源。**

**据此回改的四项**：
1. `error_type` 由「闭集枚举收敛」改为「**稳定码 + 保留未知码**」——DSH 把「用最近通用类覆盖下层结构化失败」列为归因反模式；
2. M-05 由「补采样元数据」升级为「**不采样或自描述**」；
3. I-04/I-06 增补「**稳定去重键 + 行级字节上限 + 截断标记**」，对标 DSH 的有界队列与去重键；
4. 时间信息改为「**单点 `time` + 成对事件派生**」，避免显式双时间戳引入第二个真值。

**不应照抄的三处**：DSH 无采样元数据、无丢弃计数、无保留期——属其已知债，不是最佳实践。
另外两处需注意而非照抄：其 OTel severity 把 `aborted` 记 info（取消在告警面不可见）；默认 `FEEDBACK_ONLY` 导出意味着它的「轨迹」不是持续监控通道。

---

## 12. 实施落地记录（Wave 0 + Wave B + 选取 D）

用户批准「pony-sentry 消费者侧 Wave 0 + Wave B + 选取 D」与「允许 DB 迁移（trace_turns + 索引）」后，
由 dev team 按冻结契约 `.dev-team/trace-fix-contract.md`（v1.3）实施：

| 波次 | 落地内容 | 证据 |
| :--- | :--- | :--- |
| Wave 0 | 空错误/状态矛盾诊断标注与红黄标（M-01/W-06）、POST 响应 `id` 可 `GET`（W-07）、响应 `total_*` 与库内一致（W-08）、分页参数 400 与 5xx 不回显 DB 文案（I-10） | `40-verify` 后新增 `trace-fix-redphase/smoke-real-incident.txt` |
| Wave B | 合并语义 v2（终态单调 + 字段级深合并 + tool_calls 按 id，W-01）、`trace_turns` 行级表 + 事务/advisory lock（W-02）、`extra/tags` 深合并（W-09）、双写口径收敛（W-10） | `migrations/0005_trace_turns.sql`、`crates/core/src/trace_v2.rs` |
| Wave D | trace 四指标（I-06）、`note` 落库（M-03 部分）、retention 按 `updated_at`（I-11）、run_id/turn_status/时间窗过滤、契约文档（I-08） | `docs/trace-contract.md`、`README.md` trace 章节 |

**验收结果**（物理收据见 `.dev-team/trace-fix-redphase/`）：
- 红相冻结：`tests/acceptance_trace_v2.rs` 11 条 + `web/tests/trace-ui.test.js` 3 条，红相 11+3 失败（`red-phase-trace-v2.txt`）；
- 绿相（含两轮 L3 修复后）：`cargo test --workspace` **183 passed / 0 failed（PG 启用，SKIP=0）**、
  `cargo clippy -D warnings` Exit 0、`pnpm test` 40/40、`pnpm build` Exit 0、`acceptance_trace_v2` **11 passed**
  （`gates-lead-final3.txt`）；
- 真实事故 payload 冒烟：`diagnostics={"error_contract":"missing","status_conflict":true,"error_missing":true}`、
  `turn_status=failed`、`turns_count=1`、响应 `id` GET 200、`failed→running` 不回退、四个 trace 指标可见
  （`smoke-real-incident.txt`）；修复后控制台截图见 `console-trace-detail-after-fix.png`（Turn 头 `failed` 徽标 +
  「失败原因」区三条诊断）。
- 迁移回填：dev 库 37 行只读快照 → `trace_turns` 57 行、0 口径不一致、重放幂等；回填改为 Rust 侧运行时 sha256 口径
  （`turn_srv_*`），两轮迁移 idempotency/CONCURRENTLY 索引/64 位锁键均实测（`gates-fix-core2.txt`）。
- 对抗审查与关闭：L3-A/L3-B 各判 FAIL（1 BLOCKER + 5/4 MAJOR），修复后终审
  **PASS**（`adjudication2.json`：0 OPEN / 0 PARTIAL）；控制台补齐失败状态色与 20KB payload 预览上限。
- **第二轮（v1.5，2026-10-23 23:xx）**：剩余条目已收口——M-04（脱敏单引号/裸 token）、W-05（24h 越界 + 默认 reported_at 排序）、
  I-01（`_unmapped`）、I-02（`_warnings` 归一）、I-07（`_redactions` 计数）、W-04（`trace_turns.run_id` + 双来源过滤）、
  I-04（`fields=summary` + `X-Total-Count`）、I-06（3 个计数器 + 20k 截断）、I-09（可选字段回退）、M-03（可选 webhook + 待评审预设）、
  M-02/M-05/M-06（job_copilot 生产者：`llm_calls[]`/`sampled`/`input_summary`，96 条契约测试通过）。
  门禁：`cargo test --workspace` **206 passed（PG，SKIP=0）**、clippy 0、acceptance **11 + 10 passed**、`pnpm test` 40/40、build 0
  （`gates-lead-ext.txt`）；生产库迁移已执行并校验（38 traces → 59 turns，0 不一致，见 `migration-runbook.md`）。
  仍未做：issue 建单（改为 webhook + 待评审预设，按审查建议延后）。
- **第三轮（v1.6，ext 波两路 L3 复审后的修复）**：M-04 引号内嵌不再提前闭合、`_` 前缀未知键保留、
  warnings 并集、脱敏→截断顺序、0006 down 与 run_id 回填、`llm_calls` 服务端归一（≤50/白名单）、
  producer `input_summary` 去 head、采样率原值语义、webhook 双上限/并发闸/host 日志、summary 去 note。
  门禁：workspace **218+ passed（PG，SKIP=0）**、clippy 0、acceptance **11 + 10 + 8**、web 40/40、build 0；
  producer **100 passed**。两路 L3 的 16 条 ext findings 逐条关闭（见 `review-ext-l3a/b.json` 与复审判据）。
