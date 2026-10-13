# PonySentry API 契约与调试命令

所有端点基址：`https://sentry.ponyjob.top`（公网 Ingest）/ `https://sentry-internal.ponyjob.top`（私网 Web 与查询）。

当前生产部署环境**已强制开启 `CLIENT_TOKEN` 鉴权**，未携带合法 Token 会返回 `401 Unauthorized`。上报请求必须带 `X-Client-Token: <token>` 或 `Authorization: Bearer <token>`。

## 端点契约

| 方法 | 路径 | 用途 | 鉴权 |
|---|---|---|---|
| POST | `/api/v1/ingest` | 上报错误事件 | 可选 CLIENT_TOKEN |
| GET | `/api/v1/issues?status=&platform=&release=&project=&limit=&offset=` | 列 issue（limit 钳制 1~100） | 私网 |
| GET | `/api/v1/projects` | 已出现过的项目名去重列表（项目筛选下拉的候选源） | 私网 |
| GET | `/api/v1/issues/{id}` | 单 issue 详情 | 私网 |
| PATCH | `/api/v1/issues/{id}` | 更新状态/处理人（`in_progress`/`resolved`/`ignored`） | 私网 |
| GET | `/api/v1/issues/{id}/events` | 事件明细（堆栈 + breadcrumbs + extra，最多 20 条） | 私网 |
| GET | `/healthz` | 存活探针 | 无 |

## 上报（curl 验证）

```bash
# 集群内验证（Pod 内执行）；公网则替换基址为 https://sentry.ponyjob.top
BASE="https://sentry.ponyjob.top"
TOKEN="$PONYSENTRY_CLIENT_TOKEN"   # 已配置才需要

RESP=$(curl -s -X POST "$BASE/api/v1/ingest" \
  -H "Content-Type: application/json" \
  -H "X-Client-Token: $TOKEN" \
  -d '{
    "platform": "rust",
    "release": "v1.2.0",
    "environment": "production",
    "message": "panic occurred at worker::run",
    "tags": {"project": "shop-web"},
    "exception": {
      "error_type": "Panic",
      "value": "index out of bounds",
      "stacktrace": [
        {"filename": "src/worker.rs:42:10", "function": "worker::run", "in_app": true}
      ]
    },
    "extra": {"project_path": "/home/dev/shop-web", "user": {"access_token": "leak_test_123"}},
    "breadcrumbs": [{"category": "http", "message": "POST /api/v1/jobs", "data": {}}]
  }')
echo "$RESP"
# 期望：{"issue_id":"...","event_id":"...","fingerprint":"...","status":"unresolved","count":1}
# 契约门禁：响应不得含有 "warnings"（或 warnings 为空）
# 抽查 issue 详情，断言 project 和 culprit 均非空：
# ISSUE_ID=$(echo "$RESP" | jq -r .issue_id)
# curl -s -H "X-Client-Token: $TOKEN" "$BASE/api/v1/issues/$ISSUE_ID" | jq '{project, culprit}'
# 期望：project == "shop-web", culprit != null
```

## 项目维度（project）与识别规则

issue 的 `project` 字段通过以下优先级严格识别（`pony_sentry_ingest::infer_project_name`）：
1. 优先读取 `tags.project` / `tags.service` / `tags.app` / `tags.application`；
2. 其次读取 `extra.project` / `extra.service` / `extra.project_path`（工作区末段名称）；
3. 网关项目特征兜底（仅限网关系统错误：`GatewayExhaustedError` 等推断为 `ponyllm`）。

> **注意**：除网关专用错误外，服务端严禁根据异常类型或 IPC 盲猜项目名；所有业务应用接入必须显式提供 `tags.project` 或 `extra.project_path`，否则 issue 的 `project` 将为 `null` 并在上报响应中产生 `missing_project` 告警。

```bash
# 筛选某项目的未解决缺陷
curl -s "$BASE/api/v1/issues?project=shop-web&status=unresolved"
# 项目筛选候选
curl -s "$BASE/api/v1/projects"   # => ["blog-web","shop-web"]
```

## debug 查看（查询）

```bash
# 按端/状态/版本筛选未解决问题（Agent 排查入口）
curl -s -H "X-Client-Token: $TOKEN" \
  "$BASE/api/v1/issues?platform=tauri&status=unresolved&release=v1.2.0"

# 单 issue 详情
curl -s -H "X-Client-Token: $TOKEN" "$BASE/api/v1/issues/<ISSUE_ID>"

# 事件明细：堆栈 + breadcrumbs + extra（脱敏验证点）
curl -s -H "X-Client-Token: $TOKEN" "$BASE/api/v1/issues/<ISSUE_ID>/events"

# 认领（Agent 闭环第一步）
curl -s -X PATCH -H "X-Client-Token: $TOKEN" -H "Content-Type: application/json" \
  "$BASE/api/v1/issues/<ISSUE_ID>" \
  -d '{"status":"in_progress","assigned_to":"agent-coder-01"}'

# 修复后闭环
curl -s -X PATCH -H "X-Client-Token: $TOKEN" -H "Content-Type: application/json" \
  "$BASE/api/v1/issues/<ISSUE_ID>" -d '{"status":"resolved"}'
```

## 脱敏抽查（关键安全验证）

上报含凭据的样例后，查事件 payload，断言**不含**明文 Token/用户名路径：

```bash
curl -s -H "X-Client-Token: $TOKEN" "$BASE/api/v1/issues/<ISSUE_ID>/events" \
  | grep -E "leak_test|john_doe|/home/" && echo "❌ 脱敏失败" || echo "✅ 脱敏通过"
```

## 状态机契约

`unresolved → in_progress → resolved → ignored`；新版本（release 变化）同指纹再度触发已 resolved 的 issue → 自动 `regression`。
