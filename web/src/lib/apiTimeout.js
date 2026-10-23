// 控制台 API 读超时（毫秒）。
// 契约 .dev-team/contract-console-timeout.md R2/R3：必须严格大于服务端
// PostgreSQL/SQLite 连接池 acquire_timeout（现 3000ms，见 crates/server/src/main.rs:152
// 与 crates/core/src/db.rs:9）且余量 > 2000ms，上界 15000ms。
// 取值偏小的后果：DB 不可达时浏览器先 abort，页面把 500 误报为「请求超时」。
export const API_TIMEOUT_MS = 8000
