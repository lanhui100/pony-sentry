#!/usr/bin/env bash
set -u
: "${PGPASSWORD:?.需要 export PGPASSWORD}"
: "${CLIENT:?.需要 export CLIENT}"
: "${ADMIN:?.需要 export ADMIN}"
HOST=${PSQL_HOST:-10.43.206.170}
PSQL="psql -h $HOST -U job_copilot -d ponysentry -At -F|"
URL=${URL:-http://127.0.0.1:3000/api/v1/traces}
S=audit-probe-diag-1
echo "## P0 写入前 audit-probe-* 行数"; $PSQL -c "select count(*) from traces where session_id like 'audit-probe-%';"
echo "## P1 首次上报"
curl -s -o /tmp/p1.json -w 'http_code=%{http_code}\n' -H "x-client-token: $CLIENT" -H 'content-type: application/json' -X POST "$URL" \
 -d '{"session_id":"audit-probe-diag-1","project":"job_copilot","environment":"development","release":"probe","run_id":"run-audit-1","turn_id":"turn-audit-1","total_duration_ms":120272,"turns":[{"turn_id":"turn-audit-1","run_id":"run-audit-1","status":"failed","step":"other","duration_ms":120272}]}'
jq -c '{id,session_id,run_id,turns:.payload.turns}' /tmp/p1.json
echo "## P2 同 turn_id 二次上报（期望被丢弃）"
curl -s -o /tmp/p2.json -w 'http_code=%{http_code}\n' -H "x-client-token: $CLIENT" -H 'content-type: application/json' -X POST "$URL" \
 -d '{"session_id":"audit-probe-diag-1","project":"job_copilot","environment":"development","release":"probe","run_id":"run-audit-1","turn_id":"turn-audit-1","turns":[{"turn_id":"turn-audit-1","run_id":"run-audit-1","status":"failed","step":"other","duration_ms":120272,"error_type":"TimeoutError","tool_calls":[{"tool_name":"format_jd_to_json","status":"error","error":"ValueError: expected value at line 1 column 1"}]}]}'
jq -c '{turns:.payload.turns}' /tmp/p2.json
echo "## P3 新 turn_id 上报（期望保留）"
curl -s -o /tmp/p3.json -w 'http_code=%{http_code}\n' -H "x-client-token: $CLIENT" -H 'content-type: application/json' -X POST "$URL" \
 -d '{"session_id":"audit-probe-diag-1","project":"job_copilot","environment":"development","release":"probe","turns":[{"turn_id":"turn-audit-2","run_id":"run-audit-1","status":"failed","tool_calls":[{"tool_name":"format_jd_to_json","status":"error","error":"ValueError: expected value at line 1 column 1","error_type":"ValueError","tool_call_id":"call_x"}],"llm_calls":[{"model":"workbuddy/deepseek-v4.1-flash","status":"error","duration_ms":13240}]}]}'
jq -c '{turns:.payload.turns}' /tmp/p3.json
echo "## P5 断言：同 turn_id 补写被丢弃（true=被丢弃）"
curl -s -H "x-admin-token: $ADMIN" "$URL?session_id=$S" | jq -e '[.[0].payload.turns[] | select(.turn_id=="turn-audit-1")][0] | has("error_type") | not'
echo "## P6 断言：error 与未知嵌套字段原样入库（true）"
curl -s -H "x-admin-token: $ADMIN" "$URL?session_id=$S" | jq -e '[.[0].payload.turns[] | select(.turn_id=="turn-audit-2")][0] | (.tool_calls[0].error | startswith("ValueError")) and (.tool_calls[0].error_type=="ValueError") and (.llm_calls[0].status=="error")'
echo "## P7/P8 清理与收据"
$PSQL -c "delete from traces where session_id like 'audit-probe-%';"
$PSQL -c "select count(*) from traces where session_id like 'audit-probe-%';"
$PSQL -c "select count(*) from traces;"
