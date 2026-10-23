#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$ROOT/target/debug/pony-sentry-server"
DB="/tmp/smoke-trace-v2-$$.db"
PORT=$(( 20000 + RANDOM % 10000 ))
CLIENT="smoke-client-token"
ADMIN="smoke-admin-token"
LOG="$ROOT/.dev-team/trace-fix-redphase/smoke-server.log"
OUT="$ROOT/.dev-team/trace-fix-redphase/smoke-real-incident.txt"
rm -f "$DB"
DATABASE_URL="sqlite:$DB?mode=rwc" PORT="$PORT" CLIENT_TOKEN="$CLIENT" ADMIN_TOKEN="$ADMIN" \
  "$BIN" >"$LOG" 2>&1 &
PID=$!
cleanup(){ kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; rm -f "$DB"; }
trap cleanup EXIT
for i in $(seq 1 100); do curl -sf "http://127.0.0.1:$PORT/healthz" >/dev/null && break; sleep 0.2; done
{
echo "# 真实事故 trace 冒烟 — $(date -Iseconds)"
echo "## 1) POST 真实 payload（audit-trace-cards 基线）"
RESP=$(curl -s -w '\nHTTP %{http_code}' -X POST "http://127.0.0.1:$PORT/api/v1/traces" \
  -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
  --data-binary @"$ROOT/.dev-team/audit-trace-cards/00-baseline/03-trace-payload.raw.json")
echo "$RESP" | tail -1
ID=$(echo "$RESP" | head -n -1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')
echo "persisted id = $ID"
echo "## 2) GET by 响应 id（W-07）"
curl -s -o /tmp/smoke-get.json -w 'HTTP %{http_code}\n' -H "x-admin-token: $ADMIN" "http://127.0.0.1:$PORT/api/v1/traces/$ID"
python3 - "$ID" <<'PY'
import json,sys
d=json.load(open('/tmp/smoke-get.json'))
print('id==response:', d['id']==sys.argv[1])
print('turn_status:', d['turn_status'], '| turns_count:', d['turns_count'])
t=d['payload']['turns'][0]
print('diagnostics:', json.dumps(t.get('diagnostics'), ensure_ascii=False))
print('has eval_status in payload:', 'eval_status' in d['payload'])
print('session_id_source:', d['payload'].get('session_id_source'))
print('total_in/out/dur:', d['total_input_tokens'], d['total_output_tokens'], d['total_duration_ms'])
PY
echo "## 3) 参数校验（I-10）"
for q in 'limit=-5' 'offset=-1' 'limit=abc'; do
  printf '%-12s -> ' "$q"; curl -s -o /tmp/smoke-bad.txt -w 'HTTP %{http_code} ' -H "x-admin-token: $ADMIN" "http://127.0.0.1:$PORT/api/v1/traces?$q"; head -c 60 /tmp/smoke-bad.txt; echo
done
echo "## 4) /metrics 四个 trace 指标"
curl -s "http://127.0.0.1:$PORT/metrics" | grep -E '^pony_sentry_trace' || true
echo "## 5) 二次上报：failed -> running 不回退（W-01）"
curl -s -o /dev/null -w 'HTTP %{http_code}\n' -X POST "http://127.0.0.1:$PORT/api/v1/traces" -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
  -d '{"session_id":"thread_ref_v1_e9b4ec395c96d8913c106ef8","turns":[{"turn_id":"turn_5dd7639f928549dfb86d96cbe6cbf33a","status":"running"}]}'
curl -s -H "x-admin-token: $ADMIN" "http://127.0.0.1:$PORT/api/v1/traces/$ID" | python3 -c 'import json,sys;d=json.load(sys.stdin);print("after re-report status =", d["payload"]["turns"][0]["status"], "| turns_count =", d["turns_count"])'
} | tee "$OUT"
