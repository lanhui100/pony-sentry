-- migrations/postgres/0005_trace_turns.sql (PostgreSQL)
-- trace v2：每 turn 一行的权威轮次存储 + `traces` 聚合列（契约 `.dev-team/trace-fix-contract.md` C1.1–C1.3）。
--
-- 全部语句幂等（IF NOT EXISTS），每次启动重放安全（范式同 0001–0004）。
-- 回填（C1.3 / amend v1.4-1）：**本文件只建 schema**。存量 `traces.payload->'turns'` → `trace_turns`
-- 的搬运在 Rust 侧 `migrate()` 之后执行（`backfill_trace_turns`），以保证 turn_id 与运行时
-- `trace_v2::turn_key_of`（sha256）**同构**（SQL 方言无 sha256）；回填同 turn_id 以首个为准，
-- 缺失 turn_id 的 turn 用内容 sha256 生成 `turn_srv_*`，并在同批内容碰撞时加序号后缀，避免塌陷。
--
-- 回滚：见 `0005_trace_turns.down.sql`（DROP 表/索引/列）。

CREATE TABLE IF NOT EXISTS trace_turns (
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'unknown',
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (session_id, turn_id)
);

CREATE INDEX IF NOT EXISTS idx_trace_turns_session_seq ON trace_turns(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_trace_turns_status ON trace_turns(status);

-- `traces` 聚合列（C1.2）
ALTER TABLE traces ADD COLUMN IF NOT EXISTS turn_status TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE traces ADD COLUMN IF NOT EXISTS turns_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE traces ADD COLUMN IF NOT EXISTS note TEXT;

CREATE INDEX IF NOT EXISTS idx_traces_run_id ON traces(run_id);
CREATE INDEX IF NOT EXISTS idx_traces_turn_status ON traces(turn_status);
CREATE INDEX IF NOT EXISTS idx_traces_updated_at ON traces(updated_at);
