// 契约 .dev-team/contract-console-timeout.md (ver 2) 的共享可复用件。
// 本文件不参与 `tests/*.test.js` glob；只导出纯函数与只读扫描器。
//
// 设计原则（R4 / R7）：
// - 解析结果 null 表示「无真值或畸形」，由调用方 fail-closed 判负，绝不兜底放行。
// - 仓库级扫描遇零匹配 / 畸形参数 / 目录不可读一律 throw，测试侧即为非零退出。

import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

/** 契约冻结的余量下限与上界（与 nfr-baseline.json 一致）。 */
export const CONTRACT_MIN_MARGIN_MS = 2000
export const CONTRACT_MAX_MS = 15000

/** 契约 R4 的正则：捕获组必须整体是十进制整数。 */
export const ACQUIRE_CALL_RE = /acquire_timeout\(\s*Duration::from_millis\(\s*([^)]*)\)\s*\)/

const DECIMAL_INT_RE = /^[0-9]+$/

/**
 * 逐处检查文本中的所有 acquire_timeout(Duration::from_millis(...))。
 * @param {unknown} sourceText
 * @returns {{ matched: boolean, malformed: boolean, values: number[] }}
 *   matched=false 表示文本里根本没有该调用；malformed=true 表示有调用但参数非十进制整数。
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
 * 契约 R4 的仓库级真值：crates/**.rs 全集内所有 acquire_timeout 的 max。
 * 零 *.rs / 零匹配 / 畸形参数 / 文件不可读 → throw（禁止静默通过）。
 * @param {string} rootDir
 * @returns {{ value: number, candidates: {file: string, value: number}[], scannedFiles: number }}
 */
export function readAcquireTimeoutMaxFromRepo(rootDir) {
  const files = listRustSourceFiles(rootDir)
  if (files.length === 0) {
    throw new Error(`fail-closed: ${rootDir} 下未找到任何 *.rs 源文件`)
  }
  const candidates = []
  for (const file of files) {
    const source = readFileSync(file, 'utf8')
    const { malformed, values } = inspectAcquireTimeouts(source)
    if (malformed) {
      throw new Error(
        `fail-closed: ${file} 中 acquire_timeout(Duration::from_millis(...)) 参数非十进制整数`,
      )
    }
    for (const value of values) candidates.push({ file, value })
  }
  if (candidates.length === 0) {
    throw new Error(
      `fail-closed: ${rootDir} 下未解析到任何 acquire_timeout(Duration::from_millis(N))`,
    )
  }
  const value = Math.max(...candidates.map((c) => c.value))
  return { value, candidates, scannedFiles: files.length }
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
