-- migrations/postgres/0002_add_project.sql
-- 项目维度：上报事件的 extra.project_path 即工作区，其末段为项目名。
-- ADD COLUMN IF NOT EXISTS 保证 migrate() 每次启动重放均幂等。
ALTER TABLE issues ADD COLUMN IF NOT EXISTS project TEXT;

CREATE INDEX IF NOT EXISTS idx_issues_project ON issues(project);
