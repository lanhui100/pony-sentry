-- migrations/postgres/0004_add_issue_dimensions.sql (PostgreSQL)
-- 多环境维度与事件幂等键。全部语句幂等（IF NOT EXISTS / DROP ... IF EXISTS + 重建），
-- 与 SQLite 版语义等价（契约 R5 / R5.1）。历史 issue 的 fingerprint 保持 v2 不改写（契约 R6.1）。

ALTER TABLE issues ADD COLUMN IF NOT EXISTS environment TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE issues ADD COLUMN IF NOT EXISTS app TEXT;
ALTER TABLE issues ADD COLUMN IF NOT EXISTS namespace TEXT;
ALTER TABLE issues ADD COLUMN IF NOT EXISTS component TEXT;
ALTER TABLE issues ADD COLUMN IF NOT EXISTS level TEXT NOT NULL DEFAULT 'error';
ALTER TABLE issues ADD COLUMN IF NOT EXISTS source_kind TEXT;

-- 唯一性收窄：fingerprint → (fingerprint, environment)。
-- 0001 的 `fingerprint TEXT UNIQUE NOT NULL` 生成约束 issues_fingerprint_key，先删除再用唯一索引重建。
ALTER TABLE issues DROP CONSTRAINT IF EXISTS issues_fingerprint_key;
CREATE UNIQUE INDEX IF NOT EXISTS uq_issues_fingerprint_env ON issues(fingerprint, environment);

CREATE INDEX IF NOT EXISTS idx_issues_environment_status ON issues(environment, status);
CREATE INDEX IF NOT EXISTS idx_issues_app_environment ON issues(app, environment);
CREATE INDEX IF NOT EXISTS idx_issues_last_seen_at ON issues(last_seen_at);

ALTER TABLE events ADD COLUMN IF NOT EXISTS event_key TEXT;
ALTER TABLE events ADD COLUMN IF NOT EXISTS level TEXT;

-- 幂等键全局唯一（跨 issue 亦然）；NULL 允许重复（历史事件无 event_key）。
CREATE UNIQUE INDEX IF NOT EXISTS uq_events_event_key ON events(event_key) WHERE event_key IS NOT NULL;

-- 历史环境回填（幂等）：取该 issue 最近一条带 environment 的 events 行的环境，缺失置 'unknown'。
UPDATE issues SET environment = COALESCE((
  SELECT e.environment FROM events e
  WHERE e.issue_id = issues.id AND e.environment IS NOT NULL AND e.environment <> ''
  ORDER BY e.created_at DESC LIMIT 1), 'unknown')
WHERE environment = 'unknown';
