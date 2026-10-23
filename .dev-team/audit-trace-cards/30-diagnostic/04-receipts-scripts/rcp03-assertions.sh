#!/usr/bin/env bash
set -u
: "${ADMIN:?.需要 export ADMIN=<pony-sentry ADMIN token>}"
ID=${ID:-ca0b5494-ae1b-47db-8e00-c9e7abbb6779}
URL=${URL:-http://127.0.0.1:3000/api/v1/traces}
curl -s -H "x-admin-token: $ADMIN" "$URL/$ID" -o /tmp/t3.json || exit 1
run () { desc="$1"; shift; out=$(jq -e "$@" /tmp/t3.json 2>&1); rc=$?; echo "[$desc] exit=$rc out=$out"; }
run "B1 turn 无 error_type"        '.payload.turns[0] | has("error_type") | not'
run "B2 turn 无 error_message"     '.payload.turns[0] | has("error_message") | not'
run "B3 turn 无 started_at_ms"     '.payload.turns[0] | has("started_at_ms") | not'
run "B4 turn 无 llm_calls"         '.payload.turns[0] | has("llm_calls") | not'
run "B5 payload 无 request_id"     '.payload | has("request_id") | not'
run "B6 payload 无 user_id"        '.payload | has("user_id") | not'
run "B7 payload 无顶层 status"      '.payload | has("status") | not'
run "B8 thread_id 键存在但值为 null" '.payload | (has("thread_id") and (.thread_id == null))'
run "B9 三条 tool_call 的 error 全为 null" '(.payload.turns[0].tool_calls | map(.error) | all(. == null))'
run "B10 前两条 status=success 且 output 含失败文案" '(.payload.turns[0].tool_calls[0:2] | map(.status) == ["success","success"]) and (.payload.turns[0].tool_calls[0].output | test("职位解析失败"))'
run "B11 第三条 status=error 且 error=null" '(.payload.turns[0].tool_calls[2].status == "error") and (.payload.turns[0].tool_calls[2].error == null)'
run "B12 step == other"             '.payload.turns[0].step == "other"'
run "B13 attempt_index == 1"        '.payload.turns[0].attempt_index == 1'
run "B14 degraded == false"         '.payload.turns[0].degraded == false'
run "B15 duration == total == 120272" '(.payload.turns[0].duration_ms == 120272) and (.payload.total_duration_ms == 120272)'
run "B16 cache_hit < input_tokens"  '.payload.turns[0].cache_hit_tokens < .payload.turns[0].input_tokens'
run "B17 session_id 形如 thread_ref_v1_*" '.session_id | test("^thread_ref_v1_[0-9a-f]{24}$")'
run "B18 payload 内不含业务 thread_id" '(. | tostring) | test("efd84b9f-1956-4660-be61-91cf1ab243d1") | not'
