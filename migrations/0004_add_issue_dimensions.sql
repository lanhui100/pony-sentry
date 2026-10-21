-- migrations/0004_add_issue_dimensions.sql (SQLite)
-- 多环境维度（environment/app/namespace/component/level/source_kind）与事件幂等键（events.event_key/level）。
--
-- 方言约束（契约 R5.1 / R5.1a / R19.1）：
-- 1. SQLite 的 `ALTER TABLE ... ADD COLUMN` 不支持 `IF NOT EXISTS`，故本文件的 ADD COLUMN 语句
--    由 `SqliteIssueRepository::migrate()` 的列探测守卫包裹（范式同 0002/0003），整批在单事务内执行，
--    重复启动不会重放 ADD COLUMN；
-- 2. SQLite 无 `ALTER TABLE ... DROP CONSTRAINT`，0001 的 `fingerprint TEXT UNIQUE` 生成隐式索引
--    `sqlite_autoindex_issues_1` 不可删除，故**保留** `UNIQUE(fingerprint)`，另加
--    `uq_issues_fingerprint_env` 冗余唯一索引。等价性：v3 指纹已含 environment，
--    故 UNIQUE(fingerprint) ⟹ UNIQUE(fingerprint, environment)（契约 R19.1）。
-- 3. 历史 issue 的 fingerprint 保持 v2 不改写，仅回填 environment（契约 R6.1）。

ALTER TABLE issues ADD COLUMN environment TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE issues ADD COLUMN app TEXT;
ALTER TABLE issues ADD COLUMN namespace TEXT;
ALTER TABLE issues ADD COLUMN component TEXT;
ALTER TABLE issues ADD COLUMN level TEXT NOT NULL DEFAULT 'error';
ALTER TABLE issues ADD COLUMN source_kind TEXT;

ALTER TABLE events ADD COLUMN event_key TEXT;
ALTER TABLE events ADD COLUMN level TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS uq_issues_fingerprint_env ON issues(fingerprint, environment);
CREATE INDEX IF NOT EXISTS idx_issues_environment_status ON issues(environment, status);
CREATE INDEX IF NOT EXISTS idx_issues_app_environment ON issues(app, environment);
CREATE INDEX IF NOT EXISTS idx_issues_last_seen_at ON issues(last_seen_at);

-- 幂等键全局唯一（跨 issue 亦然）；NULL 允许重复（历史事件无 event_key）。
CREATE UNIQUE INDEX IF NOT EXISTS uq_events_event_key ON events(event_key) WHERE event_key IS NOT NULL;

-- 历史环境回填（幂等）：取该 issue 最近一条带 environment 的 events 行的环境，缺失置 'unknown'。
UPDATE issues SET environment = COALESCE((
  SELECT e.environment FROM events e
  WHERE e.issue_id = issues.id AND e.environment IS NOT NULL AND e.environment <> ''
  ORDER BY e.created_at DESC LIMIT 1), 'unknown')
WHERE environment = 'unknown';
