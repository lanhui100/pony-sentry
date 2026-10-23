-- migrations/postgres/0006_trace_turn_run_id.down.sql (PostgreSQL)
-- 0006 回滚（契约 amend v1.6-5 / L3A-E7）：删除 `trace_turns.run_id` 索引与列。
-- 幂等（全部 IF EXISTS），可重复执行。
--
-- ⚠️ 数据影响：回滚丢弃每轮的 run 归属（可从 `traces.run_id` 重新回填，但轮次级差异会丢失）。
-- SQLite 无 down 脚本（见 migrations/0005_trace_turns.sql 头注释的同类边界）。

DROP INDEX IF EXISTS idx_trace_turns_run_id;

ALTER TABLE trace_turns DROP COLUMN IF EXISTS run_id;
