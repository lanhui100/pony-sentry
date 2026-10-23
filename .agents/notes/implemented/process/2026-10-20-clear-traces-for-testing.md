# Agent Note: Clear All Trace Records for Fresh Testing

Status: implemented

## Problem

PonySentry dev 实例（`http://127.0.0.1:3000`，Postgres `ponysentry` @ `127.0.0.1:30543`）中积累了一批由 job_copilot agent runtime 上报的历史 trace 记录。用户需要把既有 trace 全部清空，以便后续新增的测试 trace 在控制台中以干净、无干扰的状态呈现。

当前存量（执行前实测）：

- `traces` 表共 **12 条**，全部 `eval_status = unreviewed`、`project = job_copilot`；
- `issues` 52 条 / `events` 534 条（不属于本次清理范围，保持不动）。

## Decision

对 dev 实例 `traces` 表执行 `DELETE FROM traces;`，清空全部 trace 记录。

- 范围仅限 `traces` 表；`issues` / `events` 表（错误归因与事件流）不在本次请求范围内，不动。
- 执行路径：直接经 Postgres 客户端（`psql`，复用运行中 `pony-sentry-server` 进程的 `DATABASE_URL`）执行；服务端当前无批量删除 API（仅 `list`/`get`/`patch eval_status`），为一次性测试数据清理直连 SQL 更小、更贴合。
- 用户已明确选择「直接清空」，不做备份导出。
- 机械验收：删除后 `SELECT count(*) FROM traces` 必须返回 `0`，且 `GET /api/v1/traces` 返回空数组。

## Alternatives considered

1. **备份后清空**：先导出 12 条 trace 到文件再删除，可恢复。用户明确选择直接清空（测试数据无保留价值），故未采用。
2. **仅标记 `eval_status = reviewed`**：旧记录仍占据列表，无法满足「新增测试」需要干净空列表的意图，未采用。
3. **走服务端新增批量删除 API**：为一次性测试数据清理引入代码变更与 API 契约变更，超出最小闭包，未采用；如未来有常态化清理需求再评估。
4. **连同 issues/events 一并清空**：超出用户请求范围（明确为 trace 记录），未采用。
