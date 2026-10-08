# Agent Note: Add Ponysentry Instrumentation Skill

Status: implemented

## Problem

开发者需要在各端（Rust/Tauri/Vue/FastAPI）为应用接入错误/崩溃埋点上报至 PonySentry，并在 debug 时查看 issue、堆栈与 breadcrumb，落地 Sentry 最佳实践（错误分组、面包屑、用户上下文、release 标记、采样、零信任脱敏）。该流程会随项目演进反复发生，需资产化为可复用技能。

## Decision

新增技能 `.agents/skills/ponysentry-instrumentation/`，按项目 skill 五要素契约（触发式 description、真相源清单、程序步骤、校准样例、验证报告）编写：
- `SKILL.md`：主流程（确认端点/鉴权 → 识别目标端 → 生成埋点 → 客户端脱敏 → curl 验证 → debug 查询）；
- `references/rust-tauri.md`、`references/vue.md`、`references/fastapi.md`：各端埋点代码模板；
- `references/api.md`：Ingest/查询/认领 API 契约与调试命令；
- `references/checklist.md`：Sentry 最佳实践落地清单（A 捕获完整性 / B 分组 / C 面包屑 / D 上下文 / E 脱敏 / F 采样性能 / G 调试闭环）。

## Alternatives considered

- **不资产化，每次现写埋点**：流程重复且容易遗漏脱敏与 release 契约，未选择。
- **接入第三方 Sentry SDK**：与自研 PonySentry 服务契约（/api/v1/ingest + CLIENT_TOKEN + 双端脱敏）不匹配，且增加依赖，未选择。

## Consequences

- 埋点接入与调试查看流程可复用，输出对齐 PonySentry 契约与 sanitizer 脱敏规则。
- 已通过 3 个 eval 用例验证（Vue/Rust/Tauri 查询），脱敏 smoke test 11/11 通过；实弹 curl 验证受沙箱无公网路由限制，需在部署环境复验。