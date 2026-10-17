# Agent Note: surface trace interrupt spans and collapsible tool calls in the web console

Status: implemented

## Problem

Web 控制台的 Trace 详情抽屉（`web/src/App.vue`）只渲染 `turns[]` 的模型、tokens、
对话文本与工具调用的三个扁平字段，存在两处信息缺失：

1. **interrupt span 完全不可见**：run 终态 trace 的 `turn.interrupt`
   （`{source, reason, query, user_choice}`）已真实落库，但 UI 没有任何渲染分支，
   HITL 中断的触发源、原因、向用户提出的问题与候选选项在控制台上无处可查。
2. **工具调用不可展开**：`tool_calls[]` 只显示 `tool_name` / `status` / `duration_ms` /
   `arguments_summary`（单行 `truncate`）/ `error`，缺少 `output`（工具响应全文），
   长参数只能在列表内被截断，排障时必须另去查原始 `payload` JSON。
3. **历史存量结构不兼容**：早期 trace 的工具调用用 `call_id`（而非 `tool_call_id`）、
   状态用 `"done"`（而非 `"success"`）。旧实现用 `call.status === 'success'` 三态硬判定，
   把 `"done"` 一律渲染成失败色（rose），误导排障。

## Decision

只改展示层，最小闭包落在 `web/src/App.vue`：

1. **interrupt 结构化区块**：在 turns 循环内、tokens 行之后新增
   `v-if="turn.interrupt && typeof turn.interrupt === 'object'"` 的琥珀警示块
   （`border-amber-900/60 bg-amber-950/20`）。逐项渲染：`source`（小徽标）、`reason`、
   `query`（`<pre class="whitespace-pre-wrap break-all">` 全文 + `max-h-60 overflow-y-auto no-scrollbar`）、
   `user_choice`（仅当为数组时按 `<ol>` 序号列出）。字段缺失时该行不渲染；
   全部值经 `stringifySpanValue()` 归一，杜绝 `undefined` / `[object Object]`。
2. **tool_calls 折叠展开**：每条调用改为 `<details class="group ...">` + `<summary>`，
   **默认折叠**（不加 `open`）。summary 内：左侧 `> tool_name`、右侧状态 + 耗时与
   `ChevronRight`（`group-open:rotate-90`），并在下一行给出 `arguments_summary` 的
   单行 `truncate` 预览使折叠态可辨认。展开体依次给出完整 `arguments_summary`（`<pre>`）、
   `output` 全文、`error`，以及 `call_id` 归因行。
3. **存量兼容读取**：`toolCallId()` 用 `call.tool_call_id || call.call_id`；
   `toolCallStatusClass()` 把 `success`/`ok`/`done` 判为成功色（emerald）、
   `error`/`failed` 判为失败色（rose）、其余为中性色（stone），缺失状态显示 `unknown`。
   所有读取路径对非对象/缺字段短路，旧数据不抛错、不渲染崩。
4. 复用既有样式令牌与交互范式（`text-[11px] font-mono`、`no-scrollbar`、
   与对话文本块同形的 `details/group/summary` 折叠机制），不新增依赖。

## Alternatives considered

- **做成可交互的 HITL 卡片（在控制台内直接作答）**：控制台是只读观测面，
  作答需要回写 run 状态与鉴权链路，属于新功能而非展示补充；本次明确只要「能展示」，
  故仅做只读结构化明细，交互能力留待后续独立提案。
- **工具调用保持扁平、仅追加 `output` 字段**：长参数与长输出会把 turns 列表撑爆，
  破坏一屏扫读；折叠展开才能兼顾概览与全文。
- **对 `status` 维持三态硬判定（`=== 'success'`）**：会把历史 `"done"` 状态误染色，
  违背「旧数据容错」要求；改为成功/失败/中性三分类的白名单判定。
- **在 Rust 侧新增渲染字段或改 payload schema**：本任务边界禁止改 `crates/**`，
  且落库结构已满足展示需要，UI 侧兼容读取即可。

## Consequences

- Trace 抽屉可完整看到 HITL 中断的 `source`/`reason`/`query`/`user_choice` 与
  每次工具调用的参数、输出、错误，排障无需回到原始 payload JSON。
- 折叠态默认收起，长参数/长输出不再撑爆 turns 列表；展开后为全文（含换行保留）。
- 历史 trace（`call_id` + `"done"`）与现行 trace（`tool_call_id` + `"success"`）
  同一套渲染路径，状态配色不再误判。
- `web/dist/index.html` 由 `vite-plugin-singlefile` 产出后，需重新 `cargo build --release`
  并由 `crates/server/src/routes.rs` 的 `include_str!` 编译期嵌入才生效——
  单纯构建前端不会改变线上页面，部署必须走「前端构建 → Rust 重编 → 安装重启」全链。
- 该区块的数据契约依赖 trace 上报侧持续提供 `turn.interrupt` / `turn.tool_calls`；
  UI 侧对缺失字段全部降级为空不渲染，不反向约束上报 schema。
