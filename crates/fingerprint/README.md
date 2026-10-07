# pony-sentry-fingerprint

## Config
- None.

## Semantics
- SHA-256 fingerprint hashing algorithm for error grouping and deduplication.
- Ignores source line/column number fluctuations.

## Limitations
- Collapses same error types with identical top frame culprit.
