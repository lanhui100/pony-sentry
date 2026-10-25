-- migrations/0007_issue_archive.sql (SQLite)
-- issue 归档 + 治理指标（契约 `.dev-team/contract-issue-governance.md` C1.2，FROZEN v1.2）。
--
-- 方言约束：
-- 1. SQLite 的 `ALTER TABLE ... ADD COLUMN` 不支持 `IF NOT EXISTS`，故本文件的 ADD COLUMN 语句
--    由 `SqliteIssueRepository::migrate()` 的**列探测守卫**包裹（范式同 0002/0004/0005）：
--    `SELECT archived_at FROM issues LIMIT 0` / `SELECT archive_restored_at FROM issues LIMIT 0`
--    分别探测、缺列才 ALTER（部分迁移可自愈，0006 先例）；
-- 2. `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` 自身幂等，每次启动都执行；
-- 3. `archived_at` / `archive_restored_at` 存 ISO8601 UTC 文本（与既有 chrono 落库格式一致），
--    NULL = 未归档 / 从未手动恢复。

ALTER TABLE issues ADD COLUMN archived_at TEXT;
ALTER TABLE issues ADD COLUMN archive_restored_at TEXT;

CREATE INDEX IF NOT EXISTS idx_issues_archived_at ON issues(archived_at);
CREATE INDEX IF NOT EXISTS idx_issues_project_status_archived ON issues(project, status, archived_at);
CREATE INDEX IF NOT EXISTS idx_issues_first_seen_at ON issues(first_seen_at);
CREATE INDEX IF NOT EXISTS idx_events_issue_created_id ON events(issue_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_events_created_at ON events(created_at);

CREATE TABLE IF NOT EXISTS ingest_metrics (
    bucket_date TEXT NOT NULL,
    project     TEXT NOT NULL DEFAULT '',
    outcome     TEXT NOT NULL,
    count       INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (bucket_date, project, outcome)
);

CREATE INDEX IF NOT EXISTS idx_ingest_metrics_bucket ON ingest_metrics(bucket_date);
