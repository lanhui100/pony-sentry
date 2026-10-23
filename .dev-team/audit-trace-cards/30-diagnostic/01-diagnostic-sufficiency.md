# 诊断充分性评估（auditor-diagnostic / task-3）

- 样本：pony-sentry `traces.id = ca0b5494-ae1b-47db-8e00-c9e7abbb6779`（`session_id=thread_ref_v1_e9b4ec395c96d8913c106ef8`，
  `run_id=run_83c1879e338d442f953a5dd4b8d1646a`，payload 3405 bytes，1 turn / 3 tool_calls）
- 审核问题：**只给审核员一份 pony-sentry trace（HTTP 详情接口返回的 JSON+DB 列），能否把这次事故定位到「哪一层、什么错、为什么停」？**
- 所有判定均可机器复现，命令与真实输出见 `04-receipts.txt`（下文 `RCP-xx`）。
- 只读取证；未修改 pony-sentry 与 job_copilot 任何受版本控制文件。

---

## 1. 判定口径（先立规矩，避免自证）

| 判定 | 定义（严格） | 计分 |
| :--- | :--- | :--- |
| **能** | 仅凭该 trace 的字段值即可直接得出该结论，无需外部知识/源码/日志 | 1.0 |
| **部分** | 需要叠加「外部常量或经验」（如 120s 恰好是超时阈值）或只能得出弱化结论（如「某工具非成功」而非「重试耗尽」） | 0.5 |
| **不能** | 该 trace 内不存在可支撑结论的字段，任何推断都只能是猜测 | 0 |

**证据边界**：只用 `curl /api/v1/traces/:id`（admin token）返回体与 `traces` 表列（RCP-01/RCP-02）；
不使用 job_copilot 源码、进程日志、job_copilot 本地 DB。对比时**另行标注**，并计入「增益反事实」（§6），不计入基线分。

---

## 2. 真因链（只读引用，不重开调查）

来自 `/home/dm/job_copilot/.dev-team/evidence-cards-run/05-verdict.md` §A/§B/§F 与 `03-error-signature.txt`：

```
① b98436cb9 把 jd_v6 文本角色切到 workbuddy/deepseek-v4.1-flash
② 结构化输出流首个 chunk 非法 → openai SDK from_json("") → ValueError: expected value at line 1 column 1
③ format_jd_to_json 3 轮工具重试（每轮 2 次 LLM 调用）全部空结果 → 重试耗尽
④ embedded_runtime 节点 120s 兜底 → TimeoutError（node_started 19:27:19.70 + 120s = 19:29:19.70，实测 19:29:19.859）
⑤ SSE run_error(504) → 前端 sse-event-error（19:29:30.582）
```

本次审核要回答的是：**pony-sentry 里的这份 trace 能把这 5 环中的哪几环「指认到字段」**。

---

## 3. 八问逐题判定

| # | 定位必须回答的问题 | 判定 | 依据（trace 实际有的字段/值，RCP-02） | 缺口（trace 中不存在的字段） | 对应真因环 |
| :-- | :--- | :--- | :--- | :--- | :--- |
| **D1** | 这次 run 是否失败？属于哪个项目/环境/版本/模型？（能否聚合与告警） | **能** (1.0) | `turns[0].status="failed"`、`total_duration_ms=120272`、`project=job_copilot`、`environment=development`、`release=dev`、`turns[0].model="workbuddy/deepseek-v4.1-flash"`、DB `eval_status=unreviewed` | — | ⑤（终态） |
| **D2** | 失败发生在**哪一层/哪个节点**（图节点、LLM、工具、网关、前端）？ | **不能** (0) | 仅 `turns[0].step="other"`（未知节点收敛值）、3 条 `tool_calls` 证明「工具被调用过」 | 无 graph node 名、无 parent/child span、无 `llm_calls[]`、无 `span_type` 维度、无 `error.kind` | ①②③④ 全部 |
| **D3** | 失败的**机制与错误签名**是什么（异常类型/消息/堆栈/上游返回体）？ | **不能** (0) | 无（`turns[0]` 无 error 键；3 条 tool_call 的 `error` 全为 `null`，**包括 status=error 那条**） | `turn.error_type` / `turn.error_message` / `stack` / `upstream_body`；生产者侧契约 R1 明确「error_message 不外发」 | ②（ValueError） |
| **D4** | 是**哪个工具**出问题，它算业务成功还是失败，失败原因是什么？ | **部分** (0.5) | `tool_name=format_jd_to_json`×3、`duration_ms=39706/39342/19554`、`arguments_summary` 有原文、第 3 条 `status="error"`、前 2 条 `output` 内含「职位解析失败（原因: 模型返回空结果）」 | 无业务态字段（`status` 是传输态）；第 3 条 `error=null`；无「重试轮次」语义 | ②③（只能看到「失败文案」，看不到「为什么空」） |
| **D5** | 是否重试？几轮/几次？为什么重试耗尽？ | **不能** (0) | 仅 `attempt_index=1`（**运行级**尝试序号，不是工具/LLM 重试轮次）；3 条同工具调用可数出「被调用 3 次」 | 无 tool/LLM 级 attempt、无 `round`/`max_retries`/`budget`、无每次调用起止时刻、无 `llm_calls[]` | ③（3 轮重试耗尽） |
| **D6** | 是否触发了**超时兜底**？何时开始、何时被杀、事件时序？ | **部分** (0.5) | `duration_ms=total_duration_ms=120272`（≈120s 整数档，强提示）；`reported_at_ms=1792754960002`（唯一时间点） | 无 `started_at_ms`/`completed_at_ms`、无 `error_type=TimeoutError`、无 `timeout_ms`/`deadline_exceeded` 标记、无事件序列 | ④（120s embedded_runtime TimeoutError） |
| **D7** | **LLM 侧**发生了什么（调用几次、每次模型/耗时/状态/token/费用）？ | **部分** (0.5) | `turn.model`、`input_tokens=36612`/`output_tokens=1368`/`cache_hit_tokens=23936`（**只有合计**） | 无逐次调用记录、无失败调用计数、无每次耗时（8.48s/13.24s/16.77s/…）、失败调用 token 缺失 | ①②（上游返回非法 JSON） |
| **D8** | 能否**跨项目回链** job_copilot（request_id / 用户 / 会话 / 日志）？ | **部分** (0.5) | 顶层 `run_id`（可 grep job_copilot 日志的 run_id 行）；`tags.trace_id`；`extra.assistant_id` | 无 `request_id`（生产侧有值未外发）、无 `user_id`、`thread_id` 键存在但值为 `null`、`session_id` 为 HMAC `thread_ref_v1_*` 单向不可逆 | ⑤（前端/请求侧关联） |

---

## 4. 覆盖率（量化）

| 口径 | 计算 | 数值 |
| :--- | :--- | :--- |
| **加权诊断覆盖率**（能=1 / 部分=0.5 / 不能=0） | (1+0+0+0.5+0+0.5+0.5+0.5) / 8 | **3.0 / 8 = 37.5%** |
| 严格可答率（仅「能」） | 1 / 8 | **12.5%** |
| 完全不可答率（「不能」） | 3 / 8 | **37.5%** |
| 部分可答率 | 4 / 8 | **50%** |

按真因链分段看更刺眼：**5 环中只有第 ⑤ 环（终态失败）能被字段直接指认；②③④ 三环（错误机制/重试耗尽/超时兜底）
全部落在「不能」或「靠 120s 数字猜」的地带**——也就是说，这份 trace 足以「知道失败了」，不足以「解释为什么失败」。

---

## 5. 三个最致命缺口（按对本次事故的杀伤力排序）

| 排序 | 缺口 | 直接导致无法回答 | 本次物证 | 生产者侧根因 |
| :-- | :--- | :--- | :--- | :--- |
| 1 | **全链路没有任何错误类型/错误签名** | D2、D3（以及 D4 的一半） | `turns[0]` 键集合无 error*；3 条 tool_call `error` 全 null，含 status=error 那条（RCP-02/A4、RCP-04） | R1 只禁止 `error_message` 自由文本外发，**但未提供 `error_type` 受控通道**；且 tool 侧采集点把 `error_type` 拿到手后丢弃（`tool_observability_middleware.py:259,278` 有 `error_type`，`:279` 硬编码 `error_message=None`，`:115` 只取 error_message） |
| 2 | **没有任何 LLM 调用级 span** | D5、D7 大部分 | trace 只有 1 个 turn 聚合；job_copilot 本地同一 run 有 **8 条 llm span / 4 条 error**（RCP-08） | `llm_calls[]` 从未被纳入 payload 契约（契约 §3.2.5 R21 只聚合 tool_calls） |
| 3 | **没有 turn 级时间戳，也没有工具级起止时刻** | D6、D5 的时序部分 | 只有 `reported_at_ms`；`duration_ms` 是标量 | payload 契约只有 `duration_ms`，无 `started_at_ms/completed_at_ms`；上报是**单次终态**，无增量事件 |

> 注：`step="other"`（D2 的另一半）是同样严重的问题，但它的修法不是「补导出」而是「改节点词表」
> （`APPROVED_CURRENT_STEPS` 7 值不含图节点名，`metrics.py:19-29`），成本高于前三条，单列在
> `02-producer-semantics.md` §7 与 `03-cross-project-contract.md` P7。

---

## 6. 反事实增益（每补一个生产者字段，能救回多少覆盖率）

单样本、非线性外推，仅用于**排序**（不作为承诺值）：

| 补丁（全部在生产侧，消费者已能存——RCP-06/P6 证明） | 预计覆盖率 | 增量 | 主要救回 |
| :--- | :--- | :--- | :--- |
| 基线 | 37.5% | — | D1 |
| + `turn.error_type` / `tool_calls[].error_type`（受控枚举） | 62.5% | +2.0 题 | D3 满分；D4 满分；D2 升为部分 |
| + `llm_calls[]`（每次调用 model/status/duration/tokens/attempt） | 81.3% | +1.5 题 | D5 满分；D7 满分 |
| + `turn.started_at_ms/completed_at_ms` + `tool_calls[].started_at_ms` | 87.5% | +0.5 题 | D6 满分 |
| + `extra.request_id` + `extra.user_id` | **93.8%** | +0.5 题 | D8 满分 |
| （需改节点词表）+ `step=<图节点名>` | 100% | +0.5 题 | D2 满分 |

**关键结论：93.8% 的覆盖率可以由「生产者补字段」单独拿到，不需要 pony-sentry 改 schema、不需要改合并语义**
（依据：ingest 的 `turns` 是 `Vec<serde_json::Value>` 不透明存放，见 `03-cross-project-contract.md` §1.1 与 RCP-06/P6）。
这正是把「诊断充分性」问题归因到**跨项目契约**而不是消费者实现的原因。

---

## 7. 结论

1. 该 trace 在当前形态下只能回答「**某项目某环境某 run 失败、耗时 120s、某工具被调用 3 次**」，
   无法回答「**哪一层出错、什么异常、重试怎么耗尽、是不是超时**」。
2. 加权覆盖率 **37.5%**，严格可答率 **12.5%**；真因链 5 环中 3 环（错误机制 / 重试耗尽 / 超时兜底）不可定位。
3. 缺口集中在**生产者侧的字段供给**（错误类型、LLM 调用级、时间戳、request_id），
   而**消费者侧的存储通道已经能承载**（不透明 turns + 未知字段原样入库），因此首要建议是
   **跨项目契约补字段（生产者改，S 级）**，而非 pony-sentry 大改。

> 复核入口：`04-receipts.txt` RCP-01 / RCP-02 / RCP-04 / RCP-06 / RCP-08。
