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
    "extra": {"project_path": "/home/dev/shop-web", "user": {"access_token": "leak_test_123"}},
    "breadcrumbs": [{"category": "http", "message": "POST /api/v1/jobs", "data": {}}]
  }'
# 期望：{"issue_id":"...","event_id":"...","fingerprint":"...","status":"unresolved","count":1}
# 注意：extra.user.access_token 应被服务端脱敏为 [REDACTED_SECRET]（抽查验证）
# 注意：extra.project_path 末段会成为 issue.project（此处为 shop-web），用于 Web 端项目筛选
```

## 项目维度（project）

issue 的 `project` 字段由上报事件 `extra.project_path` 的**末段**推导（`pony_sentry_ingest::project_name_from_extra`）：
`{"extra": {"project_path": "/home/dev/shop-web"}}` → `issue.project == "shop-web"`。

要点：

- 取末段而非整条路径——脱敏管道会把用户目录前缀替换为 `[USER_HOME]`（前缀本就不可读），
  末段（项目目录名）在脱敏后完整保留，故项目名是唯一稳定可用的展示与筛选键。
- 一个 issue 的 `project` **以首报为准**，后续同指纹上报不改写它（指纹/标题/出错位置都诞生于首报工作区）。
- 未注入 `extra.project_path` 的事件 `project` 为 `null`，不进入 `/api/v1/projects` 候选集。
- `path` 不是契约键：只有 `project_path` 生效。

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
