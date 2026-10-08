# PonySentry API 契约与调试命令

所有端点基址：`https://sentry.ponyjob.top`（公网 Ingest）/ `https://sentry-internal.ponyjob.top`（私网 Web 与查询）。

若服务端配置了 `CLIENT_TOKEN`，上报与查询均需带 `X-Client-Token: <token>` 或 `Authorization: Bearer <token>`，否则 `401`。

## 端点契约

| 方法 | 路径 | 用途 | 鉴权 |
|---|---|---|---|
| POST | `/api/v1/ingest` | 上报错误事件 | 可选 CLIENT_TOKEN |
| GET | `/api/v1/issues?status=&platform=&release=&limit=&offset=` | 列 issue（limit 钳制 1~100） | 私网 |
| GET | `/api/v1/issues/{id}` | 单 issue 详情 | 私网 |
| PATCH | `/api/v1/issues/{id}` | 更新状态/处理人（`in_progress`/`resolved`/`ignored`） | 私网 |
| GET | `/api/v1/issues/{id}/events` | 事件明细（堆栈 + breadcrumbs + extra，最多 20 条） | 私网 |
| GET | `/healthz` | 存活探针 | 无 |

## 上报（curl 验证）

```bash
# 集群内验证（Pod 内执行）；公网则替换基址为 https://sentry.ponyjob.top
BASE="https://sentry.ponyjob.top"
TOKEN="$PONYSENTRY_CLIENT_TOKEN"   # 已配置才需要

curl -s -X POST "$BASE/api/v1/ingest" \
  -H "Content-Type: application/json" \
  -H "X-Client-Token: $TOKEN" \
  -d '{
    "platform": "rust",
    "release": "v1.2.0",
    "environment": "production",
    "message": "panic occurred at worker::run",
    "exception": {
      "error_type": "Panic",
      "value": "index out of bounds",
      "stacktrace": [
        {"filename": "src/worker.rs:42:10", "function": "worker::run", "in_app": true}
      ]
    },
    "extra": {"user": {"access_token": "leak_test_123"}},
    "breadcrumbs": [{"category": "http", "message": "POST /api/v1/jobs", "data": {}}]
  }'
# 期望：{"issue_id":"...","event_id":"...","fingerprint":"...","status":"unresolved","count":1}
# 注意：extra.user.access_token 应被服务端脱敏为 [REDACTED_SECRET]（抽查验证）
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
