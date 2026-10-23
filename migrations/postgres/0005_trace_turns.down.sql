-- migrations/postgres/0005_trace_turns.down.sql (PostgreSQL)
-- 0005 回滚脚本（契约 C1.3 / amend v1.4-8）。
--
-- 语义：把 `traces` 恢复到 0004 之后的形态，并删除 `trace_turns` 权威轮次表。
-- 幂等：全部 `IF EXISTS`，可重复执行。
--
-- ⚠️ 数据影响：回滚 **丢弃 `trace_turns` 中的轮次与 `traces.turn_status/turns_count/note`**。
-- 若回滚前需要保留轮次数据，先导出：
--   COPY (SELECT * FROM trace_turns) TO '/tmp/trace_turns_backup.csv' CSV HEADER;
--   COPY (SELECT id, turn_status, turns_count, note FROM traces) TO '/tmp/traces_agg_backup.csv' CSV HEADER;
-- 回滚后 `traces.payload.turns[]` 仍保留（写入路径在回滚前的最后状态），但没有 per-turn 行。
--
-- SQLite 无对应 down 脚本（不支持 DROP COLUMN），边界见 migrations/0005_trace_turns.sql 头注释。

DROP INDEX IF EXISTS idx_trace_turns_session_seq;
DROP INDEX IF EXISTS idx_trace_turns_status;
DROP TABLE IF EXISTS trace_turns;

DROP INDEX IF EXISTS idx_traces_run_id;
DROP INDEX IF EXISTS idx_traces_turn_status;
DROP INDEX IF EXISTS idx_traces_updated_at;

ALTER TABLE traces DROP COLUMN IF EXISTS turn_status;
ALTER TABLE traces DROP COLUMN IF EXISTS turns_count;
ALTER TABLE traces DROP COLUMN IF EXISTS note;
