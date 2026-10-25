-- migrations/postgres/0007_issue_archive.down.sql (PostgreSQL)
-- 0007 回滚（契约 C1.3，FROZEN v1.2）。全部 `IF EXISTS`：二次执行 exit 0。
--
-- 回滚顺序（C1.3 / amend v1.1 B-9，**不可交换**）：
--   停止服务 → 恢复上一版二进制 → 执行本文件 → 启动 → 验证（/healthz、/api/v1/issues、/api/v1/traces）
-- 反序（先 DROP COLUMN 再回退二进制）会让仍在运行的旧二进制读 ISSUE_COLUMNS 命中"列不存在" → 全量 500。
--
-- SQLite 不提供 down：sqlx 绑定的 SQLite 不支持 DROP COLUMN，
-- 需要回滚时走重建表流程（见契约 C1.3 表）。

DROP TABLE IF EXISTS ingest_metrics;
DROP INDEX IF EXISTS idx_ingest_metrics_bucket;
DROP INDEX IF EXISTS idx_issues_project_status_archived;
DROP INDEX IF EXISTS idx_issues_archived_at;
DROP INDEX IF EXISTS idx_issues_first_seen_at;
DROP INDEX IF EXISTS idx_events_issue_created_id;
DROP INDEX IF EXISTS idx_events_created_at;
ALTER TABLE issues DROP COLUMN IF EXISTS archive_restored_at;
ALTER TABLE issues DROP COLUMN IF EXISTS archived_at;
