// 控制台数据加载失败的分类与可诊断文案。
// 这里保持为纯函数，便于在不启动浏览器的情况下做单元测试。
export function classifyLoadFailure(error) {
  if (!error || typeof error !== 'object') return 'unknown'
  if (error.name === 'AuthError' || error.status === 401 || error.status === 403) return 'auth'
  if (error.name === 'TimeoutError') return 'timeout'
  if (error.name === 'TypeError') return 'network'
  if (typeof error.status === 'number' && error.status >= 500) return 'server'
  return 'unknown'
}

export function loadFailureDetail(failures) {
  const kinds = new Set(Object.values(failures || {}))
  if (kinds.has('auth')) {
    return '鉴权失败：控制台令牌缺失或已失效，请核对 ADMIN_TOKEN 并重新部署'
  }
  if (kinds.has('timeout')) {
    return '请求超时：服务响应过慢或网络不稳定，请稍后重试'
  }
  if (kinds.has('server')) {
    return '服务端错误：PonySentry 返回 5xx，请查看服务日志'
  }
  if (kinds.has('network')) {
    return '网络异常：无法连接 PonySentry 服务，请检查网络后重试'
  }
  return ''
}
