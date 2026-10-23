#!/usr/bin/env bash
# 全量门禁：默认要求 PG 测试库（避免 PG 用例 SKIP 造成假绿）。
# 用法：PONY_TEST_PG_URL='postgresql://user:pass@host:5432/ponysentry_test' bash scripts/test-all.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -z "${PONY_TEST_PG_URL:-}" ]]; then
  echo "ERROR: PONY_TEST_PG_URL 未设置；PG 用例会被 SKIP，禁止据此判定全绿。" >&2
  echo "示例：PONY_TEST_PG_URL='postgresql://job_copilot:***@10.43.206.170:5432/ponysentry_test'" >&2
  exit 2
fi

echo "== cargo test --workspace (PG enabled) =="
SKIP_LOG="$(mktemp)"
(cargo test --workspace --offline 2>&1 | tee "$SKIP_LOG" | tail -3)
if grep -q "SKIP" "$SKIP_LOG"; then
  echo "ERROR: 检测到 SKIP —— PG 用例未真实执行。" >&2
  rm -f "$SKIP_LOG"
  exit 3
fi
rm -f "$SKIP_LOG"

echo "== cargo clippy =="
cargo clippy --workspace --all-targets -- -D warnings

echo "== acceptance (black-box) =="
cargo test --manifest-path tests/Cargo.toml --test acceptance_trace_v2 --offline
if [[ -f tests/acceptance_trace_v2_ext.rs ]]; then
  cargo test --manifest-path tests/Cargo.toml --test acceptance_trace_v2_ext --offline
fi

echo "== web =="
(cd web && pnpm test && pnpm build)
