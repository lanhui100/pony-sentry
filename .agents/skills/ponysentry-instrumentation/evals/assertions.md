# 断言（客观可验证）

## Eval 1: Vue SPA
- A1: 输出含三路捕获（errorHandler + unhandledrejection + window error）
- A2: 含深度脱敏（SENSITIVE_KEYS 或 REDACTED_SECRET）
- A3: 含面包屑队列与上限（MAX_BREADCRUMBS）
- A4: payload 含 platform=vue / release / environment / exception / breadcrumbs

## Eval 2: Rust CLI
- B1: 含 panic::set_hook
- B2: release 引用 CARGO_PKG_VERSION
- B3: 上报 fire-and-forget（tokio::spawn 或非阻塞）
- B4: 含 curl 查询命令（/api/v1/issues）

## Eval 3: Tauri 查询
- C1: 给出 platform=tauri&status=unresolved 查询命令
- C2: 分析堆栈/breadcrumbs 聚类判断
- C3: PATCH in_progress assigned_to=agent-1
