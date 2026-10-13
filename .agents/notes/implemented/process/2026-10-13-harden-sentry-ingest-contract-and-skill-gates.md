# ADR: 加固 Sentry Ingest 契约推断与接入技能门禁 (2026-10-13)

Status: implemented

- **负责人**: dev-team
- **先决 / 关联决策**:
  - `implemented/feature/2026-10-08-add-project-dimension-to-issues.md` (引入 project 维度)
  - `implemented/bug-fix/2026-10-08-upgrade-fingerprint-v2-and-reindex.md` (v2 指纹与 reindex 机制)
  - `implemented/process/2026-10-08-add-ponysentry-instrumentation-skill.md` (接入技能初始版)
- **关联文件**:
  - `crates/ingest/src/project.rs`
  - `crates/ingest/src/fingerprint.rs`
  - `crates/server/src/routes.rs`
  - `.agents/skills/ponysentry-instrumentation/SKILL.md`
  - `.agents/skills/ponysentry-instrumentation/references/checklist.md`
  - `.agents/skills/ponysentry-instrumentation/references/api.md`

## 背景与问题
在真实客户端上报过程中，发现部分上报事件落库后的 `project` 字段为 `null`，且出错栈 `culprit` 亦为空：
1. 客户端接入时使用 `tags.service: "campus_collector"` 或通过 Tauri IPC 上报 `IpcCommandError`，而原推断逻辑仅识别 `tags.project`，对 `service`/`app` 缺少同义词映射；
2. 跨进程 IPC 调用往往没有物理调用栈（`stacktrace: null`），导致 Culprit 提取回退为 `null`；
3. 接入技能（`ponysentry-instrumentation`）未将 `project` 归属与 `culprit` 结构列为强校验门禁，缺乏上报验证时的断言检查。

## 决策内容
1. **同义词扩展与严格项目契约（服务端）**：
   - 在 `infer_project_name` 中增加对 `tags.service`、`tags.app`、`tags.application`、`extra.service` 等显式同义词的支持；
   - **严禁盲猜项目名**：服务端绝不根据技术栈特征（如 Tauri / IPC 命令）盲猜项目归属（例如不可将未带项目的 IPC 错误推测为 `job_copilot`），若客户端未提供有效项目标识，严格保持 `project: null` 并在响应中产生 `missing_project` 警告。
2. **Culprit 智能兜底合成（服务端）**：
   - 当 `stacktrace` 为空时，若存在 `tags.command` / `tags.source`，将其合成为 `{source} in {command}`（如 `tauri_safe_invoke in ask_cancel`）；
   - 启发式从 IPC 错误消息中提取命令名称合成 Culprit。
3. **IngestResponse 诊断反馈（服务端）**：
   - 在上报响应中增加 `warnings: Vec<String>`，当检测到未提供准确项目或栈时报警，杜绝静默吞掉契约缺陷。
4. **接入 Skill 门禁硬化（客户端）**：
   - 升级 `ponysentry-instrumentation` 的接入步骤与 Checklist（新增 H 节门禁）；
   - 强制要求客户端上报时必须显式注入 `tags.project` 或 `extra.project_path`，自检时验证响应中 warnings 为空且 `GET /api/v1/issues/{id}` 校验 `project` 与 `culprit` 均非空。
5. **存量数据刷正**：
   - 重新构建发布服务，并通过 `PONY_REINDEX_FINGERPRINTS=1` 完成数据库存量数据重索引。

## Alternatives considered
- **服务端根据 IPC 命令特征盲猜项目归属（如将 ask_* 推断为 job_copilot）**：
  - 理由被否决：job_copilot 本身非 Tauri 技术栈，盲猜既不准确，更会掩盖客户端缺少 `project` 字段的契约缺陷。正确的工程治理是严格执行门禁，要求客户端显式声明项目名称或工作区路径。
- **破坏性 Schema 校验（直接拒绝并返回 400 Bad Request）**：
  - 理由被否决：已有存量应用正在线上运行，直接阻断 HTTP 400 会导致客户端错误丢失或崩溃风险。采用“智能兼容推断 + warnings 诊断告警”兼顾了零崩溃平滑过渡与接入方 CI 门禁强约束。
- **纯粹要求客户端修复，服务端不推断**：
  - 理由被否决：真实多进程/IPC 错误天然缺乏传统 call stack，若服务端不提供特征提炼支持，即使客户端规范化也无法有效生成 Culprit。

## 验收结果与物理收据
- 原生测试门禁：`cargo test` 全库 45+ 测试用例全绿（Exit Code 0）。
- 生产环境重索引验证：
  - `[TEST] Probe issue from campus_collector` 的 `project` 规范识别为 `campus_collector`；
  - 未带项目信息的 `IpcCommandError` 事件保持 `project: null` 并触发 `missing_project` warning 契约门禁；其 `culprit` 成功合成提炼为 `tauri_safe_invoke in ask_cancel` / `tauri_safe_invoke in ask_answer`。
