# PonySentry Web 控制台 UI/UX 改进波次 —— NFR 达标报告 (wave-1-web-ux)

> 交付资产归档：波次实施（commit e546333）的 NFR 基准逐项达标证据。
> 基准文件：`.dev-team/nfr-baseline.json`（零字段变更，协议特派员核对结论：已覆盖前端需求）。

## 逐项对照（基准字段 vs 实现证据 vs 判定）

| # | 基准字段 | 实现证据（web/src/App.vue，行号以 e546333 为准） | 判定 |
|---|---|---|---|
| 1 | `external_call_timeout_ms: 3000` | `API_TIMEOUT_MS = 3000`；统一封装 `apiFetchJson()`：fetch + `response.ok` 校验 + `await response.json()` 全程在同一 AbortSignal 生命周期（setTimeout→abort 直到 finally 才 clearTimeout，超时覆盖响应体读取）；`/healthz` 轮询独立 AbortController + 3000ms 超时 | **达标** |
| 2 | `retry: {max_attempts: 3, backoff_ms: 500}` | 本波次不新增外部网络重试逻辑（保持现状）；前端错误经错误横幅 `data-testid="load-error"` 提供人工重试（按钮触发 refreshAll），语义等价于用户驱动重试 | **达标（人工重试路径）** |
| 3 | `concurrency_lock: optimistic_or_mutex` | 双 abort 集合隔离：`refreshControllers`（仅刷新系只读 GET）供 refreshAll 中止防旧响应覆盖；`allControllers`（含 PATCH 写请求）仅随卸载中止——刷新不会误伤 in-flight 写请求（L3-A 复审 PASS） | **达标** |
| 4 | `logging.format: json`（前端降级：错误不静默吞） | 逐 catch 检查：apiFetchJson 超时→TimeoutError、主动取消→AbortError（非裸吞）；各 fetcher 失败置 loadFailed 旗标驱动可见错误横幅 + console.error；checkHealth 失败置 degraded 可见态；无裸 pass 吞异常 | **达标（前端降级路径）** |
| 5 | `resource_release: [abort_controllers, event_listeners, background_timers]` | onUnmounted：`removeEventListener('keydown')`、`clearInterval(healthTimer=15000)`、`clearInterval(relTimeTimer=30000)`、abort healthController、abortAllControllers() 中止全部 in-flight | **达标** |

## 浏览器物理验收（Tier-1 真实环境）

- **全量验收测试**：`tests/web/test_web_ui_acceptance.py` 26/26 通过（`green-phase-final3.txt`，Ran 26 tests in 143.190s — OK，零跳过），覆盖契约 T1.1–T6.6 + 硬门禁 a（Console Error = 0）+ 硬门禁 b（关键旅程截图落盘）。
- **关键旅程截图**（存档 `docs/release/wave-1-web-ux/`）：Issues 列表、详情抽屉、Traces 视图各一张，真实浏览器渲染物。
- **线上服务**：重编 `target/release/pony-sentry-server` 并部署 `/usr/local/bin/pony-sentry-server`（systemd `pony-sentry.service` 重启后 active，`/healthz` 200）。

## 审查记录（L3 三路）

| 审查 | 初审 | 复审 | 落盘文件 |
|---|---|---|---|
| L3-A 并发/状态流转 | FAIL（2 条缺陷）→ 修复 | PASS | `review-l3a.json` / `review-l3a-r2.json` |
| L3-B a11y/契约一致 | FAIL（3 条 a11y 缺陷）→ 修复（绿相 26/26 复验通过） | —（修复后全量绿相含全部相关用例） | `review-l3b.json` |
| L3-P 生产就绪五问 | PASS | — | `review-l3p.json` |

## 已知残留（如实记录，不掩饰）

- `selectIssue` 快速切换 A→B 时，A 的事件明细 GET 不随选择切换中止，理论上存在晚到响应覆盖新明细的竞态；该缺陷 wave 前已存在、非本次引入（L3-A/L3-P 均确认），建议后续 backlog 处理。
- 健康徽标每 15s 轮询周期起始重置 checking 再迁移，健康时徽标周期灰闪（契约 T6.3 仅要求初始态，未违规），后续可优化为"无变化不闪"。