# Web 控制台 UI 契约（冻结版）

- **来源**：ADR `.agents/notes/proposed/architecture/2026-10-09-web-uiux-audit-and-improve.md`（Proposal 全部 8 项）
- **状态**：frozen（本波次唯一权威契约；测试与实施以此为准，先于口头描述）
- **读者**：Test Agent（断言方，写 E2E 前逐条对照）、Executor（实施方，逐条落实）
- **契约边界**：只约束行为/语义/可达性，不冻结视觉色值、字体、文案措辞（除明确规定的格式）
- **禁止项**：不改后端 API 契约；不新增页面/路由/图表/分页；不改视觉主题骨架；`web/src` 由 Executor 独占，本契约不产生任何业务代码
- **断言总则**：一切机器断言以真实浏览器（agent-browser 驱动，目标 http://127.0.0.1:3000 同源）为准：`getAttribute` / `getComputedStyle` / `textContent` / 网络请求日志。无法机器证明的项显式标注 `@review`。
- **验收口径**（ADR Consequences）：浏览器物理验证，Console Error = 0 + 关键旅程截图落盘。

---

## 1. data-testid 清单（机器断言，全部 REQ）

| testid | 位置 | 附加属性约束 |
|---|---|---|
| `gateway-status` | 健康徽标容器（header 内） | 必须恰好 1 个；`data-state` ∈ {`checking`, `ready`, `degraded`}（任何时刻仅取其一） |
| `load-error` | 错误横幅（页首可见反馈条） | 恰好 1 个；仅在数据 fetch 失败时可见（非 `display:none`/`hidden`），正常/空态时不可见 |
| `rel-time` | 列表"最后上报"单元格内的相对时间元素 | 每条含"最后上报"列的行必有 1 个；该元素自身携带 `title`（绝对时间,见 §4） |

- T1.1 **REQ**：`querySelectorAll('[data-testid="gateway-status"]').length === 1`；且 `getAttribute('data-state')` ∈ {checking, ready, degraded}。
- T1.2 **REQ**：`[data-testid="load-error"]` 存在；对数据接口（/api/issues、/api/traces）模拟失败后，横幅可见且默认文本 ≠ 空态"无数据"文案、≠ 骨架屏占位（文案必须能区分：加载失败 / 真无数据）。
- T1.3 **REQ**：`[data-testid="rel-time"]` 在 Traces 列表每一行存在（Issues 列表如含"最后上报"列同样要求）。

## 2. a11y 契约（机器断言，全部 REQ）

- T2.1 **REQ**：Issues 表格每个可点击 `<tr>`：`tabindex="0"` 且 `role="button"`；聚焦后按 Enter 或 Space 必须触发与 mouse click 相同的副作用（打开详情抽屉）。断言：行元素 `getAttribute('tabindex')==='0'`、`getAttribute('role')==='button'`；对首行 `send_keys(Enter)` 与 `send_keys(Space)` 各验证抽屉开启。
- T2.2 **REQ**：Traces 列表每个可点击卡片同 T2.1（`tabindex="0"` + `role="button"` + Enter/Space 生效）。断言同式。
- T2.3 **REQ**：页面上每个 icon-only 按钮（文本/aria-label 之外无可见文本、内容仅为 `<svg>`/`<img>` 的 `<button>`）必须带非空 `aria-label`；筛选 `<select>` 必须带非空 `aria-label`。断言：遍历页面全部 `button`/`select`，凡无可见文本者 `aria-label` 非空。
- T2.4 **REQ**：每个抽屉容器（面板元素自身）：`role="dialog"` + `aria-modal="true"` + 非空 `aria-label`（如"异常缺陷详情"/"Trace 详情"）。断言：`getAttribute('role')==='dialog'`、`getAttribute('aria-modal')==='true'`、`aria-label` 非空。

## 3. Trace 列表行内边距契约（机器断言，REQ + 静态）

- T3 **REQ（动态——主判据）**：每条 Trace 列表行元素，`getComputedStyle(row)` 四轴 `paddingTop/Right/Bottom/Left` 均 `≥ 8px`。`p-4.5` 非法类已废除。
- T3 静态附加门禁 `@review`（非零退出命令，源码不含非法类即通过）：
  ```bash
  ! grep -rn -E '(^|[[:space:]"'\''`])p-4\.5([[:space:]"'\''`]|$)' web/src
  ```
  允许的间距值来自 Tailwind 合法刻度（px-0/p-0.5/1/1.5/2/2.5/3/3.5/4/5…/12/14/16/20/24/28/32/36/40/44/48/52/56/60/64/72/80/96，无 4.5 类刻度）。动态断言为准，静态为兜底扫描。

## 4. 相对时间契约（机器断言，REQ）

- T4.1 **REQ**："最后上报"单元格文本为相对时间，匹配：`^\d+\s*(分钟|小时|天)前$`（≤60s 数据渲染为 `0分钟前`；不接受 `刚刚`，避免歧义）。
- T4.2 **REQ**：同一 `[data-testid="rel-time"]` 元素携带 `title` 属性 = 绝对时间，ISO 8601（`YYYY-MM-DDTHH:MM:SS±hh:mm` 或 `...Z`），Python `datetime.fromisoformat(title.replace('Z','+00:00'))` 可解析，且与相对时间同源（title 距今距离 ≈ 相对时间所表达值，±2 分钟容差）。
  ```python
  import re, datetime
  assert re.match(r'^\d+\s*(分钟|小时|天)前$', rel.text_content())
  dt = datetime.datetime.fromisoformat(rel.get_attribute('title').replace('Z','+00:00'))
  ```
- T4.3 **REQ**：详情抽屉内保留绝对时间（不强制相对格式），不在断言范围。

## 5. 抽屉交互契约（机器断言，REQ）

- T5.1 **REQ（Issues 背景点击关闭，与 Traces 一致）**：打开 Issues 抽屉后，点击视口内位于对话框面板 bounding rect **之外**的背景遮罩层区域 → 抽屉在 1s 内关闭（`role="dialog"` 元素从 DOM 移除或不可见）。实现形态：遮罩层元素独立于面板，`@click.self` 语义。断言：记面板 rect，点击 `(min(2, viewport 边角点) 且不在 rect 内)` 的坐标，随后断言抽屉消失。旧版（无该行为）红相必失败。
- T5.2 **REQ（Esc 统一全局兜底）**：抽屉开启且页面内无元素聚焦时按 Escape → 抽屉关闭且恰好关闭一次（无重开/双触发）。Issues 与 Traces 同规则；不允许仅挂在无焦点 div 上（`@review`：移除双轨依赖，统一走全局 window keydown handler，一处收口）。
- T5.3 **REQ**：背景点击关闭仅对遮罩层生效；点击面板内部不得关闭抽屉。

## 6. 健康徽标契约（REQ 机器断言 + `@review` 项）

- T6.1 **REQ**：页面加载后 3s 内，浏览器网络日志出现 ≥1 次 `GET /healthz`（同源）。断言：wait 至多 3s，network 中命中 `/healthz`。
- T6.2 **REQ**：首次请求返回后 `data-state` 与真实响应一致：HTTP 200 → `ready`；非 200/网络错误/超时 → `degraded`。断言：mock `/healthz` 为 200 → `data-state==='ready'`；mock 500 → `degraded`（用 agent-browser 请求拦截；旧版无该元素，红相失败）。
- T6.3 **REQ**：初始渲染状态必须为 `checking`（灰），随后按响应迁移。
- T6.4 **REQ（轮询间隔，慢速断言或 `@review`）**：间隔常数 = `15000`ms（`setInterval(…, 15000)`）。E2E 可选慢断言：首请求后 16s 内出现第 2 次 `/healthz`；或以源码级 `@review` 确认 15000 常量。二者至少执行其一。
- T6.5 **REQ**：`/healthz` fetch 挂 abort 信号，超时 ≤ `3000`ms（对齐 NFR `external_call_timeout_ms`）。`@review`：fetch 使用 `AbortSignal.timeout(3000)` 或等效（controller + setTimeout 3000 + abort）；挂起 `/healthz` 时 3000ms 内状态转 `degraded`（可选慢断言）。
- T6.6 **REQ `@review`（卸载清理）**：组件卸载（beforeUnmount/onUnmounted）时 `clearInterval` 轮询 timer、`abort()` 进行中的 fetch、移除挂载的全局监听器。SPA 单页无天然卸载路径，列为源码级核验项（grep `clearInterval`/`onUnmounted`/`abort` 于健康徽标实现）。
- 三态可区分性（软断言，可选）：checking/ready/degraded 三态 computed color 两两不同（不冻结具体色值）。

## 7. NFR 基准核对（结论：**已覆盖，零字段变更**）

`.dev-team/nfr-baseline.json` Schema 固定字段集：`external_call_timeout_ms` / `retry` / `concurrency_lock` / `logging` / `resource_release`，**未知字段拒绝**（additionalProperties 语义）。核对映射：

| 前端需求（本波） | NFR 覆盖字段 | 结论 |
|---|---|---|
| 全部 fetch 挂 AbortSignal ≤ 3000ms | `external_call_timeout_ms: 3000` + `resource_release.abort_controllers` | 已覆盖 |
| 卸载清理 event listener | `resource_release.event_listeners` | 已覆盖 |
| 卸载清理轮询 background timer | `resource_release.background_timers` | 已覆盖 |
| retry / concurrency_lock / logging | 后端侧重，前端不触达 | 不涉及 |

→ **不改动 `nfr-baseline.json` 任何字段**；前端专属细则全部落地于本文档 §1–§6，**不向 NFR JSON 塞入前端专属字段**（会被 Schema 拒绝）。

NFR 物理校验收据（退出码 0 且输出 `NFR_JSON_OK` 方通过）：
```bash
python3 -m json.tool .dev-team/nfr-baseline.json > /dev/null && echo NFR_JSON_OK
```
Schema 严格校验（字段集精确匹配 + 类型 + 关键值）：
```bash
python3 - <<'PY'
import json
EXPECT = {"external_call_timeout_ms": int, "retry": dict,
          "concurrency_lock": str, "logging": dict, "resource_release": list}
d = json.load(open(".dev-team/nfr-baseline.json"))
assert set(d) == set(EXPECT), f"unknown/missing fields: {set(d) ^ set(EXPECT)}"
for k, t in EXPECT.items():
    assert isinstance(d[k], t), k
assert d["external_call_timeout_ms"] == 3000, "frozen value drift"
assert d["retry"] == {"max_attempts": 3, "backoff_ms": 500}
assert set(d["resource_release"]) >= {"abort_controllers", "event_listeners", "background_timers"}
print("NFR_SCHEMA_OK")
PY
```

## 8. 红相 / 绿相预期（供 Test Agent 校准断言有效性）

- **旧版（红相必失败项）**：`gateway-status` testid 不存在（硬编码徽标）；`load-error` 不存在；`rel-time` 不存在；可点击行/卡片无 `tabindex`/`role`；Trace 行 computed padding 四轴 < 8px（`p-4.5` 静默丢失）；Issues 抽屉背景点击不关闭；无 `/healthz` 轮询请求发出。
- **新版（绿相全过）**：§1–§6 全部 REQ 断言通过且 Console Error = 0。

## 9. Non-Goals（本契约不约束）

视觉主题/字体/布局骨架不变更；不新增路由/图表/页面；不改 Rust API 契约；不做分页/无限滚动；不冻结具体文案与色值；不新增 NFR JSON 字段。