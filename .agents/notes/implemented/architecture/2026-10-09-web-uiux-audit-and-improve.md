# Agent Note: web console UI/UX audit and improvement wave

Status: implemented

## Problem

Web 控制台（`web/src/App.vue`，单文件 SPA）已完成功能覆盖（Issues / Traces 双视图、KPI、
筛选、抽屉详情），但审计发现若干 UI/UX 硬伤，直接影响可信度与可用性：

1. **"网关就绪"徽标是硬编码假状态**：header 恒显示绿色 `网关就绪` 脉冲，后端 /healthz 挂了也照常；
   对监控类控制台这是信任硬伤。
2. **Trace 列表行无内边距**：`class="p-4.5"` 不是合法 Tailwind 类（间距刻度无 4.5），
   该样式静默丢失，行内容贴边（已用 dist CSS 缺失 `.p-4\.5` 规则机械证实）。
3. **无加载/错误状态**：fetch 失败仅 console.error，UI 无任何可见反馈；
   空态文案"无匹配的异常缺陷"在"加载失败"与"真无数据"之间不可区分，误导排查。
4. **fetch 无超时/取消**：未用 AbortController，组件卸载后 in-flight 响应仍写入，
   违反 `.dev-team/nfr-baseline.json` 的 `resource_release: abort_controllers` 承诺。
5. **可访问性缺失**：可点击 `<tr>`/KPI 卡片无 `tabindex`/`role`/键盘操作；
   icon-only 按钮无 `aria-label`；抽屉无 `role="dialog"`/`aria-modal`。
6. **抽屉交互不一致**：Issues 抽屉无背景点击关闭（Traces 有 `@click.self`）；
   Issues 抽屉的 `@keydown.esc` 挂在无焦点的 div 上（实际靠全局 window handler 兜底），机制双轨。
7. **无相对时间**：列表时间全为绝对时间戳，可扫读性差（遥测控制台宜"x 分钟前" + tooltip 绝对时间）。
8. **KPI 口径不一致**："需关注缺陷 (默认)" 副文案写"待处理+诊断中"，但 attention 口径实际含 regression；
   卡片标题与过滤语义脱节。

## Decision

以 **B 级（跨文件多模块、涉设计 spec）** 编排一个实施波次，目标为最小闭环修复上述硬伤，
**不**重构整体视觉风格（MiSans/深色/靛蓝方向保留）、不新增页面、不改后端 API 契约：

1. **真实健康状态**：header 徽标改为轮询 `/healthz`（初始 + 15s 间隔），
   呈现 checking（灰）/ ready（绿）/ degraded（红）三态；轮询 timer 随组件卸载清理。
2. **合法化间距**：`p-4.5` → `p-4`（Trace 列表行），全局扫一遍非法 Tailwind 类。
3. **加载骨架 + 错误横幅**：首载显示 skeleton；fetch 失败显示可关闭/可重试的错误横幅，
   空态文案区分"加载失败"与"无数据"。
4. **AbortController + 超时**：所有 fetch 挂 AbortSignal（≤3000ms，对齐 NFR external_call_timeout_ms），
   组件卸载/刷新时 abort 旧请求。
5. **可访问性**：可点击行/卡片加 `tabindex="0"` + `role="button"` + Enter/Space 触发；
   icon-only 按钮与筛选 select 加 aria-label；抽屉加 `role="dialog"`/`aria-modal`。
6. **交互一致**：Issues 抽屉补 `@click.self` 背景关闭，移除双轨 Esc 依赖（统一走全局 handler）。
7. **相对时间**：列表列改 `x 分钟前/小时前`，title 悬浮绝对时间；详情抽屉保留绝对时间。
8. **KPI 口径**：attention 副文案改"待处理+诊断中+复现"，与过滤语义一致。

**Non-Goals**：不改视觉主题/字体/布局骨架；不加路由/图表/新页面；不改 Rust API 契约；
不做分页/无限滚动（当前数据量下非必要，记为后续 backlog）。

## Alternatives considered

- **整体重做视觉（换主题/组件库）**：风险高、收益低——现有深色+靛蓝+MiSans 已可用，
  当前痛点全在真实性与可达性而非美观。落选：本轮只修硬伤。
- **网关徽标仅改文案（"状态未知"）**：治标不治本，监控控制台必须反映真实健康。落选：接 /healthz 轮询。
- **引入 UI 组件库（Element/Naive）**：单文件 SPA 内换库需重写全部交互，爆炸半径过大。落选：Tailwind 手写补齐。
- **错误态只在控制台打日志**：用户（人）看不到 = 无反馈，监控台信任崩塌。落选：加可见错误横幅。
- **相对时间仅前端**：绝对时间仍存于 tooltip/详情，两全。采纳。

## Consequences

- `web/src/App.vue` 为主要改动载体（+style.css 骨架/横幅样式），`web/dist/index.html` 由构建再生。
- 服务端无改动，但 `include_str!("web/dist/index.html")` 意味着**必须重编 server 二进制并重启服务**
  新 UI 才生效（`sudo cp target/release/pony-sentry-server /usr/local/bin/ && sudo systemctl restart pony-sentry`）。
- 验收以浏览器物理验证为准：Console Error = 0 + 关键旅程截图落盘。
