# Agent Note: pony-sentry trace 诊断力审核与契约补全提案

Status: proposed

## Problem

pony-sentry 现有 trace 通道按「评测语料」定位设计（`EvalStatus` 六值全部为 triage/eval 语义），
对故障诊断天然不足：2026-10-23 job_copilot Tilt dev 会话 `efd84b9f-…`（run `run_83c1879e…`）失败后，
库内唯一 trace（`traces.id=ca0b5494-…`）只能回答「失败了」，无法回答「哪一层、什么错、为什么停」。
以 8 个定位必答题衡量，加权诊断覆盖率仅 **37.5%**（严格可答 12.5%），事故真因链 5 环中 3 环不可定位。

根因两处：
1. **跨项目契约供给不足**——生产者按 R1 不外发错误文本、且从不上报 LLM 调用级 span 与 turn 时间戳；
2. **消费者缺护栏**——`turns/tool_calls` 零 schema 校验、合并语义「重复轮次以旧为准」可永久钉住非终态、
   同 session 并发上报读-改-写无事务（实测 18–19/20 丢轮）、二次上报把 `extra` 整体置 null、
   控制台不渲染 `turn.status`；脱敏对单引号 repr 与裸 `token=` 存在绕过面。

本 note 记录审核结论与后续改造提案；本轮**不实施**任何代码变更（用户明确「只审核评估」）。

已落地部分见 `.agents/notes/implemented/architecture/2026-10-23-trace-merge-v2-and-turn-store.md`（合并 v2 + 轮次存储 + 诊断标注）。

关联既有决策：`.agents/notes/implemented/feature/2026-10-17-trace-ui-interrupt-and-collapsible-tool-calls.md`（trace 展示层，明确 UI 不反向约束上报 schema）、
`.agents/notes/implemented/feature/2026-10-21-multienv-issue-dimensions-and-ingest-idempotency.md`（traces 移出公网与保留期）。

## Proposal

分四波推进（详见 `docs/release/trace-audit-cards-2026-10-23/report.md`）：

- **Wave 0（消费者，≈1 人日）**：空错误检测与红标、POST 响应 id 回读、响应 `total_*` 与库内一致、分页参数校验。
- **Wave A（生产者，≈2–2.5 人日，零 schema 变更）**：全部落在 `turns[]` 内——`error_type`（受控词表）、
  `started_at_ms/completed_at_ms`、`calls_total/calls_failed`、`token_scope`、采样元数据、`output` 受控抽取。
  不引入 `user_id`/明文 `thread_id`/`error_message`（隐私红线），也不用 `extra`（二次上报会被置 null）。
  覆盖率 37.5% → ~56%；加 `llm_calls[]` 后 ~75%，加时间戳 ~81%，节点词表 ~87.5%（当前隐私不变量下的天花板）。
- **Wave B（消费者，≈3–4 人日）**：合并语义 v2（状态单调 + 字段级深合并）、事务/行锁（含 `lock_timeout`/池预算，
  或 `trace_turns` 表）、`extra/tags` 深合并、空错误检测与红标、POST 响应 id 与 `total_*` 回读一致性。
- **Wave C（双方，≈3–4 人日）**：`llm_calls[]`、控制台失败原因区与 LLM 调用折叠、
  List `run_id`/状态/时间窗筛选 + 投影 + `limit` clamp。覆盖率 → ~93.8%。
- **Wave D（消费者，≈1.5–2 人日）**：trace→issue/告警/指标、`note` 落库、脱敏粒度与 `_redactions`、契约文档化。

## Alternatives considered

1. **只做控制台展示优化（纯前端）**：成本最低（S 级），但数据本身不含错误签名与 LLM span，
   展示得再全也回答不了「为什么失败」——否决。
2. **外发 `error_message` 原文/traceback**：信息量最大，但违反 R1 隐私契约且会被 `sanitize_error_message`
   清零（恒 `[REDACTED]`），并扩大泄露面——否决，改用受控 `error_type` 词表（D3 因此只能答到「部分」）。
2b. **外发 `user_id` / 明文 `thread_id` / `request_id`**：会绕过 HMAC 隐私设计，且读侧为 tailnet 只读、`environment` 由客户端自报——
   否决；跨项目回链维持使用明文 `run_id`。
3. **消费者侧反解 `thread_ref_v1_*` 回链业务会话**：HMAC-SHA256 单向、无 key 不可逆，
   且违反生产者 INV-2（明文 thread_id 绝不外发）——否决，回链留在生产者侧（`request_id`/日志）。
4. **立即重写 trace 存储为「session 行 + turn 行」并改冻结的合并契约**：能根治 W-01/W-02，
   但属破坏性变更，需与生产者同步且触及已冻结语义——本轮否决，列为 Wave B 内的灰度方案。
5. **把诊断缺口全部归因 pony-sentry 并大改消费者**：与事实不符——`turns` 不透明存放，
   80% 诊断收益可由生产者补字段零改动拿到——否决。

## Acceptance criteria

- Wave A：`payload.turns[0].error_type` 存在且 ∈ `_SAFE_ERROR_TYPES`；`tool_calls[i].error_type` 在工具抛
  `TimeoutError/CancelledError` 时非空；`extra.request_id` 非空且经 `sanitize_trace_string`；
  体积回归 8 次 `llm_calls` < 64KB；同 session 用新 `turn_id` 二次上报后两条 turn 均可见（消费者 e2e）。
- Wave B：同 turn 先报非终态后报终态，`GET` 必须返回终态；20 session × 2 并发上报后每 session turn 数完整且
  `lock_timeout` 内完成；POST 响应 `id` 可 `GET` 到 200；响应 `total_*` == 库内 `total_*`；二次上报后 `extra` 不丢字段。
- Wave C：控制台抽屉对 `turn.status=failed` 显式展示；`?run_id=` 精确命中；`?limit=100000` 被 clamp。
- 全波次：不得破坏 `.dev-team/contract-console-timeout.md` 的 R1–R8（控制台读超时预算）。

## Risks

- W-01/W-03 属语义破坏性变更，需生产者同步与 `merge_semantics:v2` 灰度标记，否则历史 trace 语义漂移；
- 补 `user_id`/`request_id` 扩大隐私面，需限定环境（dev/preprod）并沿用受控脱敏；
- `llm_calls[]` 增加体积（8 次 ≈1.5–2KB），需 50 条上限与 2MB 硬限兜底；
- 成本估算（人日）未经实际排期验证，属裁量；
- 审核基于单一样本 + 37 条存量 trace，长会话与高并发场景的结论来自构造实验而非生产观测。

## External benchmark（DSH 轨迹实现）

对照 `/home/dm/deepseek-harness @ dsh-v0.2.1-rc.2` 的会话事件账本实现复核本提案，27 条审核结论中 18 条被印证、
7 条方向成立但实现方式需修正、3 条不可比、0 条被反驳。据此修正三处：
1. `error_type` 不做闭集收敛，改用**稳定码 + 保留未知码**（DSH 将「用最近通用类覆盖下层结构化失败」列为归因反模式）；
2. 采样二选一：**不采样全量入账，或任何采样都必须自描述 `sampled/sample_rate`**；
3. 容量治理增补**稳定去重键 + 行级字节上限 + 截断标记**（对标 DSH 有界队列与 `(session.id,format_version,seq)` 去重）；
4. 时间信息优先「单点 `time` + 成对事件派生」，避免显式双时间戳引入第二个真值。

不复刻 DSH 的三处已知债：无采样元数据、无丢弃计数、无保留期。详见
`docs/release/trace-audit-cards-2026-10-23/dsh-benchmark-review.md`。

## Verification

- 审核报告：`docs/release/trace-audit-cards-2026-10-23/report.md`
- 机器收据：`.dev-team/audit-trace-cards/`（基线、契约矩阵、判据、各角色收据、59 条独立裁决、2 路对抗审查）
- 外部基准：`docs/release/trace-audit-cards-2026-10-23/dsh-benchmark-review.md`
- 复审入口：`40-verify/run-independent-receipts*.sh`、`run-verify-t2-findings.sh`
