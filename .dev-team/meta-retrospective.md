# Wave-9 元框架复盘（Meta-Retrospective）

> 任务：PonySentry 服务端 issue 治理与分析接口（B 级开发任务）
> 周期：2026-10-25 18:48 至 20:30
> 参与角色：Lead（调度/裁决/交付）、Protocol Agent（契约与空桩）、Test Agent & Test Agent 2（黑盒常规验收）、L2-AT（破坏性对抗测试）、Executor-core（数据层与迁移）、Executor-server（服务与路由层）

---

## 一、通信拓扑与信噪比

本次 Wave-9 开发全面遵循了 Agent Team 的状态机与通信拓扑公理：
- **拓扑严谨度**：严格贯彻了“持久有状态者走 Teammate、无状态单次局部求值走 Subagent”的映射公理。在 Roster 预算内派生 Protocol Agent、Test Agent 与 Executors（常驻成员 ≤ 6，严格受控于 Roster ≤ 8 上限）。
- **通信收敛与信噪比**：
  1. 放弃了盲目短轮询，充分依靠 `wait_agent` 的事件驱动机制等待结果。
  2. Protocol Agent、Test Agent 与 Executor 均采用高度结构化的纯事实 JSON 或紧凑型清单进行汇报，彻底消除了自然语言外壳与过程性废话。
  3. 审查阶段采用严格的 `dev-review-lint.py` JSON Schema 规范，在 Gate-1 阶段精确捕获了 10 个 BLOCKING 缺陷，并在收口汇聚阶段生成标准收据 `review-join-wave9.json`，信噪比极高。
- **Token 消耗与重复命令率**：
  在前期，第一代 Test Agent 与 L2-AT Subagent 一度因尝试整读 2600 行的 `routes.rs` 和 2100 行的 `sqlite_repository.rs` 而耗尽上下文。Lead 及时识别该反模式，实施“单次读取 ≤ 120 行、必须以 grep/sed 精准切片”的硬预算约束，换用第二棒后顺利在极小 Token 成本内产出了 1925 行的常规用例和 454 行的对抗用例。

---

## 二、门禁穿透与误杀率

- **红相拦截有效性**：
  常规验收套件（`tests/acceptance_issue_governance.rs` 12 个用例）和对抗破坏套件（`tests/acceptance_issue_governance_adversarial.rs` 9 个用例）在空桩阶段均严格产生了原生非零退出码（`EXIT_CODE=101`），并通过了 `dev-red-purity.py` 的机械校验（Exit Code 0）。真实拦截了新路由 404、参数静默吞没、游标分页缺失等未实现逻辑，坚决杜绝了“假绿”与“无断言测试”。
- **并发与锁升级穿透防线**：
  L2-AT 编写的确定性并发用例 `concurrent_duplicate_ingest_is_exactly_once`（8 线程 × 5 轮高并发同 event_id）成功击穿了既有代码中 SQLite 默认 deferred 事务在并发读写下的锁升级漏洞（触发 `SQLITE_BUSY: database is locked` 500）。由于红相硬约束，Executor-core 不得不重构了底层事务逻辑，引入 `BEGIN IMMEDIATE`、连接级 `busy_timeout=5000` 以及整事务有界退避重试，将这一深层隐患彻底根治在交付前。
- **门禁误杀与修复**：
  1. 部署脚本 `deploy-console.sh` 原先硬编码要求 `/api/v1/traces?limit=100` 返回 200，而在生产配置 `PONY_TRACE_INGEST_ENABLED=0`（停收开关开启）下实际返回 410，导致部署脚本自检误杀并回滚。Lead 审计后修补了自检脚本对停收配置的动态适配。
  2. 既有工作区单测 `test_sql_injection_is_safely_parametrized` 包含旧行为（负数分页参数静默钳制返回 200）的过时断言，与本次契约 B4（负数显式返回 400）冲突。经 Lead 授权单断言同步修正后，22 套件全绿通过。

---

## 三、分工契约与隔离有效性

- **严格分权与职责正交**：
  - 彻底斩断了实施者“既写代码又写测试”的作弊链条。黑盒契约用例全部位于独立的 `tests/` 包，完全由 Test Agent 与 L2-AT 依据冻结契约独立产出，Executor 全程只读。
  - Executor-core 独占 `migrations/` 与 `crates/core/src/` 写域；Executor-server 独占 `crates/server/src/` 写域，二者写域严格正交。
- **契约演进与版本化闭环**：
  - 需求与设计由 Lead 决策，Protocol Agent 独占负责将契约落盘（v1.0）。
  - 在 Gate-1 阶段，方案对抗审查员提出了关于 `archive_restored_at` 留痕、活跃态自动解档、批量归档分桶口径、游标精度丢失等 10 项 BLOCKING 缺陷，Protocol Agent 迅速推进完成 v1.1、v1.2、v1.3 三轮增量 amend 提交，保证了实施者与测试者拥有绝对唯一、自洽的真源。
- **物理账本与断点恢复**：
  所有的契约冻结、红相基线、实现提交均被打上了精确的 Git Commit 和 Tag 检查点（`wave-9-protocol`、`wave-9-protocol-v11`、`wave-9-red` 等），`.dev-team/state.json` 实时维护全量波次账本。

---

## 四、元协议迭代建议

1. **大文件侦察熔断机制**：
   对于超过 1000 行的大型单文件（如 `routes.rs`），Subagent 在执行代码阅读前应默认增加静态行数检查；若直接使用未带 offset/limit 的 read，框架层或 Prompt 应强制告警或拦截，避免由于上下文超限发生 Subagent 中途静默阵亡。
2. **多方言存储的游标基准规范化**：
   在 SQLite 与 PostgreSQL 混用架构中，时间戳列的存储格式存在微秒与 RFC3339 纳秒文本的差异。未来契约设计应将“游标必须采用库内原始序列化文本”作为通用模式纳入 `dev-team` 模板，避免重复排查微秒截断导致的漏行问题。
3. **部署脚本与功能开关联动校验**：
   部署脚本在执行端点自检时，应当从环境配置中动态解析功能开关状态（如停收开关、信任开关），自适应调整断言期望码，避免因脚本期望僵化阻断正常的发布流水线。
