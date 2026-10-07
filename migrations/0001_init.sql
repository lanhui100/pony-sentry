-- migrations/0001_init.sql
CREATE TABLE IF NOT EXISTS issues (
    id TEXT PRIMARY KEY NOT NULL,
    fingerprint TEXT UNIQUE NOT NULL,
    title TEXT NOT NULL,
    culprit TEXT,
    platform TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'unresolved',
    assigned_to TEXT,
    count INTEGER NOT NULL DEFAULT 1,
    last_release TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY NOT NULL,
    issue_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    release TEXT,
    environment TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(issue_id) REFERENCES issues(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_issues_status ON issues(status);
CREATE INDEX IF NOT EXISTS idx_issues_platform ON issues(platform);
CREATE INDEX IF NOT EXISTS idx_issues_fingerprint ON issues(fingerprint);
CREATE INDEX IF NOT EXISTS idx_events_issue_id ON events(issue_id);
