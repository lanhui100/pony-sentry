# Agent Note: style dashboard kpi cards with semantic panels and white inverted text

Status: implemented

## Problem

在 Web 控制台的数字统计 Dashboard 中（Issues 与 Traces 视图顶部的 KPI 卡片），
原样式采用暗灰半透明背景，标题和数字依赖微弱的浅色或彩色单字，语义色彩感知较弱，弱光/快速扫读时对比度与辨识度不足。
用户要求将 web 端的数字统计 dashboard 改为对应语义色的面板，并使用反白字体提高视觉对比度与语义清晰度。

## Decision

对 `web/src/App.vue` 中的 Issues KPI 统计卡片与 Traces KPI 统计卡片统一升级视觉样式：
1. **需关注 / 已优化**：采用中性深石色语义面板（`bg-stone-800`，边框 `border-stone-700`），文本与数字使用纯白反白字体（`text-white`、`text-stone-200`、`text-stone-300`）。
2. **待处理 / 缺陷**：采用高对比度危险语义面板（`bg-rose-950/80`，激活时深红实色渐强，边框 `border-rose-700/80`），数字反白（`text-white`），标题与副文本反白高对比（`text-rose-200`、`text-rose-300`）。
3. **处理中 / 待评估**：采用警戒警告语义面板（`bg-amber-950/80`，边框 `border-amber-700/80`），数字反白（`text-white`），副文本（`text-amber-200`、`text-amber-300`）。
4. **已解决 / 标杆**：采用成功语义面板（`bg-emerald-950/80`，边框 `border-emerald-700/80`），数字反白（`text-white`），副文本（`text-emerald-200`、`text-emerald-300`）。
5. 保持原有 a11y 属性（`tabindex="0"`、`role="button"`、键盘事件 Enter/Space）与过滤联动逻辑完全不变，确保契约零破坏。
6. 构建前端产物（`pnpm run build`）并将 release 二进制重新编译注入或验证构建。

## Alternatives considered

- **仅更改数字颜色为纯白，保留浅暗灰背景**：未满足“对应语义色的面板”要求，语义传达仍然微弱。
- **使用亮色浅底+黑字**：与全局深色暗黑控制台风格冲突剧烈，造成视觉刺眼与割裂。

## Consequences

- 统计 KPI 卡片在默认态与选中态下均具备鲜明语义色底衬与反白字体，扫读效率显著提升。
- a11y、键盘交互与 E2E 契约全部保留。
