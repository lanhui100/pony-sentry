# 控制台超时契约波次 —— NFR 达标报告 (wave-6-console-timeout)

> 交付资产归档：commit `c1c942a`（实现）+ `93de327`（L3-P 复审修复）的 NFR 基准逐项证据。
> 基准文件：`.dev-team/nfr-baseline.json`（新增 `console_api_timeout_ms`，既有 6 个顶层键零改动）。
> 契约：`.dev-team/contract-console-timeout.md`（FROZEN ver 3，R1–R8）。

## 逐项对照（基准字段 vs 代码行证据 vs 判定）

| # | 基准字段 | 实现/收据证据 | 判定 |
| :-- | :-- | :-- | :-- |
| 1 | `external_call_timeout_ms: 3000` | 本次不改对外调用超时（webhook/通知路径未触碰）。控制台**入站**读超时拆分为 `console_api_timeout_ms: 8000`：`web/src/lib/apiTimeout.js:6` 单一来源，`web/src/App.vue:917` import、`:1040` apiFetchJson 默认值、`:1233` healthz 探活共用；下界/上界由 `web/tests/api-timeout-contract.test.js` 机械守卫 | **达标（边界分离有据）** |
| 2 | `console_api_timeout_ms: {value: 8000, min_margin_over_acquire_ms: 2000, max_ms: 15000}`（本波新增） | 严格下界 `8000 > 3000 + 2000`；上界 8000 ≤ 15000；`crates/**/*.rs` 两处 `acquire_timeout` 均 3000（`crates/server/src/main.rs:152`、`crates/core/src/db.rs:9`）。变异矩阵：改 3000/20000 → 非零退出 | **达标** |
| 3 | `retry: {max_attempts: 3, backoff_ms: 500}` | 本波次**不引入**任何自动重试（契约 R6 最小闭包）；错误仍由横幅 + 人工「刷新」驱动 | **达标（保持现状）** |
| 4 | `concurrency_lock: optimistic_or_mutex` | `refreshControllers`（`App.vue:1037`）在 `abortRefreshFetches()`（`:1099-1101`）中止上一轮刷新，迟到响应不能覆盖新状态；超时窗口由 3s→8s 不改变该机制（abort 由新一轮刷新触发，而非定时器） | **达标** |
| 5 | `logging: {format: json, trace_id: request_id}` | 前端无新增日志路径；`apiFetchJson` 的 catch（`:1075-1092`）仍区分 TimeoutError / AbortError / 其它，未新增裸吞；服务端 tracing 未改动 | **达标（前端降级路径，non-machine）** |
| 6 | `resource_release: [abort_controllers, event_listeners, background_timers]` | 超时定时器在 `finally` 清除（`:1090`、`:1242`）；healthz AbortController 在 finally 置空（`:1247`）；卸载时 `clearInterval(healthTimer)` / abort 全部 controller（`:1738-1744`） | **达标** |
| 7 | 配置与自检 | 超时单一来源可被 `pnpm test` 机器校验（R1）；部署自检含 `/healthz` 的 `db:up` 断言（`deploy/scripts/deploy-console.sh:169-171`），本次部署实测通过 | **达标** |

## 物理验收（Tier-1 真实环境）

- **测试**：红相 `exit=1`（23 用例 4 红）→ 绿相 `23/23 pass`；`cargo test --workspace` exit 0。
- **变异矩阵**（`/tmp/wave6-mut`，HEAD worktree）：基线 23/0 → apiTimeout=3000 失败 / =20000 失败 /
  `db.rs=6000` 失败 / `from_millis(abc)` 失败 / 删除两处来源失败。详见
  `.dev-team/e2e-console-timeout.txt §5`。
- **浏览器混沌 A/B**（同一 3.05s 延迟 500 现场）：旧产物 →「请求超时」；新产物 →「服务端错误：PonySentry 返回 5xx」；
  两例 `agent-browser errors` 为空。截图：`old-frontend-timeout.png` / `new-frontend-server-error.png`。
- **生产部署**：`bash deploy/scripts/deploy-console.sh` exit 0；自检 healthz `db:up`、匿名读 401、
  tailnet 双门（loopback+伪造 Host 401 / tailnet IP+白名单 Host 200）、登录换 Cookie、CSRF 403、
  产物无 ADMIN_TOKEN 全通过；线上产物含 `8e3`，三读接口 200（2.5/20.7/3.3ms）。

## 审查记录

| 审查 | 结论 | 落盘 |
| :-- | :-- | :-- |
| L3-P 生产就绪五问（首轮） | **FAIL**：① 跟踪产物 `web/dist` 未同提交（干净 checkout 会内嵌 3000ms）；② `main.rs` 注释插入使契约锚点 `:152` 漂移到 `:153` | `.dev-team/review-l3p-console-timeout.json` |
| L3-P 复审修复 | 两条均 ACCEPTED_FIXED（`93de327`：dist 同步 + 注释下移复原锚点） | `.dev-team/review-l3p-adjudication-wave6.json` |
| L3-A 契约覆盖/测试真实性 | **FAIL**：R1 计数可被死引用凑数、R8 只看 max（单点下调 2500 放行）、R5 的 `git diff` 判据恒真空 | `.dev-team/review-l3a-console-timeout.json` |
| L3-A 复审修复 | 三条均 ACCEPTED_FIXED（`981b669`）：判据改结构判据，37 用例；变异矩阵复验全部非零退出 | `.dev-team/review-l3a-adjudication-wave6.json` |
| L3-B 覆盖完备性/实现等价性/变异 | **PASS**（另记 2 条非阻断观察：R6 语义判据靠 review；R1 行号锚点已并入 ver 4 修订） | `.dev-team/review-l3b-console-timeout.json` |

## 已知残留（如实记录）

- `console_api_timeout_ms=8000` 使单请求挂死时 UI 最多多等 5s（上界由 R3 的 15000 收口）。
- R8（`A ≡ 3000`）是刻意漂移哨兵：任一侧 `acquire_timeout` 调值都会让 `pnpm test` 非零退出，
  须重走契约冻结；这不是误杀。
- `db.rs`（SQLite 默认路径）与 `main.rs`（Postgres）各自硬编码 3000：真值取两者 max，但两处仍可能
  在无人察觉时分化——R2 的下界只保证前端大于 max，不保证两者相等。
