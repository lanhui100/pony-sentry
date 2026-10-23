# L3-A 对抗审查（并发 / 状态 / 数据完整性）— trace v2

审查对象：契约 v1.3 + 工作树实现（HEAD 9695676 + 未提交改动）。只读审查，未改代码/测试。
**VERDICT: FAIL**（1 BLOCKER + 5 MAJOR + 4 MINOR，明细见 `review-l3a.json`）

## 机器复核的硬证据
- **F1**：/tmp 独立库直接执行 `migrations/0005_trace_turns.sql`。payload 里两条均缺 `turn_id` 的 turn → `trace_turns` 只有 1 行（`turn_auto_7b22737461747573`），`turns_count=1` 而 `payload.turns=2`。`hex()` 是字节编码，截前 16 hex = 只取 JSON 前 8 字节，同前缀必撞。
- **F2**：同一 legacy turn 迁移后键 `turn_auto_7b22…`，运行时 `turn_key_of` 算出 `turn_auto_117bf6b6f370aa6a` → 升级后重报即多一行、tokens 重复计入。

## 逐项结论
1. **合并语义 v2**：终态单调（旧终态不被非终态回退）、字段级深合并、tool_calls 按 id 原位合并、diagnostics 双方合集均按 C2 落地；diagnostics 每写由 `annotate_turn` 全量重算，**不存在** diagnostics 无界增长。例外：F5（无 id 的 tool_call 每次重报追加）、F10（契约允许新终态覆盖旧终态）。
2. **并发**：PG `pg_advisory_xact_lock(hashtext(session_id))` 在事务内且先于读改写，SQLite `BEGIN IMMEDIATE` 真正串行化写事务；聚合行与 trace_turns 写入同事务，任何一步失败整体回滚、不留孤儿（hydrate 也在同事务）。例外：F4（retention 两条 DELETE 在事务/锁外）、F8（32 位锁键碰撞仅增阻塞）。
3. **迁移**：回填幂等（PG 每次启动重放安全、`ON CONFLICT`/`turns_count=0` 守卫）；seq 按数组序稳定；老行 diagnostics 缺失兼容（只有新写才标注）。但 F1/F2/F3 是真实数据完整性缺口；F6 的“幂等”断言在 SQLite 上是空转。
4. **聚合口径**：同 turn 重报因 `(session_id,turn_id)` 主键去重，**不会**把 tokens 重复求和（PASS）；`turns_count` 取实际读回行数，正常路径不会自发漂移。但 F1（迁移丢行）、F3（部分补齐被跳过）、F4（retention 竞态）都会让 `turns_count` 与 `payload.turns`/真实行数漂移，F2 叠加 F1 后还会双计 tokens。F9 是溢出型污染（debug panic / release 负值）。
5. **测试强度**：无空断言用例；C9.1/C9.3/C9.4/C9.5/C9.6/C9.7/C9.8 的断言是结构化的、能力缺失版本会红。并发用例在 SQLite 上由 `BEGIN IMMEDIATE` 决定性串行，不是偶发通过。缺口：F7（PG 用例 SKIP 即通过，最终门禁未带 `PONY_TEST_PG_URL`；冻结验收 C9.2 只跑 SQLite 与契约明文不符）、F6（SQLite 迁移幂等靠“没执行”通过）；另无“同 turn 带 tokens 重报后 totals/turns_count 不变”的守护用例，也无缺 `turn_id` 回填的负向用例（正好漏掉 F1）。

## 残留契约风险（非实现缺陷，供 Lead 裁决）
- C2.1 明文允许“新终态→新覆盖”：迟到的 `succeeded` 会抹掉 `failed`，且旧 `error` 按字段合并被保留，形成 status/error 自相矛盾行（F10）。
- C2.3 只定义按 `tool_call_id` 合并，未规定缺 id 条目；建议契约补“无 id 时按内容哈希归并”（F5）。
