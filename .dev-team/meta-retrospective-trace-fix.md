# 元框架复盘：trace v2 修复实施（wave-7）

> 复盘对象是协作元框架与组织协议本身，不是业务代码。上一轮：`.dev-team/meta-retrospective-trace-audit.md`（审计战役）。

## 1. 通信拓扑与信噪比（Topology & Noise）

- 名册：Lead + Protocol Agent（空桩/契约）+ Test Agent（红相）+ Executor×3（core / http / web）+ Subagent 池（2 路 L3 + 1 路终审）。
  常驻 5（≤8）；审查不占槽位。**Roster 触顶后改用「复用已退役审核员为 Executor」**（auditor-core→存储层、auditor-diagnostic→HTTP 层），
  避免第 9 个成员；代价是旧上下文占用（两名成员仍带完整审计上下文），收益是零等待。
- 信噪比最高：**两路 L3 各抓到 1 个 BLOCKER**——迁移回填键 hex/md5 ≠ 运行时 sha256（L3-A 实测两轮塌陷、L3-B 实测 token 翻倍），
  以及 9 处错误原文泄漏（L3-B 用真实 `DROP TABLE` 造 DB 故障验证）。这两条都是「机器全绿但语义错」，
  证明独立审查在实施轮仍是不可替代的。
- 噪声与浪费：① 首位 Test Agent 因上下文耗尽未产出（450 行测试文件写一半），重派后成功——红相任务应一次给足上下文预算；
  ② `gates-lead-final.txt` 首次记录 exit 1 却无失败项（管道退出码误捕获），重跑显式 Exit Code 才消除歧义；
  ③ task-13 漏建（web 小修），执行者先干活后补账本。

## 2. 门禁穿透与误杀率（Gate Penetration / False Negatives & Positives）

- **冻结测试被安全修复逼出冲突**：C9.6 断言「首报 eval_status 落库」，L3-B F10 指出写 token 可自评 → 冲突。
  Lead 按契约 v1.4 改断言并**重新冻结提交**（`9786975`），执行者不得自改测试——测试分权在冲突时仍守住。
- **假绿被 L3 击穿**：C9.10 迁移回填用例只造了带 `turn_id` 的存量行，回填键不一致因此不可见；
  L3-A/B 各自用「缺 turn_id 的两条 legacy turn」反例打穿。修复后新增两条单测（SQLite+PG 各一）锁死该边界。
- **PG 门禁缺口**：`PONY_TEST_PG_URL` 未设时 PG 用例 SKIP 即通过，C9.2 并发只覆盖 SQLite。Lead 最终门禁显式带上 PG URL，
  并要求收据输出 `SKIP` 命中数=0。
- 实验卫生：每轮 L3 都在 `/tmp` 隔离库探测、用完删除；最终 `ponysentry_test` 库保留但无生产写入；生产库仅只读快照。

## 3. 分工契约与隔离有效性（Contract Isolation）

- 契约先行有效：`trace-fix-contract.md`（C1–C10 + v1.1–v1.4 amend）让 5 个成员在 4 小时内完成「红相→core→http→web→修复」，
  无一次接口对不上；`TraceRepository::update_trace_review` 等接口变更通过收据 §6 显式交接。
- 写域隔离：core / server / web 三写域并行时仅一处短暂编译互斥（E0716），无人越界写他人文件；
  测试文件全程只读（`git status tests/` 为空），Lead 是唯一改测试的人。
- 缺口：`crates/core/tests/**` 与 `crates/core/src/**` 同属 Executor 写域，导致「改语义顺手改断言」的诱惑存在；
  本轮靠 Lead 裁决 + L3 抽查挡住，但应在协议里把「core 单测」也纳入 Test Agent 冻结面。

## 4. 元协议迭代建议（Self-Evolving Protocol）

1. **红相任务上下文预算**：写 >300 行测试的任务，派发时要求「先落盘骨架 + 分片提交」，避免一次上下文耗尽重派。
2. **gate 收据必须显式 Exit Code**：禁止管道 `| tail` 决定退出码；收据模板固定 `cmd; echo EXIT=$?`。
3. **Roster 触顶的复用规则入协议**：优先复用已退役审核员执行新角色（保留上下文是资产），并记录角色切换。
4. **冻结测试与安全修复冲突**：定义「Lead 单方 amend + 重新冻结 + 显式 commit」为唯一合法路径（本轮已实践）。
5. **迁移类交付必带 down 与实测**：把「up→down→重放→再 up」写成门禁；无 down 的方言必须在契约文档登记。
