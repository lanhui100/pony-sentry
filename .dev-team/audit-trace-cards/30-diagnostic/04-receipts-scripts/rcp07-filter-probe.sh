#!/usr/bin/env bash
set -u
: "${PGPASSWORD:?.需要 export PGPASSWORD}"
: "${CLIENT:?.需要 export CLIENT}"
: "${ADMIN:?.需要 export ADMIN}"
HOST=${PSQL_HOST:-10.43.206.170}
PSQL="psql -h $HOST -U job_copilot -d ponysentry -At -F|"
URL=${URL:-http://127.0.0.1:3000/api/v1/traces}
echo "## run_id 过滤被忽略（对照 project 过滤生效）"
echo -n "no filter: "; curl -s -H "x-admin-token: $ADMIN" "$URL?limit=50" | jq 'length'
echo -n "run_id=真实值: "; curl -s -H "x-admin-token: $ADMIN" "$URL?run_id=run_83c1879e338d442f953a5dd4b8d1646a&limit=50" | jq 'length'
echo -n "run_id=__nope__: "; curl -s -H "x-admin-token: $ADMIN" "$URL?run_id=__nope__&limit=50" | jq 'length'
echo -n "project=__nope__: "; curl -s -H "x-admin-token: $ADMIN" "$URL?project=__nope__&limit=50" | jq 'length'
echo "## 顶层未知字段被丢弃 / turn 内未知字段保留"
$PSQL -c "select count(*) from traces where session_id like 'audit-probe-%';"
curl -s -o /tmp/q1.json -w 'http_code=%{http_code}\n' -H "x-client-token: $CLIENT" -H 'content-type: application/json' -X POST "$URL" \
 -d '{"session_id":"audit-probe-diag-2","project":"job_copilot","environment":"development","release":"probe","top_unknown_field":"SHOULD_BE_DROPPED","error_type":"TimeoutError","turns":[{"turn_id":"turn-q1","status":"failed","turn_unknown_field":"SURVIVES","llm_calls":[{"model":"m","status":"error"}]}]}'
curl -s -H "x-admin-token: $ADMIN" "$URL?session_id=audit-probe-diag-2" | jq -c '{payload_keys:(.[0].payload|keys), has_top_unknown:(.[0].payload|has("top_unknown_field")), has_top_error_type:(.[0].payload|has("error_type")), turn_unknown:(.[0].payload.turns[0].turn_unknown_field), llm_calls:(.[0].payload.turns[0].llm_calls)}'
$PSQL -c "delete from traces where session_id like 'audit-probe-%';"
$PSQL -c "select count(*) from traces where session_id like 'audit-probe-%';"
$PSQL -c "select count(*) from traces;"
