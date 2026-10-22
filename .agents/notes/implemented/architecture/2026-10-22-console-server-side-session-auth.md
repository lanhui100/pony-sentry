# Agent Note: Console Server-Side Session Authentication

Status: implemented

## Problem

此前控制台把静态 `ADMIN_TOKEN` 烘焙进 Web 产物并随二进制分发，任何能打开控制台的人都能从
HTML/JS 中提取长期管理凭证。后续虽把安装二进制收紧为 `0750 root:dm`，但凭证仍在静态产物中，
只是提高了读取门槛，没有消除暴露面。需要一个不把长期 token 暴露给浏览器的控制台认证方案。

约束：控制台与 API 同源、单副本部署；静态 `ADMIN_TOKEN` 仍需兼容运维/API 客户端；
现有 R10 鉴权矩阵、PATCH 契约和 UI `data-testid` 不破坏；不引入外部 IdP。

## Decision

控制台采用服务端 opaque 会话 + HttpOnly Cookie，前端只保存短期 CSRF token：

1. `POST /api/v1/auth/session` 接收 `{"token":"..."}`，用常量时间比较校验
   `ADMIN_TOKEN`/`CLIENT_TOKEN`；成功后生成 256-bit 随机 opaque 会话 token，
   服务端内存保存 `{expires_at, csrf_token}`，响应设置
   `pony_sentry_session=<opaque>; HttpOnly; SameSite=Strict; Path=/; Max-Age=<ttl>`。
2. 会话默认 TTL 8 小时，`CONSOLE_SESSION_TTL_SECS` 可调（60–86400 秒）；
   `CONSOLE_COOKIE_SECURE=1` 时加 `Secure`。服务重启即清空所有会话。
3. `GET /api/v1/auth/session` 恢复会话状态和 CSRF token；
   `DELETE /api/v1/auth/session` 立即删除服务端会话并清 Cookie。
4. 管理路由鉴权顺序：静态 `x-admin-token`/Bearer 优先（API 客户端兼容），
   否则接受有效 Cookie 会话或 `Authorization: Bearer <session-token>`；Cookie 会话执行
   非安全方法（当前 PATCH）必须携带匹配的 `x-csrf-token`，否则 403 `csrf_invalid`。
5. 登录限速：同一来源 60 秒内 5 次失败后返回 429；登录成功清除窗口。
6. Web 控制台删除 `VITE_PONY_SENTRY_TOKEN` / localStorage token 逻辑，改为登录遮罩；
   数据请求使用同源 Cookie + 内存 CSRF header。401 时重新显示登录入口；
   页头提供退出按钮。错误横幅继续按原因提示。
7. 部署脚本不再用 `VITE_PONY_SENTRY_TOKEN` 构建；ADMIN_TOKEN 只经 stdin 传入部署后登录冒烟，
   并检查产物不含静态 token、Cookie 读接口 200、Cookie PATCH 缺 CSRF 为 403、注销后旧 Cookie 为 401。

## Alternatives considered

- **继续构建期内嵌 + 更短 TTL/轮换**：产物仍可被读取，token 轮换还要重建部署，未解决根因，否决。
- **HMAC/JWT stateless 短期令牌**：无需服务端存储，但无法即时注销、泄露后只能等过期，且需要额外密钥生命周期管理；当前单副本内存会话更简单且可撤销，否决。
- **浏览器 Basic Auth**：每次请求携带长期凭证且 UX/登出行为差，否决。
- **外接 OAuth/OIDC**：安全性更强但为单管理员内网控制台引入 IdP 运维成本，当前规模不划算，否决。
- **localStorage 存短期 token**：方便但 XSS 可读；HttpOnly Cookie + CSRF 方案暴露面更小，否决。

## Consequences

- 长期 `ADMIN_TOKEN` 不再出现在 Web 产物、二进制字符串或 localStorage 中；浏览器只持有 HttpOnly 会话 Cookie。
- 每个浏览器会话需要一次登录；会话过期或服务重启后重新登录。多标签页共享 Cookie，页面刷新通过 GET session 恢复。
- 会话为单进程内存，单副本约束不变；未来多副本需换共享存储（如 Redis/PG）并处理过期清理。
- 静态 API 客户端可继续使用 `x-admin-token`/Bearer；Cookie 会话的 PATCH 增加 CSRF 保护。
- `CONSOLE_COOKIE_SECURE` 默认关闭以适配当前 tailnet HTTP；HTTPS 暴露时必须置 1。
- 测试覆盖：session store 单测、`crates/server/tests/api_test.rs` 端到端 Cookie/CSRF/注销、
  `tests/api_admin_auth_contract.rs` 进程级契约、Web 登录/注销 E2E 与部署自检。
- 取代前序构建期内嵌方案：`implemented/bug-fix/2026-10-21-console-data-load-failure-token-not-baked.md`
  与 `implemented/process/2026-10-22-console-deploy-rollback-and-secret-hardening.md` 的 token 烘焙部分。
