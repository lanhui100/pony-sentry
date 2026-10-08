# Sentry 最佳实践清单（PonySentry 落地映射）

接入埋点时逐条核对；接入完成后把命中的条目编号写入报告。

## A. 错误捕获完整性（Sentry 核心）

- [ ] **A1 全局兜底**：语言/框架级未捕获错误全接入（panic hook / unhandledrejection / errorHandler / 异常中间件），不留裸奔路径。
- [ ] **A2 显式上报平衡**：关键失败路径手动上报（带业务上下文），但成功路径不上报（去噪）。
- [ ] **A3 不吞异常**：`catch (e) {}` / `except: pass` 裸吞异常禁止——吞了也要加 breadcrumb 留痕。

## B. 分组与去重（fingerprint 契约）

- [ ] **B1 干净 error_type**：上报 `error_type` 用稳定错误类名（如 `JobNotFoundError`），不要塞动态消息。
- [ ] **B2 稳定栈顶帧**：`stacktrace` 首帧用真实出错函数名（PonySentry 用它 + error_type 计算指纹；行号/列号已自动剥离）。
- [ ] **B3 不手工指定 fingerprint**：PonySentry 自动分组，客户端只需保证 B1/B2 稳定。

## C. 面包屑（breadcrumbs）

- [ ] **C1 关键轨迹**：导航/HTTP 请求/DB 操作/用户表单提交，`category` + `message` + 脱敏后的 `data`。
- [ ] **C2 环形上限**：队列上限 64 条（超出淘汰最旧，防内存膨胀）。
- [ ] **C3 附随上报**：错误发生时把面包屑一并送上（Sentry 标准行为），上报后清空。

## D. 上下文（user / release / environment）

- [ ] **D1 user context 最小化**：仅 id/role 等非 PII，**禁** username/email/phone/password。
- [ ] **D2 release 必须对齐**：`release` 填部署版本（`env!("CARGO_PKG_VERSION")` / `__version__` / `VITE_APP_RELEASE`），否则 Regression 自动检测失效。
- [ ] **D3 environment 三态**：`production` / `staging` / `dev`，禁止漏填或自定义混乱值。环境变量的值必须与部署环境一致，凭 review 判断。

## E. 脱敏（零信任，客户端先做一层，服务端兜底）

- [ ] **E1 敏感键黑名单**：token/password/secret/api_key/access_token/authorization/cookie/private_key 键值一律 `[REDACTED_SECRET]`。
- [ ] **E2 递归清洗**：`extra`/`breadcrumbs`/`tags` 全做深度递归脱敏，深度上限 32。
- [ ] **E3 路径脱敏**：`/home/<user>`、`/Users/<user>`、`C:\Users\<user>` → `[USER_HOME]`（多端正则一致）。
- [ ] **E4 凭据格式**：Bearer/Basic/JWT/PEM 私钥块整体替换。
- [ ] **E5 服务端兜底**：即使客户端漏了，PonySentry `sanitizer.rs` 也做同样清洗——双保险，但客户端别依赖兜底。

## F. 采样与性能

- [ ] **F1 错误 100%**：异常/崩溃类必须全量上报（采样只用于面包屑/性能类）。
- [ ] **F2 外部超时**：上报 HTTP 客户端硬超时 ≤3s（与 NFR baseline `external_call_timeout_ms` 对齐）。
- [ ] **F3 fire-and-forget**：上报不阻塞业务主流程（spawn / create_task / keepalive fetch）。

## G. 调试闭环（Agent 协作）

- [ ] **G1 可查性**：上报后可通过 `/api/v1/issues` 按 platform/status/release 检索。
- [ ] **G2 认领与解决**：Agent 用 `PATCH /api/v1/issues/{id}` 完成 `in_progress → resolved` 闭环。
- [ ] **G3 回归感知**：`resolved` 后新版本再触发 → `regression` 自动重开（验收此行为）。

## 校准样例

- 正例：接入后 curl 上报 200、`status: unresolved`、`count: 1`，再次上报 `count: 2` → 去重生效。
- 反例：`release` 漏填/乱填 → 回归检测失效（B2/D2 违约）。
- 正例：payload 中 `extra.user.access_token` 落库为 `[REDACTED_SECRET]` → E 系列通过。
- 反例：页面里有 `console.log(password)` 但没走上报 → 本地日志不受监控，不等于埋点已闭环（G1 可查性为核心交付）。