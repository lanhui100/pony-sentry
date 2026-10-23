# Agent Note: 控制台读超时必须活过服务端连接池等待

Status: implemented

## Problem

2026-10-23 的事故复盘（`implemented/bug-fix/2026-10-23-console-timeout-db-nodeport-retired.md`）
暴露出一个诊断层的结构性缺陷：前端 `API_TIMEOUT_MS = 3000` 与服务端 PostgreSQL
`acquire_timeout(3000ms)` 相等。DB 不可达时服务端要 ~3.003s 才返回 500，浏览器在 3.000s 先
abort，`apiFetchJson` 把 `AbortError` 改写成 `TimeoutError`，`classifyLoadFailure` 归为 `timeout`
→ 页面显示「请求超时：服务响应过慢或网络不稳定」。真实原因（数据库连不上）被伪装成网络问题，
排障因此多花了 12 小时。

这不是「调大一个数字」，而是跨层不变量：**浏览器侧超时必须晚于它所等待的服务端内部等待链**。
同一处 3s 实测也让 1.5MB 的 traces 列表在慢链路上有假超时风险。

## Decision

1. 前端超时提为单一来源 `web/src/lib/apiTimeout.js` 的 `export const API_TIMEOUT_MS = 8000`；
   `App.vue` 删除本地常量改为 import（`apiFetchJson` 默认值、`/healthz` 探活共用），逻辑零改动。
2. 冻结契约 `.dev-team/contract-console-timeout.md`（R1–R8）：单一来源、下界
   `API_TIMEOUT_MS > acquire + 2000`（严格）、上界 ≤15000、真值取 `crates/**/*.rs` 内全部
   `acquire_timeout` 的 **max** 且缺失/畸形时 fail-closed、500 归因文案、最小闭包、对抗判据、
   R8 漂移哨兵（`A` 必须恰为 3000，任何一侧调值都必须重走把契约冻结）。
3. NFR 基准新增 `console_api_timeout_ms=8000`，与 `external_call_timeout_ms=3000` 分家：
   后者约束本服务**对外发起**的调用（webhook/通知），前者约束**入站**读请求在浏览器侧的等待。
4. Rust 侧只在 `crates/server/src/main.rs:152` 上方加一行指向契约的注释，不动数值。

验证（物理收据）：红相 `node --test` 23 用例中 4 红（apiTimeout.js 缺失 + App.vue 裸 3000），
实现后 23/23 绿、`cargo test --workspace` exit 0；浏览器混沌 A/B（同一 3.05s 延迟 500 现场）：
旧产物 → 「请求超时」，新产物 → 「服务端错误：PonySentry 返回 5xx」，两例 console error 均为 0；
生产 `deploy-console.sh` exit 0（healthz db up / 匿名 401 / tailnet 双门 / 登录 / CSRF 全过），
线上产物实测含 `8e3` 且三读接口 200。

## Alternatives considered

- **只调小服务端 `acquire_timeout`（如 1500ms）**：能让 500 早于前端 3s 到达，但池等待本身是
  资源保护，缩短后并发高峰更容易误杀正常请求；且不解决 traces 大响应在慢链路上的假超时。
- **只调大数值、不提常量**：值留在 SFC 内联，`node:test` 读不到 → 契约无法机械守卫，回归只能靠人眼。
- **加自动重试掩盖超时**：把一次 3s 变 9s，放大 DB 压力，且违反最小闭包；诊断问题不该用重试遮蔽。
- **强绑定常量 == NFR 基准 value**：改一个数字要同改常量/基准/契约三处，噪声大于收益；契约只保证
  区间（AMB-c 裁决，见契约 §5）。
- **换成 fetch 封装层统一注入超时**：属重构，超出本次最小闭包，且会把 AbortError 语义分散到新层。

## Consequences

- DB 不可达时页面在 ~3.05s 显示「服务端错误」，不再误报网络问题；慢链路读 1.5MB traces 获得 5s 余量。
- 前端最大等待 8s：单请求挂死时 UI 比过去多等 5s（上界由 R3 的 15000 封顶）。
- R8 是刻意警报：任一侧 `acquire_timeout` 变化都会让 `pnpm test` 非零退出，迫使重新做超时决策。
- 仓库跟踪的 `web/dist/index.html` 随本次同提交更新（历史惯例：每次 web 变更同步产物），
  避免「只 `cargo build` 不构建 web」时内嵌旧前端；`deploy/scripts/deploy-console.sh` 仍以源码构建为准。
- 事故记录的因果链仍以 `2026-10-23-console-timeout-db-nodeport-retired.md` 为家；本条目只记超时契约，
  跨层不复述。
- 审查闭环：L3-P 判 FAIL 2 条（跟踪产物未同步、注释导致行号锚点漂移）、L3-A 判 FAIL 3 条（R1 计数可
  凑、R8 只看 max、R5 的 git diff 恒空），均在本波内修复并用变异矩阵复验（37 用例，8 类变异全部非零
  退出）；L3-B 复核 PASS。审查记录见 `.dev-team/review-*-console-timeout.json` 与 `docs/release/
  wave-6-console-timeout/NFR-report.md`。
