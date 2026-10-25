-- migrations/postgres/0007_issue_archive.sql (PostgreSQL)
-- issue 归档 + 治理指标（契约 `.dev-team/contract-issue-governance.md` C1.1，FROZEN v1.2）。
--
-- 幂等约束：全文件可重复执行（IF NOT EXISTS 全覆盖）。
-- 执行方式：由 `PgIssueRepository::ensure_schema` **逐条 autocommit** 执行——
--   ADD COLUMN 先查 information_schema.columns（存在即跳过），
--   CREATE INDEX 走 CONCURRENTLY 并先查 pg_indexes。
-- 因此每条 ALTER 必须是**单列**形态（`add_column_target` 只解析第一条 COLUMN）。

ALTER TABLE issues ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ NULL;
ALTER TABLE issues ADD COLUMN IF NOT EXISTS archive_restored_at TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS idx_issues_archived_at ON issues(archived_at);
CREATE INDEX IF NOT EXISTS idx_issues_project_status_archived ON issues(project, status, archived_at);
CREATE INDEX IF NOT EXISTS idx_issues_first_seen_at ON issues(first_seen_at);
CREATE INDEX IF NOT EXISTS idx_events_issue_created_id ON events(issue_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_events_created_at ON events(created_at);

CREATE TABLE IF NOT EXISTS ingest_metrics (
    bucket_date DATE NOT NULL,
    project     TEXT NOT NULL DEFAULT '',
    outcome     TEXT NOT NULL,
    count       BIGINT NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_date, project, outcome)
);

CREATE INDEX IF NOT EXISTS idx_ingest_metrics_bucket ON ingest_metrics(bucket_date);
