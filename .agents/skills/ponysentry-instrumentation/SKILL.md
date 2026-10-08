---
name: ponysentry-instrumentation
description: 何时用：用户开发应用时需要接入错误/崩溃埋点（"给这个项目加埋点""上报错误到 sentry/ponysentry""接入崩溃收集""加 breadcrumbs""生成错误追踪 SDK 代码"）；或需要调试/查看已上报的错误数据（"看下有哪些 issue""查某个错误的堆栈""这个 bug 上报了吗""验证脱敏""认领 issue"）。任何需要 Sentry 风格遥测（错误分组、面包屑、用户上下文、release 标记、采样、脱敏）的场景，即使未明确说"sentry"。面向本项目 PonySentry 服务（/api/v1/ingest），覆盖 Rust/Tauri/Vue/FastAPI 多端。
---

# PonySentry Instrumentation

把 PonySentry 埋点按 Sentry 最佳实践接入目标端，并在 debug 时查看上报数据。

## 真相源
开工前先读（按需，缺一不可）：
- `docs/architecture/system-design.md` —— Ingest/指纹/生命周期契约（模块契约节）
- `crates/ingest/src/parser.rs` —— `RawEvent` 上报字段契约（platform/release/environment/message/exception/tags/extra/breadcrumbs）
- `crates/ingest/src/sanitizer.rs` —— 服务端零信任脱敏规则（客户端应先脱敏，服务端兜底）
- `deploy/k3s/app.yaml` —— 上报端点与鉴权（`CLIENT_TOKEN` 配置时强制校验）
- `references/` 下对应语言栈模板 + `references/api.md` 调试命令 + `references/checklist.md` 最佳实践清单

## 步骤

1. **确认上报端点与鉴权**（靠 review）：
   - 公网 ingest 端点：`https://sentry.ponyjob.top/api/v1/ingest`，Content-Type: `application/json`。
   - 鉴权机制：生产集群（k3s）已强制开启 `CLIENT_TOKEN`。未携带合法 Token 时直接返回 `401 Unauthorized ("Invalid or missing client token")`。
   - 环境变量与客户端配置规范：
     - **后端/桌面端（Node/Python/Rust/Tauri）**：从环境读取 `PONYSENTRY_CLIENT_TOKEN`，必须通过私有变量或构建注入注入 Token，请求附带 `X-Client-Token: <token>` 或 `Authorization: Bearer <token>`。
     - **纯浏览器前端（SPA 如 Vue/React）**：若需直连公网上报，需通过构建环境（如 `VITE_PONYSENTRY_CLIENT_TOKEN`）注入该 Token；或由同构后端/网关代理转发以防公网暴露。
   - 生产当前 Client Token 可通过 k8s secret `pony-sentry-secrets` 获取。

2. **识别目标端与入口文件**（靠 review）：
   - Rust 库/CLI：`src/main.rs` / `src/lib.rs` → 参考 `references/rust-tauri.md`
   - Tauri 桌面端：`src-tauri/src/main.rs` / `tauri.conf.json` → 参考 `references/rust-tauri.md`
   - Vue SPA：`src/main.js` / `src/App.vue` → 参考 `references/vue.md`
   - FastAPI：`app/main.py` → 参考 `references/fastapi.md`

3. **生成埋点接入代码**：按对应 `references/<lang>.md` 模板生成，必须包含（Sentry 最佳实践）：
   - **全局错误捕获**：Rust panic hook / Tauri `std::panic` + window error / Vue `errorHandler` + `unhandledrejection` / FastAPI 异常中间件；
   - **结构化 payload**：`platform`、`release`、`environment`、`exception`（error_type + value + stacktrace）；
   - **定位工作区（重要）**：必须在 `extra` 中注入 `project_path`（例如 Node/Python/Rust 服务端取当前工作区绝对路径 `process.cwd()` / `os.getcwd()` / `std::env::current_dir()`；前端取项目根目录或部署约定的工作区标识），确保服务端触发 Webhook 调度 DSH Headless 时，能精准在对应工作区拉起修复会话；
   - **breadcrumbs**：关键用户操作 / HTTP 请求 / 路由导航 / 数据库操作，`category` + `message` + `data`；
   - **user context**（可选，脱敏后）：仅存 id/role，禁止 username/email/password 等 PII；
   - **release 标记**：必须与部署版本一致（回归检测依赖该字段）；
   - **采样**（高流量端）：错误 100%，breadcrumbs 可降采样。

4. **客户端脱敏**（与 `sanitizer.rs` 对齐，零信任双保险）：
   - 上报前替换 message/exception/extra/breadcrumbs 中的 Token、密码、私钥、绝对路径用户名；
   - 若目标端已有统一脱敏 util，优先复用；否则用 `references/checklist.md` 的脱敏规则内联实现。

5. **本地验证（机械）**：用 `references/api.md` 中的 curl 命令模拟一次上报，确认 HTTP 200 且响应含 `fingerprint`/`issue_id`；检查响应 `status` 与 `count` 符合预期。

6. **debug 查看（机械）**：用 `references/api.md` 的查询命令：
   - 按 platform/status/release 列出 issues；
   - 查看单 issue 的 events（堆栈 + breadcrumbs + extra）；
   - 认领/标记 resolved 完成闭环；
   - 抽查脱敏是否生效（`payload` 中不应出现明文 Token/用户名路径）。

## 校准样例

- 正例：Rust 库崩溃，用户说"接入崩溃上报" → 在 `main.rs` 加 panic hook + 上报 payload，`release` 填当前版本，验证 curl 200。
- 反例：用户只改业务逻辑报错文案 → 不新增埋点，仅确认既有上报路径覆盖该异常（不重复埋）。
- 正例：Vue 路由跳转 404，用户说"加个面包屑看用户走到哪了" → 在 router `afterEach` 加 breadcrumb（category: navigation），不新建重复上报。
- 反例：用户说"把日志打印到控制台" → 不是埋点场景，不触发（本地 console 日志与遥测上报是两件事）。
- 正例：debug 查"windows 端 v1.2 版本还有哪些未解决错误" → `GET /api/v1/issues?platform=tauri&status=unresolved&release=v1.2`。
- 反例：查数据库表结构 → 不是埋点查看场景，不触发。

## 验证与报告

- 跑：`bash <skill>/references/api.md` 中的 curl 验证命令（或等价的 HTTP 客户端调用），得到 200 + `issue_id`。
- 报告格式（给用户）：
  - 接入文件清单（路径 + 改动点）
  - 已遵循的最佳实践条目（引用 checklist 编号）
  - 验证结果：curl 响应 `status`/`count`/`fingerprint` + 脱敏抽查结论
  - debug 查询结果摘要（issues 数、状态分布）
