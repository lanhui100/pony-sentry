-- migrations/0002_add_project.sql (SQLite)
-- SQLite 的 ALTER TABLE ADD COLUMN 不支持 IF NOT EXISTS，
-- 幂等性由 SqliteIssueRepository::migrate() 的 PRAGMA table_info 守卫保证。
ALTER TABLE issues ADD COLUMN project TEXT;

CREATE INDEX IF NOT EXISTS idx_issues_project ON issues(project);
