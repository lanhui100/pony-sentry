// token K/M 与耗时 ms/s/m 展示口径（纯函数，真实断言）。
import test from 'node:test'
import assert from 'node:assert/strict'
import { formatTokens, formatDuration } from '../src/lib/format.js'

test('formatTokens：K/M 单位与边界', () => {
  assert.equal(formatTokens(0), '0')
  assert.equal(formatTokens(undefined), '0')
  assert.equal(formatTokens(-5), '0')
  assert.equal(formatTokens(999), '999')
  assert.equal(formatTokens(1000), '1K')
  assert.equal(formatTokens(1500), '1.5K')
  assert.equal(formatTokens(37980), '38K')
  assert.equal(formatTokens(23936), '23.9K')
  assert.equal(formatTokens(1_020_695), '1.02M')
  assert.equal(formatTokens(2_000_000), '2M')
  assert.equal(formatTokens(999_999), '1000K')
})

test('formatDuration：ms/s/m 单位与边界', () => {
  assert.equal(formatDuration(0), '0ms')
  assert.equal(formatDuration(undefined), '0ms')
  assert.equal(formatDuration(-1), '0ms')
  assert.equal(formatDuration(999), '999ms')
  assert.equal(formatDuration(1000), '1s')
  assert.equal(formatDuration(19554), '19.6s')
  assert.equal(formatDuration(59_999), '60s')
  assert.equal(formatDuration(60_000), '1m')
  assert.equal(formatDuration(120_272), '2m')
  assert.equal(formatDuration(125_000), '2m 5s')
  assert.equal(formatDuration(3_600_000), '60m')
})
