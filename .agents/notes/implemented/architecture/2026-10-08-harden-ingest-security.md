# Agent Note: Harden Ingest Security Against Adversarial Findings

Status: implemented

## Problem

对 k3s 生产部署执行三路安全对抗审核后，发现以下高危漏洞需修复：
1. `list_issues` 中 `platform`/`release` 使用字符串拼接构造 SQL，存在 SQL 注入（Critical）。
2. 脱敏管道遗漏 `tags`/`extra`/`breadcrumbs` 结构化字段，敏感凭据（Authorization/Cookie/Token）明文落库。
3. 公网 Ingest 无客户端身份校验、CORS 全放行、缺请求体大小限制与超时防护。
4. 网关层缺限流（RateLimit）、请求体缓冲上限（Buffering）与 HTTPS 强制重定向。

## Decision

1. **存储层**：`pg_repository` 与 `sqlite_repository` 的 `list_issues` 改用 `sqlx::QueryBuilder` 参数化绑定；`limit` 钳制 `1..=100`、`offset` 下限 0。
2. **脱敏管道**：新增递归 JSON 脱敏（`sanitize_json_value`），覆盖 `extra`/`breadcrumbs`/`tags`；SENSITIVE_KEYS 键级黑名单 + SECRET_PATTERNS 内容正则双层防护；`MAX_RECURSION_DEPTH=32` 防栈溢出。
3. **应用层**：CORS 收紧为 Ingest 仅 POST；`DefaultBodyLimit` 512KB（Ingest）/64KB（PATCH）；`TimeoutLayer` 10s；支持可选 `CLIENT_TOKEN` 鉴权（`X-Client-Token` 或 `Authorization: Bearer`）。
4. **网关层**：Traefik `ingest-ratelimit`（100/min, burst 200）、`ingest-buffering`（512KB）、`redirect-https` 重定向；Ingest/healthz 收敛为 `pathType: Exact`；移除 Pod CIDR 白名单，仅保留 Tailscale 网段。

## Alternatives considered

- **仅靠网关层防御，不动应用层**：无法防住集群内绕过与脱敏字段穿透，未选择。
- **强制所有客户端用 Token**：破坏开箱即用体验，故采用"配置了 CLIENT_TOKEN 才强制校验"的可选模式。

## Consequences

- SQL 注入向量消除，分页参数安全钳制。
- 深度递归脱敏覆盖全部结构化字段，含深度熔断。
- 公网上报具备可选鉴权 + 网关限流/体积限制 + 强制 HTTPS。
- 已通过单元测试与集群实测验证（401/429/413/301 行为符合预期）。
