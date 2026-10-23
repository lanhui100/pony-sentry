#!/usr/bin/env bash
set -u
: "${ADMIN:?.需要 export ADMIN}"
ID=${ID:-ca0b5494-ae1b-47db-8e00-c9e7abbb6779}; URL=${URL:-http://127.0.0.1:3000/api/v1/traces}
J=/tmp/diag_trace.json; curl -s -H "x-admin-token: $ADMIN" "$URL/$ID" -o "$J"
echo "## 顶层键";            jq -c 'keys' "$J"
echo "## payload 键";        jq -c '.payload|keys' "$J"
echo "## turns[0] 键";       jq -c '.payload.turns[0]|keys' "$J"
echo "## tool_calls";        jq -c '.payload.turns[0].tool_calls[] | {tool_name,status,error,duration_ms,out_len:(.output|length),args_len:(.arguments_summary|length)}' "$J"
echo "## 诊断字段存在性";     jq -c '{error_type:(.payload.turns[0]|has("error_type")),error_message:(.payload.turns[0]|has("error_message")),turn_started_at:(.payload.turns[0]|has("started_at_ms")),llm_calls:(.payload.turns[0]|has("llm_calls")),spans:(.payload|has("spans")),request_id:(.payload|has("request_id")),user_id:(.payload|has("user_id")),thread_id:(.payload|has("thread_id")),top_status:(.payload|has("status"))}' "$J"
echo "## token 口径";        jq -c '.payload|{total_input_tokens,total_output_tokens,total_duration_ms,reported_at_ms,turn:{input_tokens:.turns[0].input_tokens,output_tokens:.turns[0].output_tokens,cache_hit_tokens:.turns[0].cache_hit_tokens,duration_ms:.turns[0].duration_ms,status:.turns[0].status,attempt_index:.turns[0].attempt_index,degraded:.turns[0].degraded,step:.turns[0].step,model:.turns[0].model}}' "$J"
echo "## 时间字段";          jq -r '"reported_at=\(.reported_at) created_at=\(.created_at) reported_at_ms=\(.payload.reported_at_ms)"' "$J"
