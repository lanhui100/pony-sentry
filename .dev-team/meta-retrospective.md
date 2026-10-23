# 元框架复盘：wave-6 / 控制台读超时契约

> 复盘对象是**协作元框架与组织协议本身**，不是业务代码变量命名。上一轮（wave-5）复盘已归档为
> `.dev-team/meta-retrospective-wave5.md`。

## 1. 通信拓扑与信噪比（Topology & Noise）

- 本轮名册：Lead（编排+裁决+同提交文档）→ Protocol Agent（契约 ver1→ver4、NFR 基准）→
  Test Agent（红相 23 用例 → 加固 37 用例）→ Executor（最小闭包 3 处）→ Subagent 池
  （L3-A 契约/测试真实性、L3-B 覆盖与等价性、L3-P 五问）。常驻 4（≤8），审查不占槽位。
- 信噪比最高的是**外部视角推翻"全绿即安全"**：37 条测试全绿时，L3-A 仍用三条变形（死引用凑数、
  单点下调 2500、auth/timeout 对调）证明旧判据可绕过；L3-P 则用 `git show HEAD:web/dist` 证明
  跟踪产物未同步、用 `grep -n acquire_timeout` 证明行号锚点漂移。三次都是「机器绿了但语义有洞」。
- 噪声与浪费：① L3-A 首轮耗时 ~13 分钟（要求在 clean clone 里做变异，复制开销大），期间 Lead 空等；
  ② 我把 L3-B 与 Test Agent 并行派出，L3-B 审到的是加固前快照（23 用例），其 PASS 不能覆盖加固后
  语义——需要一次人工对账（结论是 L3-A 已覆盖同一面）。
- 令牌产出比：最省的是把「契约含糊点」变成三条显式裁决（AMB-a/b/c），每条两行、零返工；
  最贵的是 L3-A 的 clean clone 变异（每轮 cp + 跑测），本可给它一个预置 worktree 直接复用。

## 2. 门禁穿透与误杀率（Gate Penetration / False Negatives & Positives）

- **真实拦截**：L3-P 2 条（产物未同提交 → 干净 checkout 内嵌旧前端；注释插入导致 `main.rs:152`
  锚点漂移）；L3-A 3 条（R1 计数可凑、R8 只看 max、R5 git diff 恒空）。5 条全部有可复现反例，
  Lead 逐条复核后修复：产物同步 + 注释下移 + 测试改结构判据（37 用例，变异矩阵四类全拦）。
- **误杀/过度声明**：0 条。L3-P 判「dist 未同步」时，Lead 本就计划在 release commit 同步——但审查在
  release 前给出 FAIL 属正确时序，不算误杀。
- **假放行风险点（本轮新发现）**：测试的结构性弱点不会被测试自己发现。三条穿透的共同形态是
  **判据只验证了代理指标**（出现次数、max、git diff），而非被约束的真实结构。加固后每条判据都直接
  读结构（定时器实参、逐站点值、分支顺序），并配合成反例单测。
- **物理凭证纪律**：红相 exit=1、绿相 37/37、变异矩阵 5 类非零退出、混沌 A/B（旧→「请求超时」/
  新→「服务端错误」）、部署自检 exit=0，全部落盘 `.dev-team/e2e-console-timeout.txt`。
- **一次自造混乱**：E2E 准备期我误以为 `web/dist` 已是新产物，实际它与旧构建**字节等长**
  （`3e3`→`8e3` 同长度），只有内容不同；靠 sha256 + 内容 grep 才定性。教训：产物类事实必须以内容
  哈希为准，不得以体积/mtime/git status 推断。

## 3. 分工契约与隔离有效性（Contract Isolation）

- 写域零冲突：Protocol（`.dev-team/` 两文件）、Test（`web/tests/` 三文件）、Executor
  （`web/src/lib`、`web/src/App.vue`、`crates/server/src/main.rs`）、Lead（docs/README/ADR/state/
  review JSON/部署）。Executor 额外跑过一次 `npx vite build` 改写被跟踪的 `web/dist`，已自查还原并
  主动申报——本轮唯一越界，靠「执行者自报 + L3-P 复核」闭环，未逃逸。
- 测试分权有效：Executor 全程未碰 `web/tests/**`；加固由 Test Agent 完成，Lead 只验证不代写。
- 信息下发：契约含糊点（AMB-a/b/c）集中由 Protocol 提报、Lead 裁决、回写正文，避免了 wave-5 那种
  「并行派发 + 裁决滞后」的返工。

## 4. 元协议迭代建议（Self-Evolving Protocol）

1. **审查快照锚定**：`review-*.json` 增加必填字段 `reviewed_commit`，并规定「测试冻结 commit 之后
   才允许派发 L3」，否则审查结论不可比（本轮 L3-B 审 23 用例、L3-A 审 37 用例）。
2. **变异矩阵作为 PASS 必要条件**：凡以测试为门禁的波次，审查者必须自跑 ≥3 类变异并贴退出码，
   不接受"读过测试觉得没问题"。
3. **产物同源进入 Phase 4 清单**：凡仓库跟踪构建产物（如 `web/dist`），收口前必须机械比对
   `git show HEAD:<artifact>` 与源码语义（本轮全靠 L3-P 才发现）。
4. **审查者预置 worktree**：Lead 派发前准备 `git worktree`/副本路径，禁止审查者自行 clean clone。
5. **行号锚点治理**：契约/注释引用源码行号时必须与被引用符号同行，或改用符号名——本轮
   「插一行注释导致锚点漂移」可预防，应写进契约模板检查项。
