# 外部基准复核：借 DSH 的轨迹实现验证本次审核结论

> 基准对象：`/home/dm/deepseek-harness` @ `dsh-v0.2.1-rc.2-1-g5ad609c9ea`（TS monorepo）
> 目的：用一套成熟 Agent Harness 的轨迹/遥测实现，检验本次审核的 27 条结论是否成立、是否需要修正
> 方法：只读代码与文档（`docs/subsystems/session*.md`、`session-telemetry.md`、`otel.md`、`packages/core/session`、
> `packages/session/session-*`、`packages/llm/llm*`、`docs/persistence-catalog.md`）+ 侦察员收据；不改 DSH、不改 pony-sentry 代码
> 日期：2026-10-23

---

## 1. DSH 轨迹实现的六个关键事实

| 维度 | DSH 的做法 | 证据 |
| :--- | :--- | :--- |
| **数据模型** | 会话 = **append-only 事件日志**（62 种事件，`seq` 单调递增、`time` epoch ms 逐事件），消息历史由日志**派生**而非另存；重放即重建 | `docs/subsystems/session.md:3,9`；`packages/core/session/src/types.ts:270-300`；`docs/persistence-catalog.md`（62 行事件目录） |
| **粒度** | 每次模型调用一个 `step/start`…`step/end`；工具 `tool/call`↔`tool/result` 配对；失败/重试/取消的模型尝试单独记 `assistant/attempt` 与 `llm/retry` | `types.ts:288-310,350-380`；`packages/llm/llm-retry/src/types.ts:16-48` |
| **失败身份** | `turn/end.reason` 是**受控判别式**：`completed/aborted(reason)/blocked/error/max-tokens/interrupted/forked`；`tool/result.error = {name, code, reason?}`（仅 `isError`）；`LlmFailure = {message, code, status?, providerRetryAfterMs?, requestId?}` | `types.ts:202-230,384`；`packages/llm/llm/src/types.ts:41-59` |
| **错误码治理** | **开集字符串码**（`HarnessError.code`，路由只按 code 不解析 message），无中心闭集枚举；LLM 有 6 个规范码 + HTTP 映射 | `packages/llm/llm/src/error.ts:13-35,175`；`packages/llm/llm-deepseek/src/transport.ts:29-35` |
| **脱敏/隐私** | 零内置规则，只有 `session-telemetry/record` waterfall 扩展点（**fail-closed**：监听器抛错即丢弃该条）；**只脱敏导出副本，canonical 本地日志永不重写**；授权在前（新的自有 feedback 才授权整段 prefix） | `packages/session/session-telemetry/src/index.ts:26-45`；`docs/subsystems/session-telemetry.md:150` |
| **无 span，调用级信息靠派生** | 全仓无 `TracerProvider`/`startSpan`；LLM 调用级视图由 `step/start` 与 `assistant/message` 的时间差 + 流内 `time0/dt` 派生 TTFT/decode/TPS；客户端缺数据时**显式渲染「not recorded / first token unavailable」** | `packages/client/ui-trajectory/src/client/contract/turn-metrics.ts:34`；`TrajectoryTable.tsx:343`；`session-stats/src/types.ts:22` |
| **容量/交付** | 无采样（全量 ledger）；有界队列 2048 条 / 批 512 / 单请求 4,000,000 B / 导出超时 30s，溢出 warn+丢弃；交付 best-effort，接收端按 `(session.id, format_version, event.seq)` 去重；本地 JSONL+Zstd 按 session 落盘、崩溃保留已提交前缀 | `packages/telemetry/otel/src/session-log.ts:12,65-66,89-92`；`docs/subsystems/session-telemetry.md:5,60`；`session-persistence-jsonl/README.md:57-72` |

> 一句话：DSH 把「轨迹」当作**事件账本**（append-only、逐事件身份、可重放、导出是副本），
> 而 pony-sentry 把「轨迹」当作**可合并的聚合行**（单 session 一行、读-改-写、字段在服务端被再加工）。

---

## 2. 逐条验证矩阵（27 条 findings）

图例：✅ 验证成立（DSH 的做法印证我们的判断）｜🟡 部分成立/需修正｜⚪ 不可比（DSH 无对应通道）｜❌ 被反驳

| Finding | 状态 | DSH 对应事实 | 说明 |
| :--- | :--- | :--- | :--- |
| W-01 旧轮优先吞终态 | ✅ | append-only + 单调 `seq`，无「合并覆盖」概念 | 印证「读-改-写 + 旧轮优先」是反模式；DSH 用事件追加天然规避 |
| W-02 并发丢轮 | ✅ | 每事件独立 `seq`、持久化按前缀崩溃恢复 | 印证单行读-改-写无锁必然丢更新 |
| W-03 `total_*` 多口径 | ✅ | usage 随 `assistant/message` 逐事件落账，聚合可随时重算 | 印证「聚合值不应与明细双源」 |
| W-04 `run_id/turn_id` 列脱钩 | ✅ | 每个事件自带 `turn/step`，无需在「行」上记最近一次 | 印证列级元数据与明细脱钩会失检索 |
| W-05 `reported_at` 无时序约束 | 🟡 | `time` 逐事件 epoch ms、`seq` 单调；但 DSH 也无时钟漂移校验 | 方向成立；「时钟校验」非行业通行做法，保留 P2 |
| W-06 `status=success` 却失败文案 | ✅ | `tool/result` 用 `isError` + `error{name,code}` 表达成败，不由自由文本推断 | 印证「成败必须结构化，不能靠 output 文案」 |
| W-07 幻影 id | ⚪ | DSH 无 REST 上报通道 | 不可比；pony-sentry 自身 API 缺陷仍成立 |
| W-08 响应 total 与库内不一致 | ⚪ | 同上 | 不可比 |
| W-09 二次上报清空 `extra` | ✅ | 无「整包替换」语义，事件追加不覆盖既有事件 | 印证「合并以新 payload 为基底」会清空跨轮字段 |
| W-10 列/payload 双写不一致 | ✅ | 唯一真相源=日志；身份属性只从事件派生 | 印证「同一事实两个家」的代价 |
| M-01 失败证据为空 | ✅ | `isError` 时 `error{name,code,reason}`；`turn/end.reason` 必带结构化失败 | 强验证：成熟实现对失败**恒有身份** |
| M-02 无 LLM 调用级记录 | ✅ | `assistant/attempt` + `llm/retry`/`llm/retry-started` 逐次记录 | 强验证：缺失调用级=无法解释重试/超时 |
| M-03 无处置/告警通道 | 🟡 | 采集侧**预映射 severity**（`info/warn/error`）便于接收端零配置告警；但 DSH 本身也无内置告警 | 方向成立；「先 severity 后告警」比「直接建 issue」更稳 |
| M-04 脱敏绕过 | ✅ | 脱敏靠**扩展点 + fail-closed**，且只改导出副本 | 验证方向；同时给出更安全的实现形态（见 §3.4） |
| M-05 采样元数据缺失 | 🟡 | DSH 对 ledger **完全不采样** | 需修正：见 §3.2 |
| M-06 输入上下文缺失 | 🟡 | `user/message` 完整入本地日志，导出经脱敏 | 本地全量 + 导出脱敏的取舍值得借用 |
| I-01 未知顶层字段静默丢弃 | ✅ | 事件类型 merge-extensible；未知事件默认**必需→拒收**（`ignorable` 才可跳过） | 强验证：默认拒收而非静默丢 |
| I-02 `turns` 零 schema 校验 | ✅ | 类型化事件 + `isJsonValue` 运行时校验 + 非规范 envelope 拒收 | 印证自由 JSON 的代价 |
| I-03 无检索能力 | 🟡 | SQLite FTS 按 session 检索事件；无跨会话 trace 检索 | 方向成立，形态不同 |
| I-04 无投影/无上限 | ✅ | 增量 watermark + 单请求 4MB/8MiB 上限 | 印证「列表拉全量」不可扩展 |
| I-05 控制台不渲染 turn 状态 | ✅ | `TrajectoryTable` 渲染起止/总时长/TTFT/decode，`turn-error` 行显示 message+code，缺数据显式「not recorded」 | 强验证：成熟 UI 对失败与缺失都显式呈现，不靠隐藏 |
| I-06 无指标/丢弃计数 | ✅ | DSH 亦无丢弃计数（exporter meter 恒 noop） | 说明这是行业普遍债；建议仍成立但非“独特缺陷” |
| I-07 脱敏改写证物 | 🟡 | DSH 只改导出副本，本地 canonical 不动 | **需修正**：见 §3.4 |
| I-08 契约无文档之家 | ✅ | 事件目录 `persistence-catalog.md` 自动生成 + doc-sync 校验 | 印证「契约必须有唯一家与漂移哨兵」 |
| I-09 死契约字段 | ✅ | 事件词表 + 目录生成防漂移 | 印证前端渲染字段必须有产出方 |
| I-10 分页参数 500 | ⚪ | 无 REST | 不可比 |
| I-11 retention 按 `created_at` | 🟡 | DSH 无保留期/轮转（负证） | 原缺陷仍成立；但「90 天保留」本身是 pony-sentry 的优势项 |

**统计：✅ 18 条、🟡 7 条、⚪ 3 条（W-07/W-08/I-10 因 DSH 无 REST 不可比）、❌ 0 条。**
没有任何一条被外部基准反驳；需要修正的是**实现方式**（8 条）而非结论方向。

---

## 3. 需要修正/加强的五处结论

### 3.1 `error_type` 不应是「闭集收敛」，而是「稳定码 + 保留未知」（修正 M-01/§7 契约提案）

我们的契约提案写的是 `error_type ∈ _SAFE_ERROR_TYPES`，否则收敛为 `"Exception"`。DSH 的经验相反：

- 错误路由只按 **code**、不看 message（`packages/llm/llm/src/error.ts:13-21`）；
- 码是**开集**，HTTP 映射出 `AUTH/QUOTA/RATE_LIMIT/CONTEXT_WINDOW_EXCEEDED/INVALID_REQUEST/SERVER/HTTP_<n>`；
- DSH 事故复盘明确把「用最近通用类覆盖下层结构化失败」列为**归因反模式**（`docs/postmortem/0004-landlock-partial-notice-misclassified-child-failures.md:52-54`）。

⇒ 修正为：`error_type` 采**稳定字符串码**（已知码保留原值、未知码原样透传 + 打 `unknown_code:true`），
**禁止**收敛到 `other/Exception`；同时保留 `error.name`（异常类名）与可选 `status/provider_retry_after_ms`。

### 3.2 采样不是通行做法；要么不采样，要么显式声明（修正 M-05）

DSH 的 ledger 通道**完全不采样**（全仓 `sample` 0 命中）；代价靠有界队列 + 去重键 + best-effort 承担。
pony-sentry 的生产者当前「失败恒导出、成功按 rate 采样」——这本身不是错，但**必须把 `sampled/sample_rate` 带进 trace**，
否则读者无法区分「没发生」与「未采样」。修正后 M-05 的表述从「建议补采样元数据」升级为
「**要么像 DSH 一样不采样全量入账，要么任何采样都必须自描述**」。

### 3.3 容量治理要对标「有界 + 去重 + 增量」，而非只加 clamp（加强 I-04/I-06）

DSH 的三件套值得直接借鉴：单请求字节上限（4MB，session-log 上传 8MiB）、有界队列（2048 条/512 批/导出超时 30s）、
接收端按 `(session.id, format_version, seq)` 去重。pony-sentry 现状是「单次 POST 2MB，行与列表无上限，无去重键」。
⇒ I-04/I-06 的建议补上：**引入稳定去重键（`session_id,turn_id,report_seq`）** 与「行级字节上限 + 超限截断并落标」。

### 3.4 脱敏的正确形态：只改导出副本 + fail-closed（修正 I-07/M-04 方案）

DSH 的边界非常清楚：**本地 canonical 是真相源、永不重写；导出是脱敏后的副本**；脱敏规则由部署挂载，
监听器抛错即丢该条（fail-closed）。pony-sentry 只有一个副本且在入库时被再加工，因此「路径被改写」不可逆。
⇒ 修正 I-07：不是「调整正则保留相对路径」这种局部修补，而是明确取舍——
**要么接受 pony-sentry 只存导出副本（则在契约里声明「不可作原样证物」），要么增加本地全量留存层**（成本更高）。

### 3.5 调用级时间：优先「单点 time + 成对事件派生」，而非新增双时间真值（修正 Wave A 时间戳建议）

DSH 每个事件只有一个 `time`（append 时刻），**没有 duration/started_at/completed_at 字段**；
区间、TTFT、decode、tokens/s 全由成对事件（`step/start`↔`assistant/message`、`tool/call`↔`tool/result`）
与流内 `time0/dt` 派生。这与我们在 §7 提案 `started_at_ms/completed_at_ms` 的做法不同：
显式双时间戳更易读，但会引入「同一区间两个真值」的风险（正是 W-03 的病灶）。
⇒ Wave A 时间戳建议改为：**保留单点 `time` + 成对事件（call/result、step/end）**，仅当事件天然不成对时才补 `started_at_ms`；
派生指标（TTFT/decode/耗时）由消费者计算，不落库。

### 3.6 W-01/W-02 的架构结论被外部基准加强，但严重度不变

DSH 用 append-only + 单调 `seq` + 前缀恢复 + 去重键，从架构上消除了「合并覆盖」与「并发丢更新」。
这加强了我们「单行读-改-写是反模式」的判断；但由于当前生产者单次终态投递、本次事故链未触发，
**严重度仍维持 P1（潜在）**，不改回 P0——外部基准验证的是方向，不是本项目的事故概率。

---

## 4. DSH 自己也没做好的地方（避免盲目照抄）

| 缺口 | 证据 | 对本次审核的含义 |
| :--- | :--- | :--- |
| 无采样元数据（因为它不采样） | 全仓 `sample` 0 命中 | 我们的 M-05 不能照抄「无采样即无需元数据」；pony-sentry 的生产者确实采样 |
| 无丢弃计数/指标（exporter meter 恒 noop） | `packages/telemetry/otel/src/transport.ts:7-14` | I-06「无 trace 指标」不是 pony-sentry 独有，可作为通用改进项而非重罪 |
| 无保留期/轮转 | 持久化文档 retention/rotate 0 命中（负证） | pony-sentry 的 90 天保留是优势；I-11 只修「按 created_at 误删活跃会话」 |
| 无持久化 deadline/cancelReason | `turn/end.aborted.reason` 有取消因，但无到期时刻字段 | 我们 M-02 建议的 `started_at/completed_at` 仍合理，但「deadline 字段」非必需 |
| 零内置脱敏规则 | 只有扩展点 | pony-sentry 有内置规则是优势（虽然 M-04 有绕过面） |
| 错误码为开集字符串 | `HarnessError.code: string` | 与我们的「受控词表」建议冲突 → 已在 §3.1 修正 |
| 取消在告警面不可见 | OTel severity 只把 `turn/end(kind=error)` 与 `isError` 映射为 error，`aborted` 记 info | 我们建议的「预映射 severity」须覆盖 cancelled/aborted，否则会复刻该盲区 |
| 默认仅 feedback 放行导出 | `FEEDBACK_ONLY` + on-demand 捕获，事件不持续上报 | DSH 的「轨迹」是本地账本+按需导出，不等同持续监控；对标时应区分用途 |
| 无 trace/span 关联 ID | 无 `trace_id/span_id/parent_span_id` | 跨进程/跨服务串联仍靠 `session.id + seq`；pony-sentry 的 HMAC ref 亦非关联 ID |

---

## 5. 复核结论

1. **审核评估整体成立**：27 条 findings 中 18 条被外部基准正面印证、7 条方向成立但实现方式需修正、
   3 条因 DSH 无 REST 通道不可比，**0 条被反驳**。
2. **最重要的验证**：成熟 Harness 把轨迹当「逐事件账本」（append-only + 逐事件身份 + 结构化失败 + 导出副本脱敏），
   而 pony-sentry 把轨迹当「可合并聚合行」——这正是本次事故「看得出失败、看不出原因」的结构性根源，
   也印证了报告 §8 的定位判断「评测语料通道 ≠ 故障诊断通道」。
3. **需要回改报告的三处**：`error_type` 的「闭集收敛」改为「稳定码 + 保留未知」；M-05 改为「不采样或自描述」；
   I-04/I-06 增补「去重键 + 行级上限 + 截断标记」；另将「显式双时间戳」改为「单点 time + 成对事件派生」。
   已在报告 v4 与 ADR 中修订。
4. **不应照抄的三处**：DSH 无采样元数据、无丢弃计数、无保留期——这些是它的已知债，不是最佳实践。
