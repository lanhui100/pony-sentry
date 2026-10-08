# Vue / Web SPA 埋点模板

浏览器端上报（`fetch` 原生实现，无 SDK 依赖）。

## 核心上报模块（`src/telemetry.js`）

```js
// src/telemetry.js
const INGEST_URL = import.meta.env.VITE_PONYSENTRY_INGEST_URL || 'https://sentry.ponyjob.top';
const CLIENT_TOKEN = import.meta.env.VITE_PONYSENTRY_CLIENT_TOKEN || ''; // 服务端配置了才填
const PROJECT_PATH = import.meta.env.VITE_PROJECT_PATH || ''; // 供 DSH 定位前端工程工作区
const SAMPLE_RATE = 1.0; // 错误 100% 上报；面包屑可在高流量端下调

// ---- 零信任脱敏（与服务端 sanitizer 对齐）----
const SENSITIVE_KEYS = /^(token|password|passwd|secret|api_key|apikey|access_token|refresh_token|authorization|cookie|private_key|credential)/i;

function sanitizeValue(value, key) {
  if (typeof value !== 'string') return value;
  let s = value
    .replace(/\/home\/[^/\s]+/g, '[USER_HOME]/')
    .replace(/\/Users\/[^/\s]+/g, '[USER_HOME]/')
    .replace(/[A-Za-z]:\\(?:Users|Documents and Settings)\\[^\\]+/g, '[USER_HOME]\\');
  // Bearer / Basic Token
  s = s.replace(/(?i)(bearer\s+[a-zA-Z0-9_.\-]{10,}|basic\s+[a-zA-Z0-9+/=]{10,})/g, '[REDACTED_SECRET]');
  if (SENSITIVE_KEYS.test(key)) {
    s = '[REDACTED_SECRET]';
  }
  return s;
}

function deepSanitize(obj, depth = 0) {
  if (depth > 32) return '[MAX_DEPTH_EXCEEDED]'; // 防深嵌套栈溢出
  if (Array.isArray(obj)) return obj.map((v) => deepSanitize(v, depth + 1));
  if (obj && typeof obj === 'object') {
    const out = {};
    for (const [k, v] of Object.entries(obj)) {
      out[k] = SENSITIVE_KEYS.test(k) ? '[REDACTED_SECRET]' : deepSanitize(v, depth + 1);
    }
    return out;
  }
  if (typeof obj === 'string') return sanitizeValue(obj, '');
  return obj;
}

// ---- 面包屑队列 ----
const breadcrumbs = [];
const MAX_BREADCRUMBS = 64;

export function addBreadcrumb(category, message, data = {}) {
  breadcrumbs.push({ category, message: sanitizeValue(message, ''), data: deepSanitize(data) });
  if (breadcrumbs.length > MAX_BREADCRUMBS) breadcrumbs.shift();
}

// ---- 上报入口 ----
export function reportError({ errorType, message, stackFrames = [], extra = {}, tags = {} }) {
  if (Math.random() > SAMPLE_RATE) return; // 采样
  const payload = {
    platform: 'vue',
    release: import.meta.env.VITE_APP_RELEASE || 'dev',
    environment: import.meta.env.VITE_APP_ENV || 'development',
    message: sanitizeValue(message, ''),
    exception: {
      error_type: errorType,
      value: sanitizeValue(message, ''),
      stacktrace: stackFrames.map((f) => ({
        filename: sanitizeValue(f.filename || '', ''),
        function: sanitizeValue(f.function || '', ''),
        lineno: f.lineno,
        in_app: true,
      })),
    },
    tags: deepSanitize(tags),
    extra: deepSanitize({ project_path: PROJECT_PATH, ...extra }),
    breadcrumbs: breadcrumbs.splice(0), // 上报后清空
  };

  const headers = { 'Content-Type': 'application/json' };
  if (CLIENT_TOKEN) headers['X-Client-Token'] = CLIENT_TOKEN;

  // fire-and-forget：不阻塞页面主流程
  fetch(`${INGEST_URL}/api/v1/ingest`, {
    method: 'POST',
    headers,
    body: JSON.stringify(payload),
    keepalive: true, // 页面卸载时也尽量送达
  }).catch(() => {});
}
```

## 入口接入（`src/main.js`）

```js
import { createApp } from 'vue';
import App from './App.vue';
import router from './router';
import { reportError, addBreadcrumb } from './telemetry';

const app = createApp(App);

// 1. Vue 组件错误捕获
app.config.errorHandler = (err, instance, info) => {
  reportError({
    errorType: err?.name || 'VueError',
    message: err?.message || String(err),
    stackFrames: parseStack(err?.stack),
    extra: { info, component: instance?.$options?.name || 'Anonymous' },
  });
};

// 2. 全局未捕获 Promise 拒绝
window.addEventListener('unhandledrejection', (event) => {
  const reason = event.reason;
  reportError({
    errorType: 'UnhandledRejection',
    message: reason?.message || String(reason),
    stackFrames: parseStack(reason?.stack),
    extra: { promise: reason },
  });
});

// 3. 全局未捕获异常
window.addEventListener('error', (event) => {
  reportError({
    errorType: event.error?.name || 'WindowError',
    message: event.message,
    stackFrames: parseStack(event.error?.stack),
    extra: { source: event.filename, line: event.lineno },
  });
});

// 4. 路由导航面包屑
router.afterEach((to) => {
  addBreadcrumb('navigation', `navigate to ${to.fullPath}`, { from: to.path });
});

// 5. 用户上下文（可选，脱敏后）：仅 id/role，禁 PII
// app.config.globalProperties.$sentryUser = { id: 'user_123', role: 'admin' };

function parseStack(stack) {
  if (!stack) return [];
  return stack.split('\n').slice(1).map((line) => {
    const m = line.match(/at\s+(.+?)\s*\((.+?):(\d+):(\d+)\)/) || line.match(/at\s+(.+?):(\d+):(\d+)/);
    if (!m) return { function: line.trim().slice(0, 200) };
    return { function: m[1], filename: m[2], lineno: Number(m[3]) };
  });
}

app.use(router).mount('#app');
```

## 校准样例

- 正例：组件抛错 → `errorHandler` 上报 VueError + 组件名 extra。
- 反例：Promise resolve 正常 → 不上报（只有 reject 才走 unhandledrejection）。
- 正例：axios 拦截器捕获 401 → `addBreadcrumb('http', 'request 401', { url })`，不直接上报错误。
- 反例：把整个请求 payload（含 body 密码）放进 extra → 必须 deepSanitize 后再放。
