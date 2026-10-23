# 修复轮终审裁决（只读）

**Verdict: FAIL** — 21 项 prior findings：13 CLOSED / 5 PARTIAL / 3 OPEN（2 项 PARTIAL 为 MAJOR）。不满足「无 OPEN 且 PARTIAL ≤2 且均 MINOR」。

## 实质关闭（重点核对项）
- **回填键同构**：迁移已无任何回填 SQL（grep hex/md5/INSERT/UPDATE = 0）；Rust `backfill_trace_turns → hydrate → turn_keys_for`（sqlite_repository.rs:128-143,1003）；测试断言两行键互异且逐条 `== turn_key_of`（trace_v2_repository_test.rs:733-752）。→ L3A-F1/F2、L3B-F3 CLOSED。
- **两条无 turn_id legacy turn**：`v2_backfill_missing_turn_ids_use_runtime_sha256_keys`（同文件:705-768）断言 2 行、`turns_count=2`、键形如 `turn_srv_<16hex>`；重复内容用例另断言 `_2` 后缀。
- **hydrate 覆盖度**：`existing.len() >= payload.turns.len()` 才跳过（sqlite:997 / pg:973），部分覆盖用例断言 `[c1,c2,c3]`（:825-857）。
- **tool_calls 无 id**：按 `tool_name+arguments` 归并（trace_v2.rs:403-465），5 轮重报恒 2 条（:975）。
- **9 处错误文案**：全部收口 `internal_error`/`Bad request`（routes.rs:1334/1346/1353/1389/1394/1427/1441/1455、:1419），真实删表集成测试逐字符断言 + 7 端点物理收据。→ L3B-F1/F6 CLOSED。
- **指标**：label 归一 + 512 series + overflow 单次 WARN（routes.rs:57-124），收据 1730B/513 series/WARN=1。→ L3B-F2 CLOSED。
- **down 迁移**：`migrations/postgres/0005_trace_turns.down.sql` 存在且全 `IF EXISTS`；收据 down 后 0 列 0 表、重放 exit 0；SQLite 边界已记录。
- **eval_status**：ingest 固定 `Unreviewed`（routes.rs:1540），C9.6 重冻结断言 unreviewed（acceptance:327）。→ L3B-F10 CLOSED。
- **前端**：F9 失败集含 timed_out/max_tokens/aborted（App.vue:1438）；F8 trace 预览 20KB + 对象身份缓存（:1488-1509）。F9 CLOSED，F8 PARTIAL（:508 面包屑仍全量 stringify）。

## 未关闭（阻塞 PASS）
1. **L3A-F4 PARTIAL/MAJOR** — retention 已单事务 + 固定 id 集（pg:797-826），但**未取 ingest 共用的 `pg_advisory_xact_lock(hashtext(session_id))`**（全仓仅 ingest :649 持锁）。同会话 ingest 先读旧轮次、retention 再删该会话行的交错仍可造成 payload.turns 与 trace_turns 漂移；测试仅串行批。
2. **L3B-F4 PARTIAL/MAJOR** — 全表 UPDATE 已移出，但 DDL 仍单条 multi-statement raw_sql 隐式事务（pg:55-58）、索引非 CONCURRENTLY、每次启动 `ADD COLUMN IF NOT EXISTS` 仍取 ACCESS EXCLUSIVE。
3. **L3A-F8 / F9 / L3B-F11 OPEN（MINOR）** — 32 位锁键碰撞无记录；`total + value` 无 checked_add（trace_v2.rs:472）；`/metrics` 仍未鉴权（routes.rs:732）。
4. **L3A-F6 / L3A-F7 PARTIAL（MINOR）** — 回填幂等用例仍被扫描条件短路；默认 `cargo test --workspace` 仍 SKIP 即通过、冻结 C9.2 仍 SQLite（PG 覆盖依赖 Lead 一次性环境变量收据）。

结论：BLOCKER 声明项确已实质关闭（非声明式），但两项 MAJOR 残留与 3 项 OPEN 使本轮不能 PASS。
