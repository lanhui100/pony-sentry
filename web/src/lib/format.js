// 数值展示统一口径：token 用 K/M，耗时用 ms/s/m。
// 纯函数、无 Vue 依赖，便于 `node --test` 直接断言。

const trimZero = (text) =>
  text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text

/**
 * Token 数量：<1000 原样；<1e6 用 K（1 位小数）；>=1e6 用 M（2 位小数）。
 * 非法/负数/空值 → '0'。
 */
export function formatTokens(value) {
  const num = Number(value)
  if (!Number.isFinite(num) || num <= 0) return '0'
  if (num < 1000) return String(Math.round(num))
  if (num < 1_000_000) return `${trimZero((num / 1000).toFixed(1))}K`
  return `${trimZero((num / 1_000_000).toFixed(2))}M`
}

/**
 * 耗时：<1000ms → `123ms`；<60s → `12.3s`；>=60s → `2m` / `2m 5s`。
 * 非法/负数/空值 → '0ms'。
 */
export function formatDuration(value) {
  const num = Number(value)
  if (!Number.isFinite(num) || num < 0) return '0ms'
  if (num < 1000) return `${Math.round(num)}ms`
  if (num < 60_000) return `${trimZero((num / 1000).toFixed(1))}s`
  const totalSeconds = Math.round(num / 1000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return seconds === 0 ? `${minutes}m` : `${minutes}m ${seconds}s`
}
