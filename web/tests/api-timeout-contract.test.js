// 蓝军功能契约测试（L2）· 契约 .dev-team/contract-console-timeout.md (ver 2)
// 覆盖 R1 / R2 / R3 / R4 / R5。实现未落地时本文件必须为红：失败是预期行为。
//
// 零假测试约束：每个用例都有真实断言；不 mock fetch / 定时器做「模拟超时」；
// 所有 fail 分支都是断言失败或抛错，不使用跳过或告警放行。

import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { execFileSync, spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { join } from 'node:path'

import {
  CONTRACT_MAX_MS,
  CONTRACT_MIN_MARGIN_MS,
  isConsoleTimeoutWithinContract,
  readAcquireTimeoutMaxFromRepo,
} from './_api-timeout-helpers.mjs'

const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url))
const WEB_ROOT = join(REPO_ROOT, 'web')
const TESTS_DIR = join(WEB_ROOT, 'tests')
const API_TIMEOUT_SOURCE_PATH = join(WEB_ROOT, 'src', 'lib', 'apiTimeout.js')
const APP_VUE_SOURCE_PATH = join(WEB_ROOT, 'src', 'App.vue')
const NFR_BASELINE_PATH = join(REPO_ROOT, '.dev-team', 'nfr-baseline.json')
const NFR_BASELINE_KEY = 'console_api_timeout_ms'
const LEGACY_LOAD_ERROR_TEST = 'web/tests/load-error.test.js'

/** 读取文件；不可读时直接抛出（fail-closed，不 catch 放行）。 */
function readSource(path) {
  return readFileSync(path, 'utf8')
}

/** 动态导入被测常量；模块缺失即断言失败（不 skip、不 warn）。 */
async function importApiTimeoutConstant() {
  try {
    const mod = await import('../src/lib/apiTimeout.js')
    return mod
  } catch (error) {
    assert.fail(
      `无法加载 web/src/lib/apiTimeout.js（R1/R2/R3 依赖的单一来源）：${error.code || error.name} ${error.message}`,
    )
  }
}

test('R4 服务端真值：递归解析 crates/**/*.rs 的全部 acquire_timeout，取 max 为 A', () => {
  // Arrange
  const cratesDir = join(REPO_ROOT, 'crates')

  // Act
  const result = readAcquireTimeoutMaxFromRepo(cratesDir)

  // Assert
  assert.equal(typeof result.value, 'number', 'A 必须是 number')
  assert.ok(Number.isSafeInteger(result.value) && result.value > 0, `A 必须是正整数，实测 ${result.value}`)
  assert.ok(result.scannedFiles > 0, '必须真的扫描到 *.rs 文件')
  assert.ok(result.candidates.length >= 2, `契约要求至少覆盖 main.rs 与 db.rs 两处，实测 ${result.candidates.length} 处`)
  assert.equal(
    result.value,
    Math.max(...result.candidates.map((c) => c.value)),
    'A 必须等于全部候选的 max',
  )
  assert.ok(
    result.candidates.some((c) => c.file.endsWith(join('crates', 'server', 'src', 'main.rs'))),
    '候选中缺少 crates/server/src/main.rs',
  )
  assert.ok(
    result.candidates.some((c) => c.file.endsWith(join('crates', 'core', 'src', 'db.rs'))),
    '候选中缺少 crates/core/src/db.rs（AMB-3 裁决要求纳入绑定）',
  )
})

test('R4 fail-closed 自检：web/tests/ 内不存在任何 skip 放行', () => {
  // Arrange
  const skipPatterns = ['\\.skip\\(', 'test\\.skip', 'describe\\.skip']

  // Act + Assert
  for (const pattern of skipPatterns) {
    const probe = spawnSync('grep', ['-rnE', pattern, TESTS_DIR], { encoding: 'utf8' })
    assert.equal(probe.error, undefined, `grep 不可用：${probe.error && probe.error.message}`)
    assert.equal(probe.status, 1, `命中跳过用例（${pattern}）：\n${probe.stdout}`)
    assert.equal(probe.stdout, '', `命中跳过用例（${pattern}）：\n${probe.stdout}`)
  }
})

test('R2/R3 下界与上界：API_TIMEOUT_MS > A + 2000（严格）且 <= 15000', async () => {
  // Arrange
  const acquire = readAcquireTimeoutMaxFromRepo(join(REPO_ROOT, 'crates'))
  const minMargin = CONTRACT_MIN_MARGIN_MS
  const maxMs = CONTRACT_MAX_MS

  // Act
  const mod = await importApiTimeoutConstant()
  const API_TIMEOUT_MS = mod.API_TIMEOUT_MS

  // Assert
  assert.equal(typeof API_TIMEOUT_MS, 'number', 'API_TIMEOUT_MS 必须是 number')
  assert.ok(Number.isSafeInteger(API_TIMEOUT_MS), `API_TIMEOUT_MS 必须是整数，实测 ${API_TIMEOUT_MS}`)
  assert.ok(API_TIMEOUT_MS > 0, 'API_TIMEOUT_MS 必须 > 0（0 会让浏览器立即 abort）')
  assert.ok(
    API_TIMEOUT_MS > acquire.value + minMargin,
    `R2 严格下界不成立：API_TIMEOUT_MS=${API_TIMEOUT_MS} 必须 > A(${acquire.value}) + ${minMargin} = ${acquire.value + minMargin}`,
  )
  assert.ok(
    API_TIMEOUT_MS <= maxMs,
    `R3 上界不成立：API_TIMEOUT_MS=${API_TIMEOUT_MS} 必须 <= ${maxMs}`,
  )
  assert.equal(
    isConsoleTimeoutWithinContract(API_TIMEOUT_MS, acquire.value),
    true,
    '真实常量必须落在契约闭区间 (A+2000, 15000] 内',
  )
})

test('R2/R3 基准表：nfr-baseline.json#console_api_timeout_ms 取值与预算下限冻结', () => {
  // Arrange
  const baseline = JSON.parse(readSource(NFR_BASELINE_PATH))
  const acquire = readAcquireTimeoutMaxFromRepo(join(REPO_ROOT, 'crates'))

  // Act
  const entry = baseline[NFR_BASELINE_KEY]

  // Assert
  assert.ok(entry && typeof entry === 'object', `缺少 #${NFR_BASELINE_KEY} 条目`)
  assert.equal(entry.value, 8000, 'R2 基准取值必须为 8000')
  assert.equal(entry.min_margin_over_acquire_ms, 2000, 'R2 仅声明预算下限 2000')
  assert.equal(entry.max_ms, 15000, 'R3 上界必须为 15000')
  assert.equal(
    isConsoleTimeoutWithinContract(entry.value, acquire.value),
    true,
    `基准值 ${entry.value} 必须满足严格下界 A+2000=${acquire.value + 2000}`,
  )
})

test('R1 单一来源：apiTimeout.js 恰有 1 处 export const API_TIMEOUT_MS', () => {
  // Arrange
  const source = readSource(API_TIMEOUT_SOURCE_PATH)

  // Act
  const exportMatches = source.match(/export const API_TIMEOUT_MS\b/g) || []

  // Assert
  assert.equal(exportMatches.length, 1, `web/src/lib/apiTimeout.js 中 export const API_TIMEOUT_MS 应恰 1 处，实测 ${exportMatches.length}`)
})

test('R1 App.vue：无裸数字 3000、API_TIMEOUT_MS 恰 3 处用途、不再本地定义常量', () => {
  // Arrange
  const source = readSource(APP_VUE_SOURCE_PATH)

  // Act
  const bare3000 = source.match(/\b3000\b/g) || []
  const references = source.match(/API_TIMEOUT_MS/g) || []
  const localDefinitions = source.match(/const\s+API_TIMEOUT_MS\b/g) || []

  // Assert
  assert.deepEqual(bare3000, [], `App.vue 内仍存在裸数字 3000：${bare3000.length} 处（R1 要求零命中）`)
  assert.equal(
    references.length,
    3,
    `API_TIMEOUT_MS 在 App.vue 应恰 3 处（import + apiFetchJson 默认值 + healthz 探活），实测 ${references.length}`,
  )
  assert.equal(
    localDefinitions.length,
    0,
    `App.vue 不得再本地定义 const API_TIMEOUT_MS，实测 ${localDefinitions.length} 处`,
  )
})

test('R5 归因语义：DB 不可达（server 5xx）必须给出服务端错误文案', async () => {
  // Arrange
  const { loadFailureDetail } = await import('../src/lib/loadError.js')

  // Act
  const serverOnly = loadFailureDetail({ issues: 'server' })
  const timeoutFirst = loadFailureDetail({ issues: 'server', traces: 'timeout' })
  const authFirst = loadFailureDetail({ issues: 'server', projects: 'auth' })

  // Assert
  assert.equal(serverOnly, '服务端错误：PonySentry 返回 5xx，请查看服务日志')
  assert.match(timeoutFirst, /请求超时/)
  assert.match(authFirst, /鉴权失败/)
})

test('R5 既有测试语义冻结：load-error.test.js 相对 HEAD 无改动且自身通过', () => {
  // Arrange + Act + Assert（一）
  let diffFailure = null
  try {
    execFileSync('git', ['diff', '--quiet', 'HEAD', '--', LEGACY_LOAD_ERROR_TEST], {
      cwd: REPO_ROOT,
      stdio: 'pipe',
    })
  } catch (error) {
    diffFailure = error
  }
  assert.equal(diffFailure, null, `${LEGACY_LOAD_ERROR_TEST} 相对 HEAD 被改动（R5 禁止）`)

  // Assert（二）：既有用例自身必须仍为绿（既有优先级语义未被破坏）
  const run = spawnSync(process.execPath, ['--test', 'tests/load-error.test.js'], {
    cwd: WEB_ROOT,
    encoding: 'utf8',
  })
  assert.equal(run.error, undefined, `无法执行 node --test：${run.error && run.error.message}`)
  assert.equal(run.status, 0, `node --test tests/load-error.test.js 非零退出：\n${run.stdout}\n${run.stderr}`)
})
