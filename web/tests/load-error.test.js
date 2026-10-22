import test from 'node:test'
import assert from 'node:assert/strict'

import { classifyLoadFailure, loadFailureDetail } from '../src/lib/loadError.js'

test('classifyLoadFailure distinguishes auth / timeout / network / server', () => {
  assert.equal(classifyLoadFailure({ name: 'AuthError', status: 401 }), 'auth')
  assert.equal(classifyLoadFailure({ status: 403 }), 'auth')
  assert.equal(classifyLoadFailure({ name: 'TimeoutError' }), 'timeout')
  assert.equal(classifyLoadFailure({ name: 'TypeError' }), 'network')
  assert.equal(classifyLoadFailure({ name: 'HttpError', status: 503 }), 'server')
  assert.equal(classifyLoadFailure({ name: 'Error' }), 'unknown')
})

test('loadFailureDetail gives actionable copy and prioritizes auth', () => {
  assert.match(loadFailureDetail({ issues: 'auth' }), /鉴权失败/)
  assert.match(loadFailureDetail({ traces: 'timeout' }), /请求超时/)
  assert.match(loadFailureDetail({ projects: 'network' }), /网络异常/)
  assert.match(loadFailureDetail({ issues: 'server' }), /服务端错误/)
  assert.match(loadFailureDetail({ traces: 'timeout', projects: 'auth' }), /鉴权失败/)
  assert.equal(loadFailureDetail({}), '')
})
