# ADR-003: PonySentry Webhook 触发链路与 DSH 自动化诊断及企微通知架构

## 状态
已采纳 (Accepted) - 2026-10-08

## 上下文与业务背景
PonySentry 作为一个轻量级崩溃与异常收集服务，当捕获并聚合到新缺陷（`new_issue`）或旧缺陷重新复现（`regression`）时，需要具备自动唤醒开发智能体进行问题诊断与方案编制的能力。
为了保障系统稳定性与安全性：
1. 诊断过程完全自动化在对应项目的 workspace 中调起；
2. 调起后自动在 DSH 中产生持久化 Session，便于在 Web UI 中跟踪；
3. 诊断完成后，通过企业微信通知直接触达责任人，附带诊断摘要、推荐方案及 Web UI 会话跳转链接；
4. 明确实施修复必须经由开发者在 Web UI 审查确认，禁止无人值守盲目修改线上代码。

## 架构决策

### 1. 企业微信通知规范对齐
参考 `job_copilot` 的通知实现标准（`NotificationService.send_wechat_work`）：
- **配置环境变量**：
  - `WX_WORK_EXTERNAL_API_URL`：企业微信发送 API 地址。
  - `WX_WORK_EXTERNAL_API_KEY`：请求鉴权 Key（Header: `X-API-KEY`）。
  - `MY_USER_ID` 或 `WX_WORK_USER_ID`：默认通知接收者企业微信工号/用户名。
- **Payload 兼容双格式**：
  - 若 URL 为 `/send_msg`（阿里云 Notifier）：
    `{"msg": message, "choice": "wx_work"}`
  - 若为通用微信中继：
    `{"content": message, "to_user": target_user}`

### 2. PonySentry Webhook 触发规范
- **触发时机**：
  - Issue 首次产生（`event_type: "issue.created"`）
  - 已标记 resolved 的 Issue 再次复现（`event_type: "issue.regression"`）
- **冷却防抖（Cooldown）**：同 Issue 指纹在 10 分钟内至多触发一次 Webhook，防止错误级联风暴。
- **Webhook Payload 结构**：
  ```json
  {
    "event_type": "issue.created",
    "timestamp": 1728374400,
    "issue": {
      "id": "iss_9f82c1",
      "fingerprint": "a1b2c3d4...",
      "title": "ZeroDivisionError: division by zero",
      "culprit": "src/services/billing.py:42",
      "platform": "python",
      "count": 1,
      "project_path": "/home/dm/job_copilot"
    },
    "latest_event": {
      "id": "evt_8832a",
      "message": "division by zero",
      "stacktrace": "Traceback (most recent call last):\n...",
      "breadcrumbs": [...]
    }
  }
  ```

### 3. DSH Headless 调度与通知链路 (Dispatcher)
轻量 Dispatcher 服务（或 webhook handler）接收上述 Payload 后按序执行：
1. **定位工作区**：检查 `issue.project_path` 是否存在。
2. **组装诊断提示词 (Prompt)**：
   包含 Issue 标题、核心堆栈、出错位置，并强制注入指令：“仅分析定位根因、制定修复方案并编写测试用例草案，严禁擅自修改核心生产文件，诊断结论整理完毕后结束”。
3. **唤起 DSH**：
   在目标工作区执行 `dsh headless --json "<Prompt>"`。
4. **解析输出并捕获 Session ID**：
   从首行 `{"type":"session","sessionId":"..."}` 提取 Session ID，从末行 `{"type":"final","text":"..."}` 提取诊断结论。
5. **企业微信通知推送**：
   组装 Markdown 格式卡片消息，推送至企业微信：
   - 告警标题：`【PonySentry 故障诊断】<Issue Title>`
   - 错误位置：`<Culprit>`
   - 诊断结论摘要：`<Summary>`
   - 会话链接：`http://127.0.0.1:3080/?session=<SessionId>`
   - 明确操作指示：`请登录 DSH Web UI 查看方案并确认执行。`

## Alternatives considered
- **直接通过 DSH 自动执行修复与提交**：存在极大越权与未知引入 Bug 风险，因此拒绝无人工确认的闭环模式。
- **纯邮件通知**：响应滞后，无法满足实时线上故障的分钟级闭环响应。