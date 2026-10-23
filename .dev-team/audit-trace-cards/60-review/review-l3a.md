# L3-A 对抗审查（契约/数据完整性 · 只读）

**verdict: FAIL**（MAJOR 10 / MINOR 4 / BLOCKER 0）。探针 `audit-probe-l3a-*` 23 行已删（23→0）；业务行 37、事故行 md5 `8abb8722…` 不变（`60-review/receipts-l3a.txt:69-83`）。

## 逐条

- **F1 [MAJOR] 验证层未回填/未吸收**：verifier 已 59 CONFIRMED / 0 REFUTED（`40-verify/01-verdicts.json`），`03-gap-report.md` 列 G-01..G-04；报告 §9.3:281 仍是占位，:19 写 19/20（verifier B-03 为 18/20）、:41 写 3405B（B-01 为 5821 bytes），且全文无 G-01/G-02/G-04。
- **F2 [MAJOR] payload 体积单位错**：:41 的 “3405B” 实为 `length()` 字符数；`pg_column_size=2641`、`octet_length=5821`（receipts:11）。10-contract:172 已要求注明表达式，报告却把字符数当字节数写进“库内真值”。
- **F3 [MAJOR] 漏审幻影 id**：二次上报 POST2 返回 id `0193e6fc…`，库内 id `39530988…`，GET 响应 id → 404（receipts:22-26；verifier §B3-E3）；根因 `pg_repository.rs:659-699`（ON CONFLICT 不更新 id，`saved=trace` 返回新 UUID）。W-01/W-04 未列。
- **F4 [MAJOR] 漏审 POST 响应 total 分叉**：verifier §B3-E3 响应 `resp_total_in=999/500`，同 session 库内 `100/120000`（`02-independent-receipts.txt:634,639`）；根因 `pg_repository.rs:636-657` 用 effective_* 入库、`:697-699` 返回未重算 trace。W-03 只写“库内三套口径”，漏了“响应体第四套”。
- **F5 [MAJOR] 脱敏只审过度、漏审绕过**：I-07:207 只说路径改写/无计数；实测 `arguments_summary="{'password': 'hunter2'}"`、`message="token=abc123def456 rejected"` 原样落库（receipts:65-66），`sanitizer.rs:35` 只匹配双引号形态；verifier G-04 同结论，且本次 tool 参数正是单引号 repr。
- **F6 [MAJOR] 严重度与冻结判据双向漂移**：`02-audit-criteria.md:542` 定 P0=`W1,W7,M1,M2,M4,I1,I3,I4`；报告把 W1→W-06、M2→M-02、I1→I-03、I3→I-02 全降 P1（:202-203,145,181），又丢掉 W7/M4；同时把本样本未触发的 W-01/W-02 列为 P0（:25；W-01 本属 criteria I4 的 P0，问题在严重度定义与样本影响不一致）。按报告 D 表 D2/D3/D5“不能”（:161-164），这些恰是“阻断诊断”。
- **F7 [MAJOR] 路线图顺序/零改动自相矛盾**：brief:73 要求“P0 立即”，报告先 Wave A 生产者字段（:217）、后 Wave B P0 止血（:218）；Wave A 称“消费者零改动”，但 diag F5（`03-cross-project-contract.md:19`）明确补字段后需纯前端展示，报告 I-05:205 也承认控制台不渲染 turn.status。
- **F8 [MAJOR] 27 条判据 11 条无对应 finding**：W5 eval_status 双写、W7/W8、M3-M8（重试/超时/父子/input_text-R19/关联键/采样丢弃）、I11（422 无痕）均无 finding（`02-audit-criteria.md:127-138,157-194,232-311,463-480`）；`grep '逐字段对照矩阵' report` → NO_HITS，而 brief:69 要求该节。报告:15/61 声称 27 条判据却无映射表。
- **F9 [MAJOR] “SELECT … FOR UPDATE 止血”对首次上报无效**：session 首报时行不存在，`SELECT … FOR UPDATE` 返回 0 行、不产生行锁（receipts:55-58）；两个并发事务各自 INSERT，ON CONFLICT 再以各自旧 payload 覆盖。receipts:49-51 的 20/20 丢轮正是该形态；源码 `pg_repository.rs:619-632`。
- **F10 [MAJOR] 未评估冻结契约/Non-Goal 冲突**：Wave B/I-03 的行锁/事务与 `turn_status` 列、`trace_turns` 表（:203,218）会触及 `contract-console-timeout.md:21,25`（R4/R8：每站点 `acquire_timeout=3000`，变更须重走冻结并复核 `API_TIMEOUT_MS=8000`），且违反 brief:30“不改数据库 schema”；报告只写“存储形态”影响，未提裁决/池等待/500 误判风险。
- **F11 [MINOR] 死契约只抓 stats_incomplete**：`App.vue:767,769` 渲染 `turn.phase/turn.sequence/turn.node`，但 producer 目录 `grep "phase|sequence|node"` 0 命中，field-matrix:105-122 的 turns 15 键也无这三键；I-09:209 只列 `stats_incomplete`。
- **F12 [MINOR] M-01 的 error 形状与生产者不符**：`payload.py:296` 把 `error` 写成 `str(...)[:500]`（field-matrix:135 记 string），报告:178 却要求 `{type,message}` 并标“向后兼容新增字段”；与 §7（:239-241 用独立 `error_type`）也不一致。
- **F13 [MINOR] R1 归属错配**：报告:17/253 称“生产者按 R1 不外发任何错误文本”；diag:30 明确 R1 只禁 `error_message` 自由文本，`_SAFE_ERROR_TYPES` 22 类已存在（`trace_safety.py:287-300`），生产者 `payload.py:296` 是自己丢弃 `error_type`。这会把“生产者零契约变更即可补受控 error_type”误写成“需先改契约”。
- **F14 [MINOR] 两处引用/口径错**：W-01:114 指向 `trace_repository_test.rs:248`（幂等 upsert），真正冻结 old-wins 的是 `:323`（gap G-07）；M-01:175 写 `"output":null`，实际是 output 键缺失（gap G-05；`jq has("output")` → false）。不影响结论方向，但会误导修 W-01 要改哪条测试及“键缺失 vs 显式 null”的契约语义。

## 结论

报告的事实结论大体被 verifier 59/59 确认，但作为“采集质量审核报告”仍有 10 条 MAJOR：证据单位/引用与验证层未回填、三类实现级数据完整性缺陷（幻影 id、响应分叉、脱敏绕过）漏审、严重度/路线图与冻结判据冲突、27 条判据 11 条无映射、P0 修复建议技术不成立及与冻结契约/Non-Goal 的隐性冲突。建议按 F1-F10 修订后重交；F11-F14 可并入同批。
