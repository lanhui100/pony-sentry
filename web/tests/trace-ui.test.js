// C9.11 控制台渲染验收（red phase）。契约依据：`.dev-team/trace-fix-contract.md` §C8 / C9.11。
// 运行：`cd web && pnpm test`（package.json 的 test 脚本为 `node --test tests/*.test.js`）。
//
// 渲染器说明（客观事实，非裁量）：本包未把 vitest/jsdom 接线进 `package.json` 的 test 脚本，
// 也没有 .vue 加载器（@vitejs/plugin-vue 只在 vite 构建期生效），故这里不做 DOM 挂载，
// 改为对 `src/App.vue` 做**模板级 DOM 契约断言**：断言 C8 要求真实存在于模板/脚本中的绑定
// （徽标绑定 + 颜色档、失败原因区、降级文案）。字段缺失时降级渲染不抛错（C8 末条）由同文件
// 既有 `span`/兼容取值函数保证，属 review 项。断言一律走 `ok(re.test(...))`，避免断言失败时
// 把整份 App.vue 打进日志。

import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const source = readFileSync(join(root, 'src/App.vue'), 'utf8')
/** 压平换行与缩进，避免断言被格式漂移误伤（不改语义）。 */
const flat = source.replace(/[ \t]+/g, ' ').replace(/\n+/g, ' ')
const holds = (re, message) => assert.ok(re.test(flat), message)

test('C9.11 列表行必须渲染 turn_status 徽标（C8 颜色档）', () => {
  holds(/turn_status/, '[C9.11] 列表/抽屉必须消费聚合列的 turn_status 字段')
  holds(/<Badge[^>]{0,160}turn_status/, '[C9.11] 列表行必须用 Badge 渲染 trace.turn_status')
  const tiers = [
    [/(failed|error|cancelled)[\s\S]{0,220}?(red|destructive)/i, 'failed/error/cancelled → 红'],
    [/interrupted[\s\S]{0,220}?(yellow|amber)/i, 'interrupted → 黄'],
    [/(succeeded|completed)[\s\S]{0,220}?(green|emerald)/i, 'succeeded/completed → 绿'],
    [/unknown[\s\S]{0,220}?(gray|grey|stone|zinc|neutral)/i, 'unknown → 灰'],
  ]
  for (const [pattern, label] of tiers) {
    holds(pattern, `[C9.11] turn_status 徽标颜色档缺失：${label}`)
  }
})

test('C9.11 抽屉必须渲染「失败原因」区（diagnostics 三标志）', () => {
  holds(/失败原因/, '[C9.11] 抽屉必须有「失败原因」区标题')
  holds(/diagnostics[\s\S]{0,60}?error_contract/, '[C9.11] 失败原因区必须渲染 diagnostics.error_contract')
  holds(/diagnostics[\s\S]{0,60}?status_conflict/, '[C9.11] 失败原因区必须渲染 diagnostics.status_conflict')
  holds(/diagnostics[\s\S]{0,60}?error_missing/, '[C9.11] 失败原因区必须渲染 diagnostics.error_missing')
  holds(/未上报错误详情/, '[C9.11] error_contract=missing 必须渲染红字文案「未上报错误详情」')
})

test('C9.11 抽屉 Turn 头必须显示 status/attempt_index/degraded，空 error 显式「未上报」', () => {
  holds(/turn\.status|turn\['status'\]|turn\["status"\]/, '[C9.11] 抽屉 Turn 头必须显示 turn.status 徽标')
  holds(/attempt_index/, '[C9.11] 抽屉 Turn 头必须显示 attempt_index')
  holds(/degraded/, '[C9.11] 抽屉 Turn 头必须显示 degraded')
  const placeholders = (flat.match(/未上报/g) || []).length
  assert.ok(
    placeholders >= 2,
    `[C9.11] tool_call.error 为空必须显示「未上报」而非隐藏（占位文案 ${placeholders} 处，需 ≥2：error_contract 与 tool_call.error 各一处）`,
  )
})
