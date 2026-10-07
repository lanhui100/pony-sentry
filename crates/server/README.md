# pony-sentry-server

## Config
- `PORT`: HTTP listen port (default `3000`).
- `DATABASE_URL`: Connection string for PostgreSQL or SQLite.

## Semantics
- Axum web server exposing `/healthz`, `/api/v1/ingest`, `/api/v1/issues`, and embedded web console.

## Limitations
- Single binary server deployment.
