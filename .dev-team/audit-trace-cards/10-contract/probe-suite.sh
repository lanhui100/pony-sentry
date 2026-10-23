set -u
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
ADMIN=a723309e828ea8366cc6d22380ab31b3b74f0b171f041c5a4c7e07ff2b03c16c
BASE=http://127.0.0.1:3000
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
PSQL="psql -h 10.43.206.170 -U job_copilot -d ponysentry -tA"
TS=$(date +%Y%m%d%H%M%S); P="audit-probe-contract-$TS"
say(){ echo; echo "### $*"; }
post(){ curl -s -w '\nHTTP=%{http_code}\n' -X POST "$BASE/api/v1/traces" -H "x-client-token: $CLIENT" -H 'content-type: application/json' -d "$1"; }
say "ENV"; echo "run_at=$(date -Is) prefix=$P"; echo "pre_count_all=$($PSQL -c 'select count(*) from traces;')"

say "P1 未知顶层键 / 嵌套未知键（期望：顶层丢失、嵌套保留、HTTP 201）"
post "{\"session_id\":\"$P-p1\",\"project\":\"job_copilot\",\"schema_version\":\"v9\",\"future_key\":{\"a\":1},\"turns\":[{\"turn_id\":\"t1\",\"status\":\"failed\",\"error_type\":\"ValueError\",\"tool_calls\":[{\"tool_name\":\"format_jd_to_json\",\"status\":\"error\"}]}],\"total_duration_ms\":1,\"reported_at_ms\":1792756123000}"

say "P2 eval_status 非法枚举（期望 422，整条丢弃）"
post "{\"session_id\":\"$P-p2\",\"eval_status\":\"bogus\"}"

say "P3 total_duration_ms 字符串（期望 422）"
post "{\"session_id\":\"$P-p3\",\"total_duration_ms\":\"120272\"}"

say "P4 turns=null（期望 201，无完整性校验）"
post "{\"session_id\":\"$P-p4\",\"turns\":null,\"run_id\":\"run_p4\"}"

say "P5 同 session 二次上报（期望：run_id 列被覆盖、turn_id 列置 NULL、total 被改写）"
post "{\"session_id\":\"$P-p5\",\"run_id\":\"run_first\",\"turn_id\":\"turn_p1\",\"turns\":[{\"turn_id\":\"turn_p1\",\"status\":\"success\"}],\"total_duration_ms\":1}" >/dev/null
post "{\"session_id\":\"$P-p5\",\"run_id\":\"run_second\",\"turns\":[{\"turn_id\":\"turn_p2\",\"status\":\"succeeded\",\"duration_ms\":500}],\"total_duration_ms\":999}" >/dev/null
$PSQL -c "select run_id, coalesce(turn_id,'<NULL>'), total_duration_ms, jsonb_array_length(payload->'turns') as turns, payload->>'total_duration_ms' as payload_total from traces where session_id='$P-p5';"

say "P6 迟到补全被旧轮吞掉（期望 FAIL：新 error 不入库）"
S6="$P-p6"
post "{\"session_id\":\"$S6\",\"turns\":[{\"turn_id\":\"t1\",\"status\":\"success\"}]}" >/dev/null
post "{\"session_id\":\"$S6\",\"turns\":[{\"turn_id\":\"t1\",\"status\":\"failed\",\"tool_calls\":[{\"tool_name\":\"format_jd_to_json\",\"status\":\"error\",\"error\":\"ValueError: expected value at line 1 column 1\"}]}]}" >/dev/null
curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?session_id=$S6" | jq -c '.[0].payload.turns'

say "P7 tags 值非字符串（期望 422）"
post "{\"session_id\":\"$P-p7\",\"tags\":{\"app\":1}}"
say "P8 turns 为对象（期望 422）"
post "{\"session_id\":\"$P-p8\",\"turns\":{\"a\":1}}"
say "P9 session_id 空白（期望 400）"
post "{\"session_id\":\"   \"}"

say "P10 thread_id 兜底（期望：列=thread_id 而 payload.session_id=null）"
post "{\"thread_id\":\"$P-p10\",\"turns\":[]}" | tail -3
$PSQL -c "select session_id, coalesce(payload->>'session_id','<NULL>') p_sid, coalesce(payload->>'thread_id','<NULL>') p_tid from traces where session_id='$P-p10';"

say "P11 空 tool_call 对象 / 空 turns 数组（期望 201）"
post "{\"session_id\":\"$P-p11\",\"turns\":[{\"turn_id\":\"t\",\"tool_calls\":[{}]}]}" | tail -3
post "{\"session_id\":\"$P-p12\",\"turns\":[]}" | tail -3

say "P13 检索：?run_id= 是否生效（期望 FAIL：返回全量）"
ALL=$($PSQL -c "select count(*) from traces;")
N=$(curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?run_id=run_83c1879e338d442f953a5dd4b8d1646a&limit=100" | jq length)
echo "all=$ALL returned_with_run_id_filter=$N"

say "P14 /metrics 是否有 trace 指标（期望 0）"
echo "trace_metric_lines=$(curl -s "$BASE/metrics" | grep -c pony_sentry_trace)"

say "P15 目标行只读复核"
$PSQL -c "select run_id, coalesce(turn_id,'<NULL>') turn_id, eval_status, coalesce(payload->>'eval_status','<NULL>') p_eval, total_duration_ms, total_input_tokens, total_output_tokens, pg_column_size(payload) from traces where id='ca0b5494-ae1b-47db-8e00-c9e7abbb6779';"
$PSQL -c "select count(*) filter (where coalesce(payload->>'eval_status','<missing>') <> eval_status) mismatch, count(*) from traces;"

say "CLEANUP（仅删本前缀）"
$PSQL -c "delete from traces where session_id like '$P-%';"
echo "after_mine=$($PSQL -c "select count(*) from traces where session_id like '$P-%';")"
echo "post_count_all=$($PSQL -c 'select count(*) from traces;')"
echo "target_row_intact=$($PSQL -c "select count(*) from traces where id='ca0b5494-ae1b-47db-8e00-c9e7abbb6779';")"
