-- migrations/0006_trace_turn_run_id.sql (SQLite)
-- W-04（amend v1.5-6）：`trace_turns` 增 `run_id`（每轮的 run 归属）+ 索引，
-- 使 `?run_id=` 可命中「traces.run_id 或其任一轮次的 run_id」。
--
-- 方言约束：SQLite 无 `ALTER TABLE ADD COLUMN IF NOT EXISTS`，本文件由
-- `SqliteIssueRepository::migrate()` 的列探测守卫包裹、单事务执行（范式同 0002/0004/0005）。

ALTER TABLE trace_turns ADD COLUMN run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_trace_turns_run_id ON trace_turns(run_id);
