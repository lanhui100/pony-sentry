-- migrations/postgres/0003_add_traces.sql (PostgreSQL)
CREATE TABLE IF NOT EXISTS traces (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL UNIQUE,
    run_id TEXT,
    turn_id TEXT,
    environment TEXT NOT NULL,
    release TEXT NOT NULL,
    project TEXT,
    eval_status TEXT NOT NULL DEFAULT 'unreviewed',
    payload JSONB NOT NULL,
    total_input_tokens BIGINT,
    total_output_tokens BIGINT,
    total_duration_ms BIGINT,
    reported_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

-- 项目维度（存量库补列，重复启动重放幂等，同 0002 的 ADD COLUMN IF NOT EXISTS 约定）
ALTER TABLE traces ADD COLUMN IF NOT EXISTS project TEXT;

CREATE INDEX IF NOT EXISTS idx_traces_session_id ON traces(session_id);
CREATE INDEX IF NOT EXISTS idx_traces_project ON traces(project);
CREATE INDEX IF NOT EXISTS idx_traces_eval_status ON traces(eval_status);
CREATE INDEX IF NOT EXISTS idx_traces_environment ON traces(environment);
CREATE INDEX IF NOT EXISTS idx_traces_created_at ON traces(created_at);
