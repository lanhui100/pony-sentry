-- migrations/postgres/0006_trace_turn_run_id.sql (PostgreSQL)
-- W-04（amend v1.5-6）：`trace_turns` 增 `run_id`（每轮的 run 归属）+ 索引。
-- 全部语句幂等（ADD COLUMN / CREATE INDEX CONCURRENTLY 由 migrate() 的 ensure 机制执行）。

ALTER TABLE trace_turns ADD COLUMN IF NOT EXISTS run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_trace_turns_run_id ON trace_turns(run_id);
