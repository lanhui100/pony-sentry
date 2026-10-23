-- migrations/0005_trace_turns.sql (SQLite)
-- trace v2：每 turn 一行的权威轮次存储 + `traces` 聚合列（契约 `.dev-team/trace-fix-contract.md` C1.1–C1.3）。
--
-- 方言约束：SQLite 的 `ALTER TABLE ... ADD COLUMN` 不支持 `IF NOT EXISTS`，故本文件的 ADD COLUMN
-- 由 `SqliteIssueRepository::migrate()` 的列探测守卫包裹（范式同 0002/0003/0004），整批在单事务内
-- 执行；重复启动不会重放 ADD COLUMN。CREATE TABLE / CREATE INDEX 自身幂等。
--
-- 回填（C1.3 / amend v1.4-1）：**本文件只建 schema**。存量 `traces.payload.turns[]` → `trace_turns`
-- 的搬运在 Rust 侧 `migrate()` 之后执行（`backfill_trace_turns`），以保证 turn_id 与运行时
-- `trace_v2::turn_key_of`（sha256）**同构**（SQL 方言无 sha256）；回填同 turn_id 以首个为准，
-- 缺失 turn_id 的 turn 用内容 sha256 生成 `turn_srv_*`，并在同批内容碰撞时加序号后缀，避免塌陷。
--
-- 回滚：SQLite 不支持 `DROP COLUMN`（本项目按 3.35 前的边界处理），故不提供 down 脚本；
-- 需要回滚时按 0005 反向重建表（边界见 `docs/trace-contract.md` §5）。

CREATE TABLE IF NOT EXISTS trace_turns (
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'unknown',
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (session_id, turn_id)
);

CREATE INDEX IF NOT EXISTS idx_trace_turns_session_seq ON trace_turns(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_trace_turns_status ON trace_turns(status);

-- `traces` 聚合列（C1.2）
ALTER TABLE traces ADD COLUMN turn_status TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE traces ADD COLUMN turns_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE traces ADD COLUMN note TEXT;

CREATE INDEX IF NOT EXISTS idx_traces_run_id ON traces(run_id);
CREATE INDEX IF NOT EXISTS idx_traces_turn_status ON traces(turn_status);
CREATE INDEX IF NOT EXISTS idx_traces_updated_at ON traces(updated_at);
