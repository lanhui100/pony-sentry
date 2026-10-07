# pony-sentry-core

## Config
- `DATABASE_URL`: SQLite (`sqlite://...`) or PostgreSQL (`postgres://...`) connection string.

## Semantics
- Core data models (`Issue`, `Event`) and storage traits (`IssueRepository`).
- Includes `SqliteIssueRepository` and `PgIssueRepository` with auto-migration.

## Limitations
- Single-instance state machine, no distributed lock coordinator.
