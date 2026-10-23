# Trace v2 迁移 Runbook（0005 / 0006）

> 目标库：`postgresql://job_copilot@10.43.206.170:5432/ponysentry`（生产，38 行 traces）。
> 迁移是**加法式**：新表 `trace_turns`、`traces` 新增 `turn_status/turns_count/note`、`trace_turns.run_id` + 5 个索引；
> 旧二进制可继续运行（忽略新对象）。回填由新二进制的 `migrate()` → `backfill_trace_turns()` 在启动时完成，幂等可重放。

## 步骤

```bash
# 1) 备份（pg_dump 版本不匹配时用 COPY）
mkdir -p /home/dm/backups/ponysentry-trace-migration-$(date +%Y%m%d)
psql "$DATABASE_URL" -c "\copy (select * from traces) to '/home/dm/backups/.../traces-before.csv' with (format csv, header true)"
psql "$DATABASE_URL" -c "\copy (select * from trace_turns) to '.../trace_turns-before.csv' with (format csv, header true)"  # 若表已存在

# 2) 用新二进制做「迁移 + 回填」，不接管生产端口（随机 PORT，只等 healthz 后退出）
DATABASE_URL="$DATABASE_URL" PORT=0 CLIENT_TOKEN=x ADMIN_TOKEN=y ./target/release/pony-sentry-server &
# 启动即执行 ensure_trace_v2_schema + backfill；健康后 kill

# 3) 校验
psql "$DATABASE_URL" -c "select count(*) from trace_turns;"
psql "$DATABASE_URL" -c "select count(*) filter (where turns_count = 0) as zero_count, count(*) from traces;"
psql "$DATABASE_URL" -c "select indexname from pg_indexes where tablename in ('traces','trace_turns');"

# 4) 回滚（仅 PG；SQLite 不支持 DROP COLUMN，需重建表）
psql "$DATABASE_URL" -f migrations/postgres/0005_trace_turns.down.sql
psql "$DATABASE_URL" -f migrations/postgres/0006_trace_turn_run_id.down.sql   # 若提供，否则手工 DROP COLUMN run_id
```

## 观测

- `pony_sentry_trace_merge_lost_turns_total`、`pony_sentry_trace_payload_bytes_total`、`pony_sentry_trace_truncated_total` 从 0 起累计。
- 回填后 `traces.turns_count` 应等于 `trace_turns` 行数（按 session 聚合），`turn_status` 取末轮 status。

## 注意

- 索引使用 `CREATE INDEX CONCURRENTLY`（事务外逐条），大表不长时间持写锁；`ADD COLUMN` 前查列存在，二次启动 0 DDL。
- 生产实际执行记录见下方「执行记录」。

## 执行记录（2026-10-23 23:10 CST）

- 备份：`/home/dm/backups/ponysentry-trace-migration-20261023/traces-before-0005.csv`（38 行，COPY CSV；pg_dump 16 与 PG 18 版本不匹配故改用 COPY）。
- 执行方式：新二进制 `DATABASE_URL=<prod> PORT=23991` 启动 → `/healthz` 200 → 退出（启动即 `ensure_trace_v2_schema` + 回填）。
- 执行后核验（psql）：
  - `traces=38`、`turns_count=0` 的行 **0**；
  - `trace_turns=59`（38 个会话展开为 59 个轮次）；
  - `turns_count <> trace_turns 实际行数` 的会话 **0**；
  - 索引 14 个（含 `idx_trace_turns_run_id/session_seq/status`、`idx_traces_run_id/turn_status/updated_at`）。
- 旧二进制仍在 3000 端口运行，未受影响（忽略新表/列）；正式切换新二进制属部署动作，待排期。
