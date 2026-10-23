// 契约 .dev-team/contract-console-timeout.md (ver 2) 的共享可复用件。
// 本文件不参与 `tests/*.test.js` glob；只导出纯函数与只读扫描器。
//
// 设计原则（R1 / R4 / R5 / R7）：
// - 解析结果 null 表示「无真值或畸形」，由调用方 fail-closed 判负，绝不兜底放行。
// - 所有「文本判据」都是纯函数（入参为文本），便于用合成字符串做正反例单测。
// - 判据返回 violations 数组，空数组 = 合规；调用方只做断言，不在此处吞错。

import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

/** 契约冻结的余量下限与上界（与 nfr-baseline.json 一致）。 */
export const CONTRACT_MIN_MARGIN_MS = 2000
export const CONTRACT_MAX_MS = 15000

/** 契约 R4/R8 的漂移文案（失败断言必须携带）。 */
export const ACQUIRE_DRIFT_MESSAGE = 'acquire_timeout 漂移：须重走契约冻结并复核 API_TIMEOUT_MS'

/** 契约 R2 的基准值与该常量应有的站点取值（当前两处均 3000）。 */
export const ACQUIRE_BASELINE_MS = 3000

/** 契约 R5 的分支优先级（auth > timeout > server > network）。 */
export const LOAD_FAILURE_BRANCH_ORDER = ['auth', 'timeout', 'server', 'network']

// 「定时器函数名」按片段拼接，避免 web/tests/ 的「禁止定时等待」自检自我命中。
export const TIMER_FN = ['set', 'Timeout'].join('')

/** 契约 R4 的正则：捕获组必须整体是十进制整数。 */
export const ACQUIRE_CALL_RE = /acquire_timeout\(\s*Duration::from_millis\(\s*([^)]*)\)\s*\)/

const DECIMAL_INT_RE = /^[0-9]+$/

/** 健康检查探活的合规写法：<timer>(() => healthController.abort(), API_TIMEOUT_MS)。 */
const HEALTHZ_TIMER_RE = new RegExp(
  `${TIMER_FN}\\(\\s*\\(\\s*\\)\\s*=>\\s*healthController\\.abort\\(\\s*\\)\\s*,\\s*API_TIMEOUT_MS\\s*\\)`,
  'g',
)

/** apiFetchJson 的合规默认参数：timeoutMs = API_TIMEOUT_MS。 */
const DEFAULT_PARAM_RE = /timeoutMs\s*=\s*API_TIMEOUT_MS\b/g

/** 违规写法：abort(), <数字>（含 healthController 变体）。 */
const NUMERIC_ABORT_RE = /abort\(\s*\)\s*,\s*\d/g
const HEALTHZ_NUMERIC_ABORT_RE = /healthController\.abort\(\s*\)\s*,\s*\d/g

/** 合规 import：import { API_TIMEOUT_MS } from '@/lib/apiTimeout'。 */
const API_TIMEOUT_IMPORT_RE =
  /import\s*\{[^}]*API_TIMEOUT_MS[^}]*\}\s*from\s*['"]@\/lib\/apiTimeout(?:\.js)?['"]/g
/** 任意来源的 API_TIMEOUT_MS import（用于禁止第二来源）。 */
const ANY_API_TIMEOUT_IMPORT_RE =
  /import\s*\{[^}]*API_TIMEOUT_MS[^}]*\}\s*from\s*['"][^'"]*apiTimeout(?:\.js)?['"]/g

/** 计数工具：返回匹配次数（每次新建正则，不存在 lastIndex 状态残留）。 */
function countMatches(re, text) {
  const globalRe = new RegExp(re.source, re.flags.includes('g') ? re.flags : `${re.flags}g`)
  return text.match(globalRe)?.length ?? 0
}

// ---------------------------------------------------------------------------
// R4：acquire_timeout 解析
// ---------------------------------------------------------------------------

/**
 * 逐处检查文本中的所有 acquire_timeout(Duration::from_millis(...))。
 * @param {unknown} sourceText
 * @returns {{ matched: boolean, malformed: boolean, values: number[] }}
 */
export function inspectAcquireTimeouts(sourceText) {
  if (typeof sourceText !== 'string') return { matched: false, malformed: true, values: [] }
  const re = new RegExp(ACQUIRE_CALL_RE.source, 'g')
  const values = []
  let matched = false
  let malformed = false
  let match
  while ((match = re.exec(sourceText)) !== null) {
    matched = true
    const raw = match[1].trim()
    if (!DECIMAL_INT_RE.test(raw)) {
      malformed = true
      continue
    }
    const value = Number(raw)
    if (!Number.isSafeInteger(value)) {
      malformed = true
      continue
    }
    values.push(value)
  }
  return { matched, malformed, values }
}

/**
 * 单段源码的真值：返回 max；无匹配 / 畸形 / 空文本 → null（调用方必须判负）。
 * @param {string} sourceText
 * @returns {number|null}
 */
export function parseAcquireTimeoutMax(sourceText) {
  const { malformed, values } = inspectAcquireTimeouts(sourceText)
  if (malformed || values.length === 0) return null
  return Math.max(...values)
}

/** 递归列出 rootDir 下所有 *.rs 文件的绝对路径；目录不可读时 throw（fail-closed）。 */
export function listRustSourceFiles(rootDir) {
  const files = []
  const walk = (dir) => {
    const entries = readdirSync(dir, { withFileTypes: true })
    for (const entry of entries) {
      const full = join(dir, entry.name)
      if (entry.isDirectory()) walk(full)
      else if (entry.isFile() && entry.name.endsWith('.rs')) files.push(full)
    }
  }
  walk(rootDir)
  files.sort()
  return files
}

/**
 * 把若干 {file, text} 源码聚合为站点清单（每个 acquire_timeout 一处站点，带文件路径）。
 * 畸形参数 → 记入 malformed（调用方 fail-closed）。
 * @param {{file: string, text: string}[]} sources
 * @returns {{ sites: {file: string, value: number}[], max: number|null, malformed: {file: string}[] }}
 */
export function collectAcquireSites(sources) {
  const sites = []
  const malformed = []
  for (const source of sources) {
    const info = inspectAcquireTimeouts(source.text)
    if (info.malformed) {
      malformed.push({ file: source.file })
      continue
    }
    for (const value of info.values) sites.push({ file: source.file, value })
  }
  const max = sites.length === 0 ? null : Math.max(...sites.map((site) => site.value))
  return { sites, max, malformed }
}

/**
 * 契约 R4/R8 的仓库级站点清单：crates/**.rs 全集内每一处 acquire_timeout 都必须列出。
 * 零 *.rs / 零匹配 / 畸形参数 / 文件不可读 → throw（禁止静默通过）。
 * @param {string} rootDir
 * @returns {{ sites: {file: string, value: number}[], max: number, scannedFiles: number }}
 */
export function readAcquireSitesFromRepo(rootDir) {
  const files = listRustSourceFiles(rootDir)
  if (files.length === 0) {
    throw new Error(`fail-closed: ${rootDir} 下未找到任何 *.rs 源文件`)
  }
  const { sites, malformed } = collectAcquireSites(
    files.map((file) => ({ file, text: readFileSync(file, 'utf8') })),
  )
  if (malformed.length > 0) {
    throw new Error(
      `fail-closed: ${malformed[0].file} 中 acquire_timeout(Duration::from_millis(...)) 参数非十进制整数`,
    )
  }
  if (sites.length === 0) {
    throw new Error(`fail-closed: ${rootDir} 下未解析到任何 acquire_timeout(Duration::from_millis(N))`)
  }
  return { sites, max: Math.max(...sites.map((site) => site.value)), scannedFiles: files.length }
}

/**
 * 契约 R4 的旧口径（max）：保留以兼容既有断言，内部委托站点清单。
 * @param {string} rootDir
 * @returns {{ value: number, candidates: {file: string, value: number}[], scannedFiles: number }}
 */
export function readAcquireTimeoutMaxFromRepo(rootDir) {
  const { sites, max, scannedFiles } = readAcquireSitesFromRepo(rootDir)
  return { value: max, candidates: sites, scannedFiles }
}

/**
 * 契约 R8：任一站点的取值偏离基准即为漂移（单点下调也必须被抓到；max 判据抓不到）。
 * @param {{file: string, value: number}[]} sites
 * @param {number} [expectedMs]
 * @returns {{file: string, value: number}[]}
 */
export function acquireDriftViolations(sites, expectedMs = ACQUIRE_BASELINE_MS) {
  return sites.filter((site) => site.value !== expectedMs).map((site) => ({ ...site }))
}

/**
 * 契约 R2/R3 的纯判定：严格下界 value > acquireMs + minMargin，上界 value <= maxMs。
 * @param {number} value
 * @param {number} acquireMs
 * @param {{ minMargin?: number, maxMs?: number }} [bounds]
 * @returns {boolean}
 */
export function isConsoleTimeoutWithinContract(value, acquireMs, bounds = {}) {
  const { minMargin = CONTRACT_MIN_MARGIN_MS, maxMs = CONTRACT_MAX_MS } = bounds
  if (!Number.isSafeInteger(value) || !Number.isSafeInteger(acquireMs)) return false
  if (value <= 0) return false
  return value > acquireMs + minMargin && value <= maxMs
}

// ---------------------------------------------------------------------------
// R1：App.vue 的单一来源接线（死引用凑数 / 数字写死 均须判负）
// ---------------------------------------------------------------------------

/** 定位 apiFetchJson 声明处，返回其后窗口（用于确认默认参数落在函数内）。 */
function apiFetchJsonWindow(text) {
  const decl = /(?:async\s+)?function\s+apiFetchJson\s*\(/.exec(text)
  if (!decl) return null
  return text.slice(decl.index, decl.index + 4000)
}

/**
 * 契约 R1（加固版）：只数出现次数可被死引用凑数，故逐项核对「用途形态」与唯一来源。
 * @param {unknown} text App.vue 源文本
 * @returns {{
 *   violations: string[],
 *   healthzTimerCount: number,
 *   defaultParamCount: number,
 *   numericAbortCount: number,
 *   healthzNumericAbortCount: number,
 *   importCount: number,
 *   anyImportCount: number,
 *   unexpected: {line: number, text: string}[],
 * }}
 */
export function inspectAppTimeoutWiring(text) {
  if (typeof text !== 'string' || text.length === 0) {
    return {
      violations: ['App.vue 源文本为空或不可读，无法核对 R1 接线'],
      healthzTimerCount: 0,
      defaultParamCount: 0,
      numericAbortCount: 0,
      healthzNumericAbortCount: 0,
      importCount: 0,
      anyImportCount: 0,
      unexpected: [],
    }
  }

  const violations = []
  const healthzTimerCount = countMatches(HEALTHZ_TIMER_RE, text)
  const defaultParamCount = countMatches(DEFAULT_PARAM_RE, text)
  const numericAbortCount = countMatches(NUMERIC_ABORT_RE, text)
  const healthzNumericAbortCount = countMatches(HEALTHZ_NUMERIC_ABORT_RE, text)
  const importCount = countMatches(API_TIMEOUT_IMPORT_RE, text)
  const anyImportCount = countMatches(ANY_API_TIMEOUT_IMPORT_RE, text)

  if (healthzTimerCount !== 1) {
    violations.push(
      `healthz 探活必须恰 1 处写成 ${TIMER_FN}(() => healthController.abort(), API_TIMEOUT_MS)，实测 ${healthzTimerCount} 处`,
    )
  }
  if (defaultParamCount !== 1) {
    violations.push(
      `apiFetchJson 默认参数必须恰 1 处写成 timeoutMs = API_TIMEOUT_MS，实测 ${defaultParamCount} 处`,
    )
  }
  const window = apiFetchJsonWindow(text)
  if (window === null) {
    violations.push('未找到 apiFetchJson 函数声明，无法确认默认参数位置')
  } else if (countMatches(DEFAULT_PARAM_RE, window) !== 1) {
    violations.push('timeoutMs = API_TIMEOUT_MS 未落在 apiFetchJson 的函数体内')
  }
  if (numericAbortCount !== 0) {
    violations.push(`禁止 abort(), <数字> 写法，实测 ${numericAbortCount} 处`)
  }
  if (healthzNumericAbortCount !== 0) {
    violations.push(`禁止 healthController.abort(), <数字> 写法，实测 ${healthzNumericAbortCount} 处`)
  }
  if (importCount !== 1) {
    violations.push(`必须恰 1 处 import { API_TIMEOUT_MS } from '@/lib/apiTimeout'，实测 ${importCount} 处`)
  }
  if (anyImportCount !== 1) {
    violations.push(`API_TIMEOUT_MS 的 import 来源必须唯一（当前 ${anyImportCount} 处）`)
  }

  // 去掉合法三处用途后仍有引用 = 死引用凑数 / 未被识别的第四种用法。
  let stripped = text.replace(API_TIMEOUT_IMPORT_RE, '/* import */')
  stripped = stripped.replace(DEFAULT_PARAM_RE, 'timeoutMs = <constant>')
  stripped = stripped.replace(HEALTHZ_TIMER_RE, '/* healthz timer */')
  const unexpected = []
  stripped.split('\n').forEach((line, index) => {
    if (line.includes('API_TIMEOUT_MS')) unexpected.push({ line: index + 1, text: line.trim() })
  })
  if (unexpected.length > 0) {
    violations.push(
      `存在合法三处用途之外的 API_TIMEOUT_MS 引用（疑似死引用凑数）：${unexpected
        .map((item) => `L${item.line} ${item.text}`)
        .join(' | ')}`,
    )
  }

  return {
    violations,
    healthzTimerCount,
    defaultParamCount,
    numericAbortCount,
    healthzNumericAbortCount,
    importCount,
    anyImportCount,
    unexpected,
  }
}

// ---------------------------------------------------------------------------
// R5：loadFailureDetail 的分支优先级（顺序判据，防分支对调后仍全绿）
// ---------------------------------------------------------------------------

/**
 * 从源码中抽取 `if (kinds.has('<kind>'))` 分支的出现顺序。
 * @param {unknown} text loadError.js 源文本
 * @returns {string[]}
 */
export function inspectLoadFailureBranchOrder(text) {
  if (typeof text !== 'string') return []
  const re = /if\s*\(\s*kinds\.has\(\s*'([a-z]+)'\s*\)\s*\)/g
  const order = []
  let match
  while ((match = re.exec(text)) !== null) order.push(match[1])
  return order
}

/**
 * 契约 R5：分支顺序必须严格等于 auth > timeout > server > network。
 * @param {unknown} text
 * @param {string[]} [expected]
 * @returns {{ order: string[], violations: string[] }}
 */
export function inspectLoadFailurePriority(text, expected = LOAD_FAILURE_BRANCH_ORDER) {
  const order = inspectLoadFailureBranchOrder(text)
  const violations = []
  if (order.length !== expected.length) {
    violations.push(
      `loadFailureDetail 分支数量应为 ${expected.length}，实测 ${order.length}：${order.join(' > ') || '<空>'}`,
    )
  }
  expected.forEach((kind, index) => {
    if (order[index] !== kind) {
      violations.push(
        `loadFailureDetail 分支顺序错误：第 ${index + 1} 位应为 ${kind}，实测 ${
          order[index] ?? '<缺失>'
        }（完整顺序：${order.join(' > ') || '<空>'}）`,
      )
    }
  })
  return { order, violations }
}
