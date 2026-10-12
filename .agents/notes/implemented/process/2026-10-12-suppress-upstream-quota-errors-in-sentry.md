# Agent Note: Suppress Upstream Quota Errors in Sentry Reporting

Status: implemented

## Problem

PonySentry（本机 http://127.0.0.1:3000，生产数据）当前 unresolved 的 `GatewayExhaustedError` issue 中，最高频来源是 jev 模型（`zen-jev` provider）的上游配额/限流/无可用 key：

- `c21a9e6c` jev-1.13-free "No available key for provider 'zen-jev'"（37 次，`RateLimitExceeded`）
- `69a0a24d` jev-1.13-free HTTP 429 `FreeUsageLimitError`（1 次，`QuotaExhausted`）

这两类是上游可预期、周期性出现的外部资源状态（配额耗尽/限流窗口/冷却），不是 ponymll 网关或本仓库的代码缺陷。Sentry 告警语义是"可行动、需修复"，持续上报只会污染 issue 面板、稀释真实缺陷的可读性。

根因：`crates/ponyllm-server/src/routes/systemone.rs` 210-229 行对 `GatewayExhaustedError` **无条件**调用 `state.sentry.capture_error(...)`，没有像 `chat.rs` / `responses.rs` 那样先做过滤（`is_client_bad_request`），也没有像 `streaming.rs` 2369-2370 行注释那样把"上游厂商网络抖动/外部环境类"错误排除出上报。

决策：jev 模型的上游配额/限流/无可用 key 类错误【不应上报 Sentry】；同一代码库已有的过滤先例（chat.rs/responses.rs 的 `is_client_bad_request` 文本过滤、streaming.rs `StreamFailureError` 不报 Sentry）证明按错误类型过滤是既定惯例。

## Decision

在 `crates/ponyllm-server/src/routes/systemone.rs`（模块 `ponyllm_server::routes::systemone`）提取一个纯函数作为上报闸门：

```rust
pub fn should_report_sentry_exhaustion(kind: &GatewayErrorKind, last_error: &str) -> bool
```

可见性为 `pub`（非 `pub(crate)`）：集成测试（`crates/ponyllm-server/tests/*.rs`）是独立 crate，只能访问 ponyllm-server 的 `pub` 项，`pub(crate)` 会使绿相验收必然 E0603「function is private」编译失败。纯函数无副作用，对外可见无害；与既有先例一致（`extractors::format_exhausted_message`、`extractors::project_openai_error` 均为 `pub fn`）。`routes` 与 `routes::systemone` 均为 `pub mod`，且 `routes/mod.rs` 有 `pub use systemone::*` 重导出，测试引用路径 `ponyllm_server::routes::systemone::should_report_sentry_exhaustion` 成立。

`handle_systemone` 末尾的 `capture_error` 块（现 210-229 行）改为 `if should_report_sentry_exhaustion(&last_kind, &last_error) { ...capture_error... }`：闸门为 `false` 时跳过 `capture_error`；`GatewayEvent::RequestFailed` 指标、`project_openai_error` 响应构造、`retry-after` 头照常执行，不受影响。

### 契约：判定语义

**返回 `false`（不上报）当且仅当满足以下任一（OR 语义，任中即不报）：**

1. **kind 命中配额族**：`kind` 为 `GatewayErrorKind::RateLimitExceeded { .. }` 或 `GatewayErrorKind::QuotaExhausted`。
   - `CoreError::NoAvailableKey(_)` 经 `CoreError::kind()`（`ponyllm-core/src/error.rs` 133-135 行）折叠为 `RateLimitExceeded { retry_after: None }`，因此"无可用 key"天然落入此规则，无需单独分支。
2. **文本兜底命中配额/冷却标记**（与 kind 正交：**无论 kind 为何，文本命中即不报**）：`last_error.to_lowercase()` 包含以下任一子串：
   - `"no available key"` —— `NoAvailableKey` 渲染文本（"No available key for provider ..."）
   - `"freeusagelimiterror"` —— zen 免费模型 `FreeUsageLimitError`（上游 429/403 限定体）
   - `"rate limit"` —— 上游限流措辞（含 `FreeUsageLimitError` 体 "Rate limit exceeded"）
   - `"quota"` —— 上游配额措辞（quota exhausted / family-quota / window-budget）
   - `"429"` —— HTTP 429 状态文本（`kind()` 分类丢失率限信号时的兜底）

**其余 kind → 返回 `true`（上报）。**

完整 kind 映射表：

| kind | 是否上报 | 依据 |
|---|---|---|
| `RateLimitExceeded { .. }` | 否 | 上游限流/冷却；`NoAvailableKey` 折叠于此 |
| `QuotaExhausted` | 否 | 上游配额耗尽（402 / 429 `FreeUsageLimitError` 等） |
| `AuthInvalid` | 是 | 凭据死亡（401）——需人工换 key |
| `UpstreamUnavailable` | 是 | 上游 5xx/连接失败——需关注上游健康 |
| `LockContention` | 是 | 跨副本刷新锁异常——需排查锁后端 |
| `ClientBadRequest` | 是 | 客户端参数 4xx（本契约不夹带 `is_client_bad_request` 过滤） |
| `CapacityExhausted` | 是 | 上下文容量不足——需扩容/改模型 |
| `ModelNotFound` | 是 | 模型不存在/无匹配 provider——需改配置 |
| `Internal` | 是 | 内部缺陷——最需上报 |

### 契约边界（不夹带重构）

- 本契约只治理"上游配额/限流/无可用 key/冷却"类错误；**不**把 chat.rs/responses.rs 的 `is_client_bad_request` 文本过滤（`invalid_request_error` / `400 Bad Request` / `Duplicate function_call_output`）搬进 systemone.rs——`ClientBadRequest` 仍上报（决策表明确），既有路由的过滤保持原样。
- 纯函数：无 IO、无副作用、不触碰 `state.sentry`；不改 `capture_error` 的 payload 结构（tags/extras 保持现状，`last_pool_exhausted` 变量与 extras 字段保留）。
- 不在本变更内改动 chat.rs / responses.rs / streaming.rs 的任何代码。

## Alternatives considered

1. **服务端 PonySentry 加 ignore 规则**（对特定 issue/指纹静默）：
   - 优点：上报端零改动。
   - 落选：服务端 ignore 只影响 UI 过滤/认领，不阻止 ingest 写入与重复聚合；无法按"错误类型"通用表达（指纹已按模型拆分，逐模型加 ignore 不可维护）；上游配额错误换模型/换 provider 会再生新 issue，规则永远追着跑。该不该告警的决定权应在产生端（ponyllm），而不是消费端事后擦除。
2. **客户端完全不上报**（systemone 路由所有 `GatewayExhaustedError` 都不报）：
   - 优点：最简单，噪音归零。
   - 落选：会连同 `AuthInvalid`（凭据死亡，需人工换 key）、`UpstreamUnavailable`（上游大面积故障）、`Internal`（真缺陷）一起吞掉——这些恰恰是 Sentry 最该亮的灯。误伤面超过收益。
3. **仅过滤 jev 模型名**（`model` 以 `jev-` 开头或 provider == `zen-jev` 时不报）：
   - 优点：直击当前两个 issue。
   - 落选：模型名是易变配置（新增/改名模型不覆盖）；同类配额错误发生在任何模型/provider 时噪音复现；与错误本质不符——该报不该报由错误类型决定，不由模型名决定。
4. **按错误类型过滤（采纳）**：
   - 与既有先例一致（chat/responses 文本过滤、streaming 排除外部环境类、`GatewayErrorKind::kind_name()` 已有稳定类型轴）；新增模型/provider 无需改上报端；`AuthInvalid` 等可行动错误保留告警；文本兜底防御 `kind()` 分类丢失的场景。契约以 `GatewayErrorKind` + `last_error` 双维度表达，未来扩展新 kind 只需补映射表。

## Consequences

- 两个存量 jev issue（`c21a9e6c` / `69a0a24d`）将不再累积新事件；存量 issue 的处置（resolve 等）属 PonySentry UI 操作，不在本变更范围。
- `GatewayEvent::RequestFailed` 指标、流日志、`retry-after` 响应头不受影响——只跳过 Sentry 错误上报。
- 契约冻结于本 ADR；代码实现（systemone.rs 落地函数 + 闸门）由 Executor 在其写域完成，Test 以本契约冻结红相验收。
- 文档家：错误上报规则的家是 `.agents/skills/ponysentry-instrumentation/SKILL.md`；本决策落地时应在其上报规则节补一行指向本 ADR（same-commit 纪律，随实现提交处理，靠 review 核验）。
