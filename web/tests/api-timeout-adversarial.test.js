// 红军对抗测试（L2-AT）· 契约 .dev-team/contract-console-timeout.md (ver 2) 的 R7。
// 确定性优先：不使用任何定时等待、不掷骰子、不重试；全部为纯函数与源码解析断言。
// 任何夹具的「判负」都落到断言失败（非零退出），不做跳过或告警放行。

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
  TIMER_FN,
  acquireDriftViolations,
  collectAcquireSites,
  inspectAcquireTimeouts,
  inspectAppTimeoutWiring,
  inspectLoadFailurePriority,
  isConsoleTimeoutWithinContract,
  listRustSourceFiles,
  parseAcquireTimeoutMax,
  readAcquireSitesFromRepo,
  readAcquireTimeoutMaxFromRepo,
} from './_api-timeout-helpers.mjs'

const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url))
const WEB_ROOT = join(REPO_ROOT, 'web')
const TESTS_DIR = join(WEB_ROOT, 'tests')
const CRATES_DIR = join(REPO_ROOT, 'crates')

const acquire = readAcquireTimeoutMaxFromRepo(CRATES_DIR)
const A = acquire.value

// 夹具文本用拼接构造，避免本文件自身被「禁止沉睡/禁止随机」自检误命中。
const CALL_PREFIX = 'acquire_timeout(Duration::from_millis('

/** 生成一处完整调用文本。 */
function callBody(argument) {
  return `${CALL_PREFIX}${argument}))`
}

test(`R7 解析器缺失 #1：真实仓库解析结果非 null 且等于 3000（实测 A=${A}）`, () => {
  // Arrange / Act
  const result = readAcquireTimeoutMaxFromRepo(CRATES_DIR)

  // Assert
  assert.notEqual(result.value, null, '真实仓库解析结果不得为 null')
  assert.equal(result.value, 3000, '当前 main.rs 与 db.rs 两处均为 3000')
  assert.equal(A, 3000, `边界夹具基准 A 必须为 3000，实测 ${A}`)
})

test(`R7 边界：${A + CONTRACT_MIN_MARGIN_MS}（= A+2000，A=${A}）判负`, () => {
  // Arrange
  const candidate = A + CONTRACT_MIN_MARGIN_MS

  // Act
  const verdict = isConsoleTimeoutWithinContract(candidate, A)

  // Assert
  assert.equal(verdict, false, `余量恰为 2000 的 ${candidate} 必须判负（R2 严格不等式）`)
})

test(`R7 边界：${A + CONTRACT_MIN_MARGIN_MS + 1}（= A+2001，A=${A}）判正`, () => {
  // Arrange
  const candidate = A + CONTRACT_MIN_MARGIN_MS + 1

  // Act
  const verdict = isConsoleTimeoutWithinContract(candidate, A)

  // Assert
  assert.equal(verdict, true, `余量 2001 的 ${candidate} 必须判正（最小合法值）`)
})

test(`R7 边界：${CONTRACT_MAX_MS} 判正（R3 闭区间上界）`, () => {
  // Arrange
  const candidate = CONTRACT_MAX_MS

  // Act
  const verdict = isConsoleTimeoutWithinContract(candidate, A)

  // Assert
  assert.equal(verdict, true, `${candidate} 是 R3 明文允许的闭合上界，必须判正`)
})

test(`R7 边界：${CONTRACT_MAX_MS + 1} 判负（超 R3 上界）`, () => {
  // Arrange
  const candidate = CONTRACT_MAX_MS + 1

  // Act
  const verdict = isConsoleTimeoutWithinContract(candidate, A)

  // Assert
  assert.equal(verdict, false, `${candidate} 超出上界必须判负`)
})

test('R7 边界补充：事故原型 3000 与 0 / 非整数一律判负', () => {
  // Arrange
  const candidates = [0, 3000, 4999, Number.NaN, 8000.5, '8000']

  // Act
  const verdicts = candidates.map((candidate) => isConsoleTimeoutWithinContract(candidate, A))

  // Assert
  assert.deepEqual(verdicts, [false, false, false, false, false, false])
})

test('R7 畸形输入：非十进制参数必须解析为 null（调用方判负）', () => {
  // Arrange
  const malformedArguments = ['abc', '', '-1', '3.5', '1_000', '0x10']

  // Act
  const parsed = malformedArguments.map((argument) => parseAcquireTimeoutMax(callBody(argument)))

  // Assert
  assert.deepEqual(parsed, malformedArguments.map(() => null))
})

test('R7 畸形输入：畸形调用必须被 inspect 标记为 malformed（不是静默无匹配）', () => {
  // Arrange
  const source = callBody('abc')

  // Act
  const info = inspectAcquireTimeouts(source)

  // Assert
  assert.equal(info.matched, true, '必须识别出调用的存在')
  assert.equal(info.malformed, true, '非十进制参数必须标记 malformed，禁止当作无匹配放行')
})

test('R7 畸形/缺失：裸片段与空文本解析结果均为 null', () => {
  // Arrange
  const sources = ['from_millis(abc)', 'from_millis()', 'from_millis(-1)', '', '// 无此调用']

  // Act
  const parsed = sources.map((source) => parseAcquireTimeoutMax(source))

  // Assert
  assert.deepEqual(parsed, sources.map(() => null))
})

test('R7 解析器缺失：main.rs 文本内无匹配 → null', () => {
  // Arrange：构造一段真实的 main.rs 片段形态，但完全没有 acquire_timeout 调用
  const source = [
    'async fn main() {',
    '    let pool = PgPoolOptions::new()',
    '        .max_connections(10)',
    '        .connect(&dsn).await?;',
    '    Ok(())',
    '}',
  ].join('\n')

  // Act
  const parsed = parseAcquireTimeoutMax(source)

  // Assert
  assert.equal(parsed, null, '无匹配必须返回 null，由调用方判负')
})

test('R7 解析器缺失 fail-closed：零候选 / 目录不可读时仓库扫描必须 throw', () => {
  // Arrange：web/ 下没有任何 *.rs（零匹配）；再取一个不存在的目录（不可读）
  assert.deepEqual(listRustSourceFiles(join(WEB_ROOT, 'src', 'lib')).filter((f) => f.endsWith('.rs')), [])

  // Act + Assert
  assert.throws(() => readAcquireTimeoutMaxFromRepo(WEB_ROOT), /fail-closed/)
  assert.throws(() => readAcquireTimeoutMaxFromRepo(join(REPO_ROOT, 'crates', '__no_such_dir__')), /ENOENT|no such file/)
})

test('R7 对抗纯度：web/tests/ 内零定时等待、零随机、零跳过', () => {
  // Arrange：模式片段拼接构造，避免本文件自我命中
  const patterns = [
    ['slee', 'p\\('].join(''),
    ['set', 'Timeout'].join(''),
    ['Math', '\\.random'].join(''),
    ['\\.skip', '\\('].join(''),
  ]

  // Act + Assert
  for (const pattern of patterns) {
    const probe = spawnSync('grep', ['-rnE', pattern, TESTS_DIR], { encoding: 'utf8' })
    assert.equal(probe.error, undefined, `grep 不可用：${probe.error && probe.error.message}`)
    assert.equal(probe.status, 1, `命中禁止模式（${pattern}）：\n${probe.stdout}`)
    assert.equal(probe.stdout, '', `命中禁止模式（${pattern}）：\n${probe.stdout}`)
  }
})

test('R7 边界闭包：真实 API_TIMEOUT_MS 落在 (A+2000, 15000] 内', async () => {
  // Arrange
  let API_TIMEOUT_MS = null

  // Act + Assert（一）：模块必须存在，缺失即判负
  try {
    const mod = await import('../src/lib/apiTimeout.js')
    API_TIMEOUT_MS = mod.API_TIMEOUT_MS
  } catch (error) {
    assert.fail(
      `无法加载 web/src/lib/apiTimeout.js：${error.code || error.name} ${error.message}`,
    )
  }
  assert.equal(typeof API_TIMEOUT_MS, 'number', `API_TIMEOUT_MS 必须是 number，实测 ${typeof API_TIMEOUT_MS}`)

  // Assert（二）
  assert.equal(
    isConsoleTimeoutWithinContract(API_TIMEOUT_MS, A),
    true,
    `真实 API_TIMEOUT_MS=${API_TIMEOUT_MS} 必须 > A+2000=${A + CONTRACT_MIN_MARGIN_MS} 且 <= ${CONTRACT_MAX_MS}`,
  )

  // Assert（三）：真实常量必须是实际可读的源文件，不是运行时拼接的假值
  const source = readFileSync(join(WEB_ROOT, 'src', 'lib', 'apiTimeout.js'), 'utf8')
  assert.match(source, /export const API_TIMEOUT_MS\b/)
})

// ---------------------------------------------------------------------------
// F1 对抗：R1 接线形态（合成字符串正反例，不 mutate 真实源码）
// ---------------------------------------------------------------------------

/** 合成一段 App.vue 形态的文本。 */
function syntheticAppVue({
  healthzArg = 'API_TIMEOUT_MS',
  defaultParam = 'API_TIMEOUT_MS',
  extraRefs = [],
  importLine = "import { API_TIMEOUT_MS } from '@/lib/apiTimeout'",
} = {}) {
  return [
    "import { ref } from 'vue'",
    importLine,
    '',
    'async function apiFetchJson(url, options = {}) {',
    `  const { timeoutMs = ${defaultParam}, refreshScope = false } = options`,
    '  return url',
    '}',
    '',
    'function startHealthCheck() {',
    `  const timeoutId = ${TIMER_FN}(() => healthController.abort(), ${healthzArg})`,
    '  return timeoutId',
    '}',
    ...extraRefs.map((ref) => `void ${ref}`),
  ].join('\n')
}

test('F1 正例：合规 App.vue 形态 → 零违规（判据不误伤正确实现）', () => {
  // Arrange
  const source = syntheticAppVue()

  // Act
  const wiring = inspectAppTimeoutWiring(source)

  // Assert
  assert.deepEqual(wiring.violations, [], `合规写法被判负：\n- ${wiring.violations.join('\n- ')}`)
  assert.equal(wiring.healthzTimerCount, 1)
  assert.equal(wiring.defaultParamCount, 1)
  assert.equal(wiring.importCount, 1)
  assert.deepEqual(wiring.unexpected, [])
})

test('F1 反例（死引用凑数）：healthz 写死 8000 + 插入 void API_TIMEOUT_MS → 必须判负', () => {
  // Arrange：出现次数仍为 3（import + 默认参数 + void），旧判据会全绿
  const source = syntheticAppVue({ healthzArg: '8000', extraRefs: ['API_TIMEOUT_MS'] })
  const referenceCount = (source.match(/API_TIMEOUT_MS/g) || []).length

  // Act
  const wiring = inspectAppTimeoutWiring(source)

  // Assert
  assert.equal(referenceCount, 3, '反例必须先满足「出现 3 次」以证明旧判据失效')
  assert.notDeepEqual(wiring.violations, [], '死引用凑数必须被检出')
  assert.equal(wiring.healthzTimerCount, 0, 'healthz 未复用常量必须计数为 0')
  assert.equal(wiring.numericAbortCount, 1, 'abort(), <数字> 必须被计数')
  assert.equal(wiring.healthzNumericAbortCount, 1, 'healthController.abort(), <数字> 必须被计数')
  assert.equal(wiring.unexpected.length, 1, '合法三处用途之外的引用必须被列出')
  assert.match(wiring.violations.join('\n'), /healthz 探活必须恰 1 处/)
  assert.match(wiring.violations.join('\n'), /疑似死引用凑数/)
})

test('F1 反例（第二来源 / 默认参数写死）：import 重复或 timeoutMs 写死数字 → 必须判负', () => {
  // Arrange
  const duplicatedImport = syntheticAppVue({
    importLine: [
      "import { API_TIMEOUT_MS } from '@/lib/apiTimeout'",
      "import { API_TIMEOUT_MS as ALT } from '@/lib/apiTimeout'",
    ].join('\n'),
  })
  const hardcodedDefault = syntheticAppVue({ defaultParam: '8000' })
  const relativeImport = syntheticAppVue({ importLine: "import { API_TIMEOUT_MS } from './lib/apiTimeout.js'" })

  // Act
  const dup = inspectAppTimeoutWiring(duplicatedImport)
  const hardcoded = inspectAppTimeoutWiring(hardcodedDefault)
  const relative = inspectAppTimeoutWiring(relativeImport)

  // Assert
  assert.notDeepEqual(dup.violations, [], '重复 import 必须判负')
  assert.match(dup.violations.join('\n'), /import 来源必须唯一/)
  assert.notDeepEqual(hardcoded.violations, [], '默认参数写死数字必须判负')
  assert.match(hardcoded.violations.join('\n'), /默认参数必须恰 1 处/)
  assert.notDeepEqual(relative.violations, [], "非 '@/lib/apiTimeout' 的 import 必须判负")
  assert.match(relative.violations.join('\n'), /@\/lib\/apiTimeout/)
})

// ---------------------------------------------------------------------------
// F2 对抗：R8 站点级漂移（合成站点正反例）
// ---------------------------------------------------------------------------

test('F2 正例：两站点均 3000 → 零漂移', () => {
  // Arrange
  const sources = [
    { file: 'crates/server/src/main.rs', text: callBody('3000') },
    { file: 'crates/core/src/db.rs', text: callBody('3000') },
  ]

  // Act
  const collected = collectAcquireSites(sources)
  const drift = acquireDriftViolations(collected.sites, ACQUIRE_BASELINE_MS)

  // Assert
  assert.equal(collected.sites.length, 2)
  assert.equal(collected.max, 3000)
  assert.deepEqual(drift, [])
})

test('F2 反例（单点下调 2500）：max 仍为 3000，但站点级漂移必须判负并带文件路径', () => {
  // Arrange
  const sources = [
    { file: 'crates/server/src/main.rs', text: callBody('3000') },
    { file: 'crates/core/src/db.rs', text: callBody('2500') },
  ]

  // Act
  const collected = collectAcquireSites(sources)
  const drift = acquireDriftViolations(collected.sites, ACQUIRE_BASELINE_MS)

  // Assert：先证明旧的 max 判据会全绿
  assert.equal(collected.max, 3000, '单点下调后 max 仍为 3000 —— 旧判据在此处失效')
  assert.match(ACQUIRE_DRIFT_MESSAGE, /acquire_timeout 漂移：须重走契约冻结并复核 API_TIMEOUT_MS/)
  assert.deepEqual(drift, [{ file: 'crates/core/src/db.rs', value: 2500 }], '必须精确指出漂移站点')
})

test('F2 反例（单点上调 4000）：max 变为 4000，站点级漂移同样判负', () => {
  // Arrange
  const sources = [
    { file: 'crates/server/src/main.rs', text: callBody('4000') },
    { file: 'crates/core/src/db.rs', text: callBody('3000') },
  ]

  // Act
  const collected = collectAcquireSites(sources)
  const drift = acquireDriftViolations(collected.sites, ACQUIRE_BASELINE_MS)

  // Assert
  assert.equal(collected.max, 4000)
  assert.deepEqual(drift, [{ file: 'crates/server/src/main.rs', value: 4000 }])
})

test('F2 真实仓库站点清单：逐站点核对并输出清单（当前 2 站点均 3000）', () => {
  // Arrange
  const repo = readAcquireSitesFromRepo(CRATES_DIR)

  // Act
  const siteList = repo.sites.map((site) => `${site.file.replace(`${REPO_ROOT}/`, '')}=${site.value}`)

  // Assert
  assert.equal(repo.sites.length, 2, `站点数应为 2，实测 ${repo.sites.length}：${siteList.join(', ')}`)
  for (const site of repo.sites) {
    assert.equal(
      site.value,
      ACQUIRE_BASELINE_MS,
      `${ACQUIRE_DRIFT_MESSAGE}（站点 ${site.file} 实测 ${site.value}；清单：${siteList.join(', ')}）`,
    )
  }
})

// ---------------------------------------------------------------------------
// F3 对抗：R5 分支优先级（合成字符串对调正反例）
// ---------------------------------------------------------------------------

/** 合成一段 loadError.js 形态的文本，branches 决定分支顺序。 */
function syntheticLoadError(branches) {
  const body = branches.map((kind) => `  if (kinds.has('${kind}')) {\n    return '${kind}'\n  }`)
  return ['export function loadFailureDetail(failures) {', '  const kinds = new Set(Object.values(failures || {}))', ...body, "  return ''", '}', ''].join('\n')
}

test('F3 正例：auth > timeout > server > network → 零违规', () => {
  // Arrange
  const source = syntheticLoadError(['auth', 'timeout', 'server', 'network'])

  // Act
  const priority = inspectLoadFailurePriority(source)

  // Assert
  assert.deepEqual(priority.violations, [])
  assert.deepEqual(priority.order, ['auth', 'timeout', 'server', 'network'])
})

test('F3 反例（auth/timeout 对调）：行为矩阵不变但源码顺序违规，必须判负', () => {
  // Arrange
  const source = syntheticLoadError(['timeout', 'auth', 'server', 'network'])

  // Act
  const priority = inspectLoadFailurePriority(source)

  // Assert
  assert.deepEqual(priority.order, ['timeout', 'auth', 'server', 'network'])
  assert.notDeepEqual(priority.violations, [], '分支对调必须被顺序判据抓到')
  assert.match(priority.violations.join('\n'), /第 1 位应为 auth/)
})

test('F3 反例（server 提到 timeout 之前）：必须判负', () => {
  // Arrange
  const source = syntheticLoadError(['auth', 'server', 'timeout', 'network'])

  // Act
  const priority = inspectLoadFailurePriority(source)

  // Assert
  assert.notDeepEqual(priority.violations, [])
  assert.match(priority.violations.join('\n'), /第 2 位应为 timeout/)
})

test('F3 真实源码：loadError.js 分支顺序符合契约', () => {
  // Arrange
  const source = readFileSync(join(WEB_ROOT, 'src', 'lib', 'loadError.js'), 'utf8')

  // Act
  const priority = inspectLoadFailurePriority(source)

  // Assert
  assert.deepEqual(priority.violations, [], priority.violations.join('\n'))
  assert.deepEqual(priority.order, ['auth', 'timeout', 'server', 'network'])
})
