# Dev-Team 元框架复盘报告 (wave-1-web-ux: Web 控制台 UI/UX 改进)

## 1. 通信拓扑与信噪比（Topology & Noise）
- 三成员团队（Protocol/Test/Executor）+ Subagent 审查池：契约-测试-实施严格分权，跨角色共识全部落盘为文件
  （`.dev-team/web-ui-contract.md` 契约、`tests/web/test_web_ui_acceptance.py` 测试、review-*.json 审查），
  口头往返零散度低；Test Agent 主动上报"异常提前知悉"（含 executor 越界、reload 缺陷等环境事实）显著降低 Lead 侦察成本。
- 信噪比失分点：Test Agent 在长跑调试期多次长消息回执（含过程性轮次证据），压缩不足；
  Executor 在红相锚定前即开始写代码（12:21:59 修改 App.vue），属时序违例——靠红相浏览器证据仍有效才未污染门禁，
  但暴露"波次就绪信号"缺失：Lead 未在红相锚定 commit 后显式广播"可以开始"，Executor 以文件存在性自判就绪。
- 改进：波次状态机（pending→red-anchored→green）应由 Lead 显式广播，而非成员轮询文件存在性。

## 2. 门禁穿透与误杀率（Gate Penetration / False Negatives & Positives）
- 红绿双相真实拦截：红相 16 FAIL（旧版无 testid/无 role/内边距 0/无轮询等），绿相 26/26 OK，无伪造通过、无裸跑放行。
- L3 商业级审查真实拦截：L3-A 命中 2 条并发真缺陷（Refresh 全量 abort 误伤写请求、超时未覆盖响应体），
  L3-B 命中 3 条 a11y 真缺陷（焦点管理、冗余 aria-label、aria-labelledby）——三条修复后均以全量绿相 + 机器证据复验，
  L3-P 五问 PASS。门禁体系（测试分权 + 对抗审查 + 浏览器物理收据）首次在此项目完整跑通。
- 测试基建误杀（非产品缺陷）：agent-browser 0.27.0 的 `console --clear`/`unroute` 不可靠、`reload` 致 Vue 不挂载、
  长跑会话劣化（open 超时）——3 次为这些环境缺陷做了测试侧加固（fresh 会话隔离、offline 注入替代 route-abort、重试+skip-flaky），
  修复过程中 3 轮红绿迭代均定位为 TEST-DEFECT 而非污染门禁，账本清晰。
- 一个残留隐患：`selectIssue` 快速切换的 events GET 晚到竞态（wave 前已存在）未修，已在 NFR 报告"已知残留"如实记录。

## 3. 分工契约与隔离有效性（Contract Isolation）
- 契约先行有效：Protocol 冻结的 `web-ui-contract.md`（9 节、每条 REQ 编号）成为测试与实现共同真源，
  Test/Executor 双方均"以契约为准"对齐，跨角色零语义争执；NFR JSON 零字段变更（核对结论"已覆盖"而非强行塞字段）符合 Schema 纪律。
- 写域隔离基本有效（tests/、web/src/、.dev-team/ 三域正交），但 Executor 提前越界写 App.vue 一次（时序而非域冲突），
  经红相证据复核未污染门禁；后续 Executor 明确"测试与契约只读"执行到位。
- 效率观察：串行波次（T1→T2→T3）在等待红相锚定时 Test Agent 已完成全部侦察（服务健康、数据前置、会话冒烟），
  零空闲等待；绿相修复由 Lead 复核缺陷分类后精准派发单域修复，Executor 每轮 return 含机器收据（build exit / 用例数），
  复审闭环（L3A r1 FAIL → 修复 → r2 PASS）验证了"审查-修复-复审"循环有效性。

## 4. 元协议迭代建议（Self-Evolving Protocol）
- ① 波次就绪信号应以 Lead 显式消息为准（如"red-anchored, start implementation"），禁止成员以文件存在性自判越权开工。
- ② Test Agent 消息模板应含"结论/证据/待决策"三段压缩，长调试过程证据落文件不落消息（降低 Lead 阅读成本）。
- ③ agent-browser 类外部工具缺陷（unroute/reload/console-clear 不可靠）应尽早上报到团队上下文，
    让 Test Agent 首版就写对注入方式（offline 替代 route-abort），避免 3 轮红绿浪费。
- ④ 前端波次契约中"refreshScope 隔离"这类并发语义在契约阶段就应显式声明（本波由 L3-A 事后补出），
    可让 Protocol 契约初版即覆盖写/读请求的 abort 域划分。
- ⑤ 验收测试的"硬门禁 a"（Console=0）在含故意故障注入的套件内必须隔离会话判定（本次的最终形态），
    建议 test-expert 规范内置此模式，避免后续波次重复踩坑。