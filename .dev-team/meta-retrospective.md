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

---

# wave-4 元框架复盘（systemone Sentry 上游错误过滤）

## 1. 通信拓扑与信噪比（Topology & Noise）
- 三成员团队（Protocol/Test/Executor）+ 1 路 L3 靶向审查 + Lead 侦察闭环：契约（ADR）、红相证据
  （`red-phase-systemone-filter.txt`）、审查（`review-systemone-filter.json`）全部落盘为机器可核文件，
  口头往返极少；跨仓（pony-sentry ↔ ponymll）通过文件共享与显式消息完成，无信息过载。
- 信噪比失分点：本波出现一次 **Executor 在红相锚定前抢先实现**（红相窗口未开即改 systemone.rs），
  违反"红相先锚定"纪律——靠 Lead 协调 stash/pop 双窗口纠正，红相 E0432 物理证据依然真实成立，未污染门禁。
  这与 wave-1 复盘第 ① 条（波次就绪信号缺失）同根：成员以契约/文件存在性自判就绪，而非等待 Lead 显式窗口信号。
- 改进：本次 Lead 显式广播了"stash 信号/恢复信号"，纠正有效；但**预防**仍不到位——应在契约冻结时就显式声明
  "红相窗口=stash 后、绿相窗口=pop 后"，并把 stash/pop 也收归 Lead 唯一写主。

## 2. 门禁穿透与误杀率（Gate Penetration / False Negatives & Positives）
- 红绿双相真实拦截：红相 E0432（函数不存在）Exit 101 → 绿相 12/12 Exit 0，零假测试；
  2 条路由级集成用例走真实注入接缝（`AppState::sentry` 替换为指向本地 mock ingest 的 `SentryClient`），
  429/文本兜底断言 0 条 ingest 事件 + 干净 500 正控断言恰 1 条——非真空，杜绝假测试。
- L3 靶向审查 PASS（真值表 9 kind × 标记全枚举、De Morgan 等价核验、NoAvailableKey 折叠链、反例穷举无命中），
  机器 JSON 校验（jq Exit 0）通过。
- 回归：既有 `systemone_tests.rs` 3/3 全绿；`cargo check -p ponyllm-server` Exit 0；双仓 pre-commit 门禁全部 PASS。
- 误杀率：本波无测试基建误杀；审查未误报（无被拒的真实缺陷被放行，也无超纲误判）。

## 3. 分工契约与隔离有效性（Contract Isolation）
- 契约先行有效：Protocol 冻结的 ADR（判定语义表 + 边界）成为唯一真源；v2 可见性修订
  （`pub(crate)`→`pub`，因集成测试独立 crate 绿相必然 E0603）由 Test Agent 主动阻断、Protocol 修订、
  契约档 task-4 重新冻结——契约演进门禁完整走通（Lead 一票 + 重锚）。
- 写域隔离有效：`systemone.rs`（Executor）↔ `tests/systemone_sentry_filter_tests.rs`（Test）↔
  `.agents/notes` + `.dev-team`（Protocol/Lead）三域正交；唯一违规是 Executor 红相前抢先写 src，经 stash/pop 纠正。
- 效率观察：串行（T1→T2→T3）下 Test Agent 主动阻断 E0603 契约缺陷、红相窗口零等待；
  Executor 每轮回执含物理 Exit Code，Lead 复核成本低。

## 4. 元协议迭代建议（Self-Evolving Protocol）
- ① **红/绿窗口由 Lead 显式广播 + 收归 git 写主**：契约冻结后 Lead 必须显式发送
  "红相窗口开启（已 stash）/绿相窗口开启（可 pop）"双向信号，成员严禁以"契约/函数已存在"自判开工；
  本波 stash/pop 也应由 Lead 亲自执行而非委托（git 写操作唯一写主原则的延伸）。
- ② **跨仓任务的写域审计**：任务板 write_scopes 不支持 `../` 跨仓路径，advisory 机制对跨仓文件失明——
  建议把跨仓真实写域写入任务 subject/description 并配 Lead 机械核验（`git -C <repo> diff --name-only`），
  本波已如此执行，可沉淀为 dev-team skill 的跨仓模板。
- ③ **契约可见性 v1→v2 的教训**：集成测试（`tests/*.rs` 独立 crate）只能访问 `pub` 项——
  Protocol 冻结契约时应首查"测试引用路径的可达性"，而非等 Test Agent 红相期才暴露 E0603；
  建议契约模板内置"测试可见性"检查项。