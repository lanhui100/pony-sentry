-- migrations/0003_add_traces.sql (SQLite)
CREATE TABLE IF NOT EXISTS traces (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL,
    run_id TEXT,
    turn_id TEXT,
    environment TEXT NOT NULL,
    release TEXT NOT NULL,
    eval_status TEXT NOT NULL DEFAULT 'unreviewed',
    payload TEXT NOT NULL,
    total_input_tokens INTEGER,
    total_output_tokens INTEGER,
    total_duration_ms INTEGER,
    reported_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_traces_session_id ON traces(session_id);
CREATE INDEX IF NOT EXISTS idx_traces_eval_status ON traces(eval_status);
CREATE INDEX IF NOT EXISTS idx_traces_environment ON traces(environment);
CREATE INDEX IF NOT EXISTS idx_traces_created_at ON traces(created_at);
