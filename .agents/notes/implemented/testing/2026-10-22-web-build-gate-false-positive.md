# Agent Note: Web Build Gate Must Not False-Pass on pnpm Filter No-Match

Status: implemented

## Problem

`tests/web/test_web_contract.py` 的构建门禁在 `web/` 目录下执行
`pnpm --filter pony-sentry-web run build`。实测该命令输出 `No projects matched the filters`
却返回退出码 0，测试只检查 returncode，因此即使源码已更新、`web/dist` 仍是旧产物也会通过。
本次新增错误诊断文案后，该假绿直接把旧 dist 当成了合格产物。

## Decision

- 测试改为在 `web/` 下执行 `pnpm run build`，并断言输出包含 Vite 的 `built in`。
- 新增 dist 内容门禁：必须包含最新的鉴权失败诊断文案，且不得出现
  `x-admin-token` 后跟 64 位 hex 的构建期内嵌 token 形态。
- CI 的 `Test & Build Web Frontend` 继续执行 `pnpm test && pnpm run build`；
  构建门禁与真实部署使用同一个包脚本，不再经过无匹配也会成功的 filter。

## Alternatives considered

- **保留 filter，只检查 dist mtime**：mtime 不是 CI fetch 后的可靠信号，且仍会掩盖 no-match，否决。
- **只检查 `web/dist/index.html` 是否存在**：旧文件也会满足，无法证明本次源码被构建，否决。
- **在 CI 单独再跑一次 build 覆盖测试中的假绿**：会掩盖测试自身缺陷，仍留下本地假绿，否决。

## Consequences

- Web 构建门禁现在有真实副作用与内容断言；no-match 会以非零退出或缺少 `built in` 失败。
- 仓库提交的 `web/dist/index.html` 必须与 `web/src` 同步，由该测试机械兜底。
- 与前端错误诊断决策见 `implemented/bug-fix/2026-10-22-console-load-error-diagnostics.md`。
- 控制台认证已演进为服务端 HttpOnly 会话；dist 无内嵌 token 的断言继续保留，
  且新增登录/注销 E2E。见 `implemented/architecture/2026-10-22-console-server-side-session-auth.md`。
