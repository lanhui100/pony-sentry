// 蓝军功能契约测试（L2）· 契约 .dev-team/contract-console-timeout.md (ver 2)
// 覆盖 R1 / R2 / R3 / R4 / R5 / R8（站点级漂移）。实现未落地时本文件必须为红。
//
// 零假测试约束：每个用例都有真实断言；不 mock fetch / 定时器做「模拟超时」；
// 所有 fail 分支都是断言失败或抛错，不使用跳过或告警放行。
// 判据全部来自 _api-timeout-helpers.mjs 的纯函数（对抗用例用合成字符串做正反例）。

import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { join } from 'node:path'

import {
  ACQUIRE_BASELINE_MS,
  ACQUIRE_DRIFT_MESSAGE,
  CONTRACT_MAX_MS,
  CONTRACT_MIN_MARGIN_MS,
  acquireDriftViolations,
  inspectAppTimeoutWiring,
  inspectLoadFailurePriority,
  isConsoleTimeoutWithinContract,
  readAcquireSitesFromRepo,
} from './_api-timeout-helpers.mjs'

const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url))
const WEB_ROOT = join(REPO_ROOT, 'web')
const TESTS_DIR = join(WEB_ROOT, 'tests')
const CRATES_DIR = join(REPO_ROOT, 'crates')
const API_TIMEOUT_SOURCE_PATH = join(WEB_ROOT, 'src', 'lib', 'apiTimeout.js')
const APP_VUE_SOURCE_PATH = join(WEB_ROOT, 'src', 'App.vue')
const LOAD_ERROR_SOURCE_PATH = join(WEB_ROOT, 'src', 'lib', 'loadError.js')
const NFR_BASELINE_PATH = join(REPO_ROOT, '.dev-team', 'nfr-baseline.json')
const NFR_BASELINE_KEY = 'console_api_timeout_ms'

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

test('R4 服务端真值：递归解析 crates/**/*.rs 的每一处 acquire_timeout，并输出站点清单', () => {
  // Arrange
  const expectedSites = 2

  // Act
  const repo = readAcquireSitesFromRepo(CRATES_DIR)
  const siteList = repo.sites.map((site) => `${site.file.replace(`${REPO_ROOT}/`, '')}=${site.value}`)

  // Assert
  assert.ok(repo.scannedFiles > 0, '必须真的扫描到 *.rs 文件')
  assert.equal(
    repo.sites.length,
    expectedSites,
    `acquire_timeout 站点数应为 ${expectedSites}（main.rs + db.rs），实测 ${repo.sites.length}：${siteList.join(', ')}`,
  )
  assert.ok(
    repo.sites.some((site) => site.file.endsWith(join('crates', 'server', 'src', 'main.rs'))),
    `站点清单缺少 crates/server/src/main.rs：${siteList.join(', ')}`,
  )
  assert.ok(
    repo.sites.some((site) => site.file.endsWith(join('crates', 'core', 'src', 'db.rs'))),
    `站点清单缺少 crates/core/src/db.rs（AMB-3 裁决要求纳入绑定）：${siteList.join(', ')}`,
  )
  assert.equal(
    repo.max,
    Math.max(...repo.sites.map((site) => site.value)),
    'A 必须等于全部站点取值的 max',
  )
  assert.ok(Number.isSafeInteger(repo.max) && repo.max > 0, `A 必须是正整数，实测 ${repo.max}`)
})

test('R8 站点级漂移：每个站点的取值都必须 === 3000（单点下调也必须被抓到）', () => {
  // Arrange
  const repo = readAcquireSitesFromRepo(CRATES_DIR)
  const siteList = repo.sites.map((site) => `${site.file.replace(`${REPO_ROOT}/`, '')}=${site.value}`)

  // Act
  const drift = acquireDriftViolations(repo.sites, ACQUIRE_BASELINE_MS)

  // Assert
  assert.deepEqual(
    drift,
    [],
    `${ACQUIRE_DRIFT_MESSAGE}；实测站点清单：${siteList.join(', ')}；偏离站点：${
      drift.map((site) => `${site.file}=${site.value}`).join(', ') || '<无>'
    }`,
  )
  // 每个站点逐条断言，保证失败信息精确到文件路径
  for (const site of repo.sites) {
    assert.equal(
      site.value,
      ACQUIRE_BASELINE_MS,
      `${ACQUIRE_DRIFT_MESSAGE}（站点 ${site.file} 实测 ${site.value}）`,
    )
  }
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
  const acquire = readAcquireSitesFromRepo(CRATES_DIR)
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
    API_TIMEOUT_MS > acquire.max + minMargin,
    `R2 严格下界不成立：API_TIMEOUT_MS=${API_TIMEOUT_MS} 必须 > A(${acquire.max}) + ${minMargin} = ${acquire.max + minMargin}`,
  )
  assert.ok(API_TIMEOUT_MS <= maxMs, `R3 上界不成立：API_TIMEOUT_MS=${API_TIMEOUT_MS} 必须 <= ${maxMs}`)
  assert.equal(
    isConsoleTimeoutWithinContract(API_TIMEOUT_MS, acquire.max),
    true,
    '真实常量必须落在契约闭区间 (A+2000, 15000] 内',
  )
})

test('R2/R3 基准表：nfr-baseline.json#console_api_timeout_ms 取值与预算下限冻结', () => {
  // Arrange
  const baseline = JSON.parse(readSource(NFR_BASELINE_PATH))
  const acquire = readAcquireSitesFromRepo(CRATES_DIR)

  // Act
  const entry = baseline[NFR_BASELINE_KEY]

  // Assert
  assert.ok(entry && typeof entry === 'object', `缺少 #${NFR_BASELINE_KEY} 条目`)
  assert.equal(entry.value, 8000, 'R2 基准取值必须为 8000')
  assert.equal(entry.min_margin_over_acquire_ms, 2000, 'R2 仅声明预算下限 2000')
  assert.equal(entry.max_ms, 15000, 'R3 上界必须为 15000')
  assert.equal(
    isConsoleTimeoutWithinContract(entry.value, acquire.max),
    true,
    `基准值 ${entry.value} 必须满足严格下界 A+2000=${acquire.max + 2000}`,
  )
})

test('R1 单一来源：apiTimeout.js 恰有 1 处 export const API_TIMEOUT_MS', () => {
  // Arrange
  const source = readSource(API_TIMEOUT_SOURCE_PATH)

  // Act
  const exportMatches = source.match(/export const API_TIMEOUT_MS\b/g) || []

  // Assert
  assert.equal(
    exportMatches.length,
    1,
    `web/src/lib/apiTimeout.js 中 export const API_TIMEOUT_MS 应恰 1 处，实测 ${exportMatches.length}`,
  )
})

test('R1 App.vue 静态判据：无裸数字 3000、无本地定义、出现次数恰 3', () => {
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

test('R1 App.vue 接线形态：healthz 复用常量、默认参数正确、无 abort(),<数字>、import 唯一', () => {
  // Arrange
  const source = readSource(APP_VUE_SOURCE_PATH)

  // Act
  const wiring = inspectAppTimeoutWiring(source)

  // Assert
  assert.deepEqual(
    wiring.violations,
    [],
    `App.vue 的 R1 接线违规：\n- ${wiring.violations.join('\n- ')}`,
  )
  assert.equal(wiring.healthzTimerCount, 1, `healthz 探活复用常量处数应为 1，实测 ${wiring.healthzTimerCount}`)
  assert.equal(wiring.defaultParamCount, 1, `timeoutMs = API_TIMEOUT_MS 处数应为 1，实测 ${wiring.defaultParamCount}`)
  assert.equal(wiring.numericAbortCount, 0, `abort(), <数字> 处数应为 0，实测 ${wiring.numericAbortCount}`)
  assert.equal(
    wiring.healthzNumericAbortCount,
    0,
    `healthController.abort(), <数字> 处数应为 0，实测 ${wiring.healthzNumericAbortCount}`,
  )
  assert.equal(wiring.importCount, 1, `@/lib/apiTimeout import 处数应为 1，实测 ${wiring.importCount}`)
  assert.equal(wiring.anyImportCount, 1, `API_TIMEOUT_MS import 来源应唯一，实测 ${wiring.anyImportCount}`)
  assert.deepEqual(wiring.unexpected, [], `存在合法三处用途之外的引用：${JSON.stringify(wiring.unexpected)}`)
})

test('R5 归因语义：server 5xx 精确文案 + auth > timeout > server > network 优先级矩阵', async () => {
  // Arrange
  const { loadFailureDetail } = await import('../src/lib/loadError.js')

  // Act
  const serverOnly = loadFailureDetail({ issues: 'server' })
  const authOverTimeout = loadFailureDetail({ traces: 'timeout', projects: 'auth' })
  const authOverServer = loadFailureDetail({ issues: 'server', projects: 'auth' })
  const timeoutOverServer = loadFailureDetail({ issues: 'server', traces: 'timeout' })
  const serverOverNetwork = loadFailureDetail({ issues: 'server', projects: 'network' })

  // Assert
  assert.equal(serverOnly, '服务端错误：PonySentry 返回 5xx，请查看服务日志')
  assert.match(authOverTimeout, /鉴权失败/, '{traces:timeout, projects:auth} 必须给出鉴权失败')
  assert.match(authOverServer, /鉴权失败/, '{issues:server, projects:auth} 必须给出鉴权失败')
  assert.match(timeoutOverServer, /请求超时/, '{issues:server, traces:timeout} 必须给出请求超时')
  assert.match(serverOverNetwork, /服务端错误/, '{issues:server, projects:network} 必须给出服务端错误')
})

test('R5 分支顺序判据：loadError.js 内 auth 分支必须先于 timeout / server / network', () => {
  // Arrange
  const source = readSource(LOAD_ERROR_SOURCE_PATH)

  // Act
  const priority = inspectLoadFailurePriority(source)

  // Assert
  assert.deepEqual(
    priority.violations,
    [],
    `loadFailureDetail 优先级违规：\n- ${priority.violations.join('\n- ')}`,
  )
  assert.deepEqual(
    priority.order,
    ['auth', 'timeout', 'server', 'network'],
    `分支顺序必须为 auth > timeout > server > network，实测 ${priority.order.join(' > ')}`,
  )
})

test('R5 既有用例回归：load-error.test.js 行为不变且自身通过（不依赖 git diff 空转）', () => {
  // Arrange + Act
  const run = spawnSync(process.execPath, ['--test', 'tests/load-error.test.js'], {
    cwd: WEB_ROOT,
    encoding: 'utf8',
  })

  // Assert
  assert.equal(run.error, undefined, `无法执行 node --test：${run.error && run.error.message}`)
  assert.equal(run.status, 0, `node --test tests/load-error.test.js 非零退出：\n${run.stdout}\n${run.stderr}`)
})
