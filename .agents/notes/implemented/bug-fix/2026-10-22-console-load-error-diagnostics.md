# Agent Note: Console Load-Failure Diagnostics by Cause

Status: implemented

## Problem

控制台把 Issues、Traces、Projects 任一次数据请求失败都统一显示为「数据加载失败」。
生产曾出现 401 鉴权失败与真实服务故障同文案，排障必须先手工 curl 才能区分；
超时、断网和 5xx 也无法从页面直接判断。前次 token 内嵌修复见
`implemented/bug-fix/2026-10-21-console-data-load-failure-token-not-baked.md`。

## Decision

前端把加载失败按原因分类并展示可操作的第二行文案：

- `apiFetchJson` 对非 2xx 响应保留 `status`，401/403 标记为 `AuthError`。
- 新增纯函数 `web/src/lib/loadError.js`：`classifyLoadFailure` 区分 auth/timeout/network/server/unknown；
  `loadFailureDetail` 生成对应中文提示，auth 优先级最高。
- `App.vue` 用 `loadFailures` 记录各数据源失败原因，成功即清除对应失败；错误横幅仍在
  `data-testid="load-error"`，新增 `data-testid="load-error-detail"` 显示原因。
- 新增 `web/tests/load-error.test.js`（node:test）并在 CI 中执行 `pnpm test`。

## Alternatives considered

- **只保留 `console.error`**：开发者可见但用户/运维仍看到笼统文案，排障成本不降，否决。
- **直接把 HTTP 响应 body 显示在横幅**：可能泄露服务端内部信息，且错误格式不稳定，否决。
- **为每类错误新建独立横幅/页面**：违反现有 UI 契约“恰好一个 load-error”约束，过度设计，否决。
- **引入完整前端测试框架**：当前只有纯函数需要单测，Node 内置 `node:test` 零新增依赖足够，否决 Vitest 新依赖。

## Consequences

- 401/403 横幅显示「鉴权失败：控制台令牌缺失或已失效，请核对 ADMIN_TOKEN 并重新部署」。
- 超时显示请求超时，断网显示网络异常，5xx 显示服务端错误；未知错误仍回退为通用「数据加载失败」。
- 同一时刻多个数据源失败时按 auth > timeout > server > network 的优先级展示一个主要原因；各源独立成功后会清除自身失败。
- 失败横幅的既有 `data-testid` 与空态区分语义保持不变。
- 已知盲区（2026-10-23 实测）：前端 `API_TIMEOUT_MS = 3000` 与服务端 PostgreSQL
  `acquire_timeout = 3000ms` 相等，DB 不可达时浏览器先 abort、拿不到那条 3.003s 的 500，
  于是归因为 timeout（显示「网络不稳定」而非「服务端错误」）。见
  `implemented/bug-fix/2026-10-23-console-timeout-db-nodeport-retired.md`。
