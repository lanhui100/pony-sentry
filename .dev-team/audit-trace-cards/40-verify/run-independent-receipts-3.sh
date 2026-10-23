#!/usr/bin/env bash
# verifier (T4) B3：按 Lead 纪律重跑全部 HTTP 上报实验，session_id 唯一前缀 audit-probe-verify-<suffix>
# 只删自己前缀；他人行（audit-probe-merge*/total*）一律不碰。
set -uo pipefail
cd /home/dm/pony-sentry
OUT=.dev-team/audit-trace-cards/40-verify
ART=$OUT/artifacts
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
PSQL="psql -h 10.43.206.170 -U job_copilot -d ponysentry -X"
BASE=ca0b5494-ae1b-47db-8e00-c9e7abbb6779
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
ADMIN=a723309e828ea8366cc6d22380ab31b3b74f0b171f041c5a4c7e07ff2b03c16c
API=http://127.0.0.1:3000
SUF=9691a9
P="audit-probe-verify-$SUF"
r() { echo; echo "\$ $1"; bash -c "$1" 2>&1; echo "[exit $?]"; }

echo
echo "=============================================================="
echo "# B3 重跑（Lead 纪律：唯一前缀 $P）"
echo "# 背景：19:5x 有他人执行 delete ... like 'audit-probe-%'，B/B2 的实验行被外部删除；"
echo "#       本段全部实验改用唯一前缀，清理只删 $P%，并附不触碰他人行的收据。"
echo "# 时间: $(date -Iseconds)"
echo "=============================================================="

echo
echo "### B3-0 现场取证：当前 audit-probe 残留行归属（证明本次不误删他人行）"
r "$PSQL -c \"select session_id, created_at from traces where session_id like 'audit-probe%' order by session_id\""

echo
echo "### B3-1 前次自建行清理（仅精确 id 前缀 audit-probe-e8-phantom-id，属我 19:49 的探测行）"
r "$PSQL -c \"delete from traces where session_id='audit-probe-e8-phantom-id'\""

# ---------- E0 ----------
cat > "$ART/b3-e0-missing-session.json" <<JSON
{"project":"audit_probe","turns":[{"turn_id":"t-e0","status":"failed"}]}
JSON
echo
echo "### E0 session_id/thread_id 双缺 → 拒绝"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e0-missing-session.json $API/api/v1/traces -w '\\nhttp=%{http_code}\\n'"

# ---------- E1 ----------
cat > "$ART/b3-e1-unknown-top.json" <<JSON
{"session_id":"$P-e1","project":"audit_probe","run_id":"run_${SUF}_e1","turn_id":"turn_${SUF}_e1_1","environment":"development","release":"dev",
 "future_field_cost_usd":12.5,"unknown_nested_object":{"a":1},"error":{"type":"ValueError","message":"expected value at line 1 column 1"},
 "turns":[{"turn_id":"turn_${SUF}_e1_1","status":"failed","unknown_turn_field":"KEEP_ME_TURN_LEVEL","duration_ms":120272,
   "tool_calls":[{"tool_name":"format_jd_to_json","status":"error","error":null,"retry_count":3,"unknown_call_field":"KEEP_ME_CALL_LEVEL"}]}],
 "total_duration_ms":120272,"reported_at_ms":1792754960002}
JSON
echo
echo "### E1 未知顶层字段 / turns 内未知字段的保留差异"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e1-unknown-top.json $API/api/v1/traces -o $ART/b3-resp-e1.json -w 'http=%{http_code}\\n'"
r "jq -c '{top_keys:(.payload|keys), has_future_field:(.payload|has(\"future_field_cost_usd\")), has_error:(.payload|has(\"error\")), turn_unknown:.payload.turns[0].unknown_turn_field, call_unknown:.payload.turns[0].tool_calls[0].unknown_call_field, call_retry:.payload.turns[0].tool_calls[0].retry_count}' $ART/b3-resp-e1.json"
r "$PSQL -At -F'|' -c \"select payload ? 'future_field_cost_usd', payload ? 'error', payload->'turns'->0 ? 'unknown_turn_field', payload->'turns'->0->'tool_calls'->0 ? 'retry_count' from traces where session_id='$P-e1'\""

# ---------- E2 ----------
cat > "$ART/b3-e2-bad-turns.json" <<JSON
{"session_id":"$P-e2","project":"audit_probe","run_id":"run_${SUF}_e2",
 "turns":[{"turn_id":"turn_${SUF}_e2_1","status":12345,"tool_calls":"NOT_AN_ARRAY","input_tokens":"NOT_A_NUMBER","duration_ms":-1},"BARE_STRING_TURN",{},
          {"turn_id":"turn_${SUF}_e2_1","status":"failed","dup":true}],
 "total_input_tokens":-5}
JSON
echo
echo "### E2 turns 元素无 schema 校验（错类型/枚举外/负数/裸字符串/重复 turn_id 全部入库）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e2-bad-turns.json $API/api/v1/traces -o $ART/b3-resp-e2.json -w 'http=%{http_code}\\n'"
r "jq -c '.payload.turns' $ART/b3-resp-e2.json"
r "$PSQL -At -F'|' -c \"select jsonb_typeof(payload->'turns'), jsonb_array_length(payload->'turns'), payload->'turns'->0->>'status', jsonb_typeof(payload->'turns'->0->'tool_calls'), payload->'turns'->1, total_input_tokens from traces where session_id='$P-e2'\""
cat > "$ART/b3-e2b-turns-object.json" <<JSON
{"session_id":"$P-e2b","project":"audit_probe","turns":{"turn_id":"x"}}
JSON
echo "### E2b turns 为对象（serde 层唯一拦截的形态）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e2b-turns-object.json $API/api/v1/traces -w '\\nhttp=%{http_code}\\n'"
echo "### E2c 顶层不是对象"
printf '["session_id","%s-e2c"]\n' "$P" > "$ART/b3-e2c-root-array.json"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e2c-root-array.json $API/api/v1/traces -w '\\nhttp=%{http_code}\\n'"

# ---------- E3 + E5: 同 turn 二次上报 + 响应体 vs 库内 + 幻影 id ----------
cat > "$ART/b3-e3-first.json" <<JSON
{"session_id":"$P-e3","project":"audit_probe","run_id":"run_${SUF}_e3_A","turn_id":"turn_${SUF}_e3_1","environment":"development","release":"dev",
 "turns":[{"turn_id":"turn_${SUF}_e3_1","status":"failed","error":{"type":"TimeoutError","message":"OLD_FAILURE"},"duration_ms":120000,"input_tokens":100,"output_tokens":10,"attempt_index":1}],
 "total_input_tokens":100,"total_output_tokens":10,"total_duration_ms":120000,"reported_at_ms":1792754960002}
JSON
cat > "$ART/b3-e3-second.json" <<JSON
{"session_id":"$P-e3","project":"audit_probe","run_id":"run_${SUF}_e3_B","turn_id":"turn_${SUF}_e3_1","environment":"development","release":"dev",
 "turns":[{"turn_id":"turn_${SUF}_e3_1","status":"succeeded","error":null,"duration_ms":500,"input_tokens":999,"output_tokens":99,"attempt_index":2}],
 "total_input_tokens":999,"total_output_tokens":99,"total_duration_ms":500,"reported_at_ms":1792755000002}
JSON
echo
echo "### E3/E5 同一 session+turn 二次上报（failed→succeeded）：终态、总量、响应体、id"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e3-first.json $API/api/v1/traces -o $ART/b3-resp-e3-first.json -w 'POST1 http=%{http_code}\\n'"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e3-second.json $API/api/v1/traces -o $ART/b3-resp-e3-second.json -w 'POST2 http=%{http_code}\\n'"
echo "--- POST2 响应体（生产者看到的终态） ---"
r "jq -c '{resp_id:.id, run_id:.run_id, turn_status:.payload.turns[0].status, turn_attempt:.payload.turns[0].attempt_index, payload_total_in:.payload.total_input_tokens, resp_total_in:.total_input_tokens, resp_total_dur:.total_duration_ms, reported_at:.reported_at}' $ART/b3-resp-e3-second.json"
echo "--- 库内真值（同一条 session） ---"
r "$PSQL -At -F'|' -c \"select id, run_id, total_input_tokens, total_output_tokens, total_duration_ms, payload->'turns'->0->>'status', payload->'turns'->0->>'attempt_index', payload->'turns'->0->>'input_tokens', payload->'turns'->0->>'error', reported_at from traces where session_id='$P-e3'\""
echo "--- 幻影 id：用 POST 返回的 id 去 GET ---"
r "RID=\$(jq -r .id $ART/b3-resp-e3-second.json); curl -s -o $ART/b3-get-phantom.txt -w 'GET resp_id http=%{http_code}\\n' -H 'x-admin-token: $ADMIN' $API/api/v1/traces/\$RID; echo body=\$(cat $ART/b3-get-phantom.txt)"
r "DID=\$($PSQL -At -c \"select id from traces where session_id='$P-e3'\"); echo DB_ID=\$DID; curl -s -H 'x-admin-token: $ADMIN' $API/api/v1/traces/\$DID | jq -c '{id, run_id, total_input_tokens, total_duration_ms, turn_status:.payload.turns[0].status, payload_total_in:.payload.total_input_tokens}'"
echo "--- session 行数（唯一约束：1 session = 1 行） ---"
r "$PSQL -At -c \"select count(*) from traces where session_id='$P-e3'\""
echo "### E3c 第三次上报不同 turn（新增轮）观察 turns 数组顺序与 total 重算"
cat > "$ART/b3-e3-third.json" <<JSON
{"session_id":"$P-e3","run_id":"run_${SUF}_e3_C","turn_id":"turn_${SUF}_e3_2",
 "turns":[{"turn_id":"turn_${SUF}_e3_2","status":"succeeded","duration_ms":11,"input_tokens":5,"output_tokens":6}],
 "total_input_tokens":5,"total_output_tokens":6,"total_duration_ms":11,"reported_at_ms":1792755040002}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e3-third.json $API/api/v1/traces -o $ART/b3-resp-e3-third.json -w 'POST3 http=%{http_code}\\n'"
r "jq -c '[.payload.turns[]|{id:.turn_id,status:.status,tokens:.input_tokens}]' $ART/b3-resp-e3-third.json"
r "$PSQL -At -F'|' -c \"select total_input_tokens,total_output_tokens,total_duration_ms,run_id,turn_id,(select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t) from traces where session_id='$P-e3'\""

# ---------- E6: total 与 turns 矛盾 ----------
cat > "$ART/b3-e6-first.json" <<JSON
{"session_id":"$P-e6","project":"audit_probe","run_id":"run_${SUF}_e6","turn_id":"turn_${SUF}_e6_1",
 "turns":[{"turn_id":"turn_${SUF}_e6_1","status":"failed","duration_ms":5,"input_tokens":3,"output_tokens":4}],
 "total_input_tokens":999999,"total_output_tokens":1,"total_duration_ms":7,"reported_at_ms":1792754960002}
JSON
cat > "$ART/b3-e6-second.json" <<JSON
{"session_id":"$P-e6","run_id":"run_${SUF}_e6","turn_id":"turn_${SUF}_e6_2",
 "turns":[{"turn_id":"turn_${SUF}_e6_2","status":"succeeded","duration_ms":11,"input_tokens":5,"output_tokens":6}],
 "total_input_tokens":777777,"total_output_tokens":2,"total_duration_ms":13,"reported_at_ms":1792755000002}
JSON
echo
echo "### E6 total_* 与 turns 求和不一致：首次上报不做一致性校验，追加轮后改由 turns 求和重算（口径切换）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e6-first.json $API/api/v1/traces -o $ART/b3-resp-e6-first.json -w 'POST1 http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select total_input_tokens, total_output_tokens, total_duration_ms, (select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_in, (select sum((t->>'output_tokens')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_out from traces where session_id='$P-e6'\""
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e6-second.json $API/api/v1/traces -o $ART/b3-resp-e6-second.json -w 'POST2 http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select total_input_tokens, total_output_tokens, total_duration_ms, payload->>'total_input_tokens', (select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_in, jsonb_array_length(payload->'turns') from traces where session_id='$P-e6'\""

# ---------- E7: 同 session 两 run ----------
cat > "$ART/b3-e7-A.json" <<JSON
{"session_id":"$P-e7","project":"audit_probe","run_id":"run_${SUF}_e7_A","turn_id":"turn_${SUF}_e7_a1","turns":[{"turn_id":"turn_${SUF}_e7_a1","run_id":"run_${SUF}_e7_A","status":"failed"}],"total_input_tokens":10,"total_output_tokens":1,"total_duration_ms":100,"reported_at_ms":1792754960002}
JSON
cat > "$ART/b3-e7-B.json" <<JSON
{"session_id":"$P-e7","project":"audit_probe","run_id":"run_${SUF}_e7_B","turn_id":"turn_${SUF}_e7_b1","turns":[{"turn_id":"turn_${SUF}_e7_b1","run_id":"run_${SUF}_e7_B","status":"succeeded"}],"total_input_tokens":20,"total_output_tokens":2,"total_duration_ms":200,"reported_at_ms":1792755000002}
JSON
echo
echo "### E7 同 session 两个不同 run → 只 1 行，run_id 列只代表最新 run，旧 run 只活在 payload.turns 内"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e7-A.json $API/api/v1/traces -o /dev/null -w 'A http=%{http_code}\\n'"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b3-e7-B.json $API/api/v1/traces -o /dev/null -w 'B http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select count(*), run_id, turn_id, payload->'turns'->0->>'run_id', payload->'turns'->1->>'run_id', jsonb_array_length(payload->'turns'), total_input_tokens from traces where session_id='$P-e7' group by run_id, turn_id, payload, total_input_tokens\""
echo "### E7b List 接口按 run_id=A 检索是否可行（应不可行：参数被忽略）"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?run_id=run_${SUF}_e7_A' | jq -c '{n:length, has_run_A:([.[]|select(.run_id==\"run_${SUF}_e7_A\")]|length)}'"

echo
echo "############ B3-clean. 仅删自己前缀 $P% ############"
r "$PSQL -c \"select session_id, id from traces where session_id like '$P%' order by session_id\""
r "$PSQL -c \"delete from traces where session_id like '$P%'\""
r "$PSQL -At -c \"select count(*) from traces where session_id like '$P%'\" | tee $ART/verify-prefix-count-final.txt"
echo "--- 他人 audit-probe 行必须仍在（只读证明未误删） ---"
r "$PSQL -c \"select session_id from traces where session_id like 'audit-probe-%' order by session_id\" | tee $ART/other-probe-rows-after.txt"
echo "--- 基线护栏 ---"
r "$PSQL -At -c \"select md5(payload::text) from traces where id='$BASE'\" | tee $ART/baseline-md5-b3.txt"
r "diff $ART/baseline-md5-before.txt $ART/baseline-md5-b3.txt && echo 'BASELINE_UNCHANGED_OK exit=0'"
r "$PSQL -At -c \"select count(*) from traces where session_id like 'thread_ref_v1_%'\" | tee $ART/thread-ref-count-b3.txt"
r "diff $ART/thread-ref-count-before.txt $ART/thread-ref-count-b3.txt && echo 'THREAD_REF_COUNT_UNCHANGED_OK exit=0'"
r "$PSQL -At -c \"select count(*) from traces\""
echo
echo "############ B3 完毕 $(date -Iseconds) ############"
