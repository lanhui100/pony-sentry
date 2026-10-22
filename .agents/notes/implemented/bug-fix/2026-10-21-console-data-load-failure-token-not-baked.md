# Agent Note: Console "数据加载失败" Root Cause and Build-Time Token Baking

Status: implemented

## Problem

生产控制台（systemd `pony-sentry`，tailnet `devserver.taildb165c.ts.net:3000`）出现
红色横幅「数据加载失败」。现场取证（2026-10-21 20:00 实测）：

- `GET /healthz` → `200 {"db":"up","status":"ok"}`，服务与 DB 正常；
- `GET /api/v1/issues`、`GET /api/v1/traces?limit=100`、`GET /api/v1/projects`
  不带任何凭证 → **`401 {"error":"unauthorized"}`**（响应 <1ms）；
- 同上带环境文件 `/etc/pony-sentry/pony-sentry.env` 中已配置的 `ADMIN_TOKEN` → 全部 `200`（2–10ms）；
- `GET /` 提供的 `index.html` 与仓库 `web/dist/index.html` 同字节（343603），
  且**不含任何 64 位十六进制 token**——`VITE_PONY_SENTRY_TOKEN` 未烘焙，产物无构建期凭证。

根因：`web/src/App.vue` 的 `apiFetchJson` 仅在
`VITE_PONY_SENTRY_TOKEN || localStorage.pony_sentry_token` 非空时注入
`x-admin-token`（`App.vue:902-908`、`authHeaders`）。构建产物两者皆空 → 三个读接口
全部 401 → `issueLoadFailed/traceLoadFailed/projectLoadFailed` 三标志同时置位
（`App.vue:889`）→ 横幅「数据加载失败」。网关徽标显示"就绪"是因为 `/healthz`
不需鉴权，与数据横幅并存正指向鉴权而非服务故障。
（R10 鉴权矩阵与"构建期/运行时二选一"机制的引入见
`implemented/feature/2026-10-21-multienv-issue-dimensions-and-ingest-idempotency.md`。）

时序：`15:41` 提交 57f26de 落地 R10 读接口鉴权；`15:43` env 写入 `ADMIN_TOKEN`；
`15:55/15:56` 重建 dist 并重装二进制——**构建时未设置 `VITE_PONY_SENTRY_TOKEN`**，
产物从未携带控制台令牌。

## Decision

采用 README 38-45 行「方式 A（构建期，产物内嵌，适合内网自建 console）」：
构建 web 时注入 `VITE_PONY_SENTRY_TOKEN=<ADMIN_TOKEN>`，生成的 `index.html` 经
`include_str!` 嵌入服务端二进制（`crates/server/src/routes.rs:368`），运行时代码
`import.meta.env.VITE_PONY_SENTRY_TOKEN` 取到构建期 token，无需浏览器
localStorage 干预。配套落地（本次提交内完成）：

1. **幂等部署脚本** `deploy/scripts/deploy-console.sh`：从
   `/etc/pony-sentry/pony-sentry.env` 读取 `ADMIN_TOKEN`（不落库不入 git）→
   `cd web && VITE_PONY_SENTRY_TOKEN=$ADMIN_TOKEN pnpm build` →
   `cargo build --release -p pony-sentry-server` → 安装 `/usr/local/bin/pony-sentry-server`
   → `systemctl restart pony-sentry` → 自检（healthz + 带 token 的读接口冒烟）。
2. **运行时不依赖 localStorage**：产物内嵌 token，浏览器直接使用；
   localStorage 回退仍保留（方式 B 兼容，未删除——README 二选一机制继续有效）。
3. **仓库无泄露**：`web/dist/index.html` 仍随源码提交，但构建的"带 token 版本"
   仅存在于部署机编译产物中；脚本在构建后把工作区 `web/dist` 还原为无 token
   版本（`git checkout -- web/dist`），保持 CI 密钥扫描（`ci.yml` secret-leak
   scan）与仓库卫生要求。部署机二进制内嵌 token 属既有的内网自建语义。

## Alternatives considered

- **A. 方式 B（运行时 localStorage，浏览器执行 `localStorage.setItem('pony_sentry_token', …)`）**
  —— 立即生效、零重建，但每个浏览器/设备都要手工执行一次，token 流转在
  人–浏览器通道，别名"先恢复可用"，属临时补丁；用户明确否决（"不补丁式修复"）。
- **B. 方式 A（构建期烘焙，本次采用）** —— 产物内嵌 token，部署即恢复，
  后端已存在的鉴权矩阵（R10）不变，暴露面不扩大（tailnet 内网 console）；
  代价是每次轮换 token 需重新构建/重装二进制，由部署脚本收敛为一条命令。
- **C. 前端对 401 做软提示（"令牌缺失/无效"替代"数据加载失败"）** —— 改善
  可诊断性，但不恢复数据加载，治标不治本；可作为后续增强，不在本次混淆目标。
- **D. 关闭读接口鉴权（回退到无 token 时代）** —— 直接违背 R10 契约与
  commit 57f26de 的安全目标，否决。

## Consequences

- 部署动作收敛为 `sudo bash deploy/scripts/deploy-console.sh`（幂等，可重复执行；
  token 轮换时重跑一次即整体更新）。
- 浏览器端无需任何手工设置；旧 localStorage 值若有残留，仍优先被构建期
  `VITE_PONY_SENTRY_TOKEN` 覆盖（`App.vue:904` 优先取构建期值）。
- 若未来迁移到公网暴露 console，需重新评估 token 内嵌在静态产物中的暴露面
  （届时应回退到服务端登录会话或短期令牌）。
- 后续部署加固见 `implemented/process/2026-10-22-console-deploy-rollback-and-secret-hardening.md`；
  可诊断错误文案见 `implemented/bug-fix/2026-10-22-console-load-error-diagnostics.md`。
