#!/usr/bin/env bash
# verifier (T4) 补测脚本 B2/C2：POST 响应 vs 库内真实落盘对比 + 控制台关键字按出现次数统计
# 仍只用 session_id 前缀 audit-probe-，结束即删。
set -uo pipefail
cd /home/dm/pony-sentry
OUT=.dev-team/audit-trace-cards/40-verify
ART=$OUT/artifacts
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
PSQL="psql -h 10.43.206.170 -U job_copilot -d ponysentry -X"
BASE=ca0b5494-ae1b-47db-8e00-c9e7abbb6779
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
API=http://127.0.0.1:3000
r() { echo; echo "\$ $1"; bash -c "$1" 2>&1; echo "[exit $?]"; }

echo
echo "############ B2. POST 响应体 vs 库内落盘真值（同一时刻对比） ############"
echo "### 动机：crates/core/src/pg_repository.rs:698-701 返回 saved=trace（total_* 未用重算值），"
echo "###       而入库用的是 effective_* 重算值（:636-657）——验证二者是否分叉。"

cat > "$ART/probe-e5-first.json" <<'JSON'
{"session_id":"audit-probe-e5-resp-vs-db","project":"audit_probe","run_id":"run_probe_e5_A","turn_id":"turn_probe_e5_1","turns":[{"turn_id":"turn_probe_e5_1","status":"failed","duration_ms":120000,"input_tokens":100,"output_tokens":10}],"total_input_tokens":100,"total_output_tokens":10,"total_duration_ms":120000,"reported_at_ms":1792754960002}
JSON
cat > "$ART/probe-e5-second.json" <<'JSON'
{"session_id":"audit-probe-e5-resp-vs-db","project":"audit_probe","run_id":"run_probe_e5_B","turn_id":"turn_probe_e5_1","turns":[{"turn_id":"turn_probe_e5_1","status":"succeeded","duration_ms":500,"input_tokens":999,"output_tokens":99}],"total_input_tokens":999,"total_output_tokens":99,"total_duration_ms":500,"reported_at_ms":1792755000002}
JSON
echo
echo "### E5 step1 首次上报（turn=failed, totals=100/10/120000）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e5-first.json $API/api/v1/traces -o $ART/resp-e5-first.json -w 'http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select total_input_tokens,total_output_tokens,total_duration_ms,payload->'turns'->0->>'status',reported_at from traces where session_id='audit-probe-e5-resp-vs-db'\""
echo
echo "### E5 step2 同 turn 二次上报（turn=succeeded, totals=999/99/500）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e5-second.json $API/api/v1/traces -o $ART/resp-e5-second.json -w 'http=%{http_code}\\n'"
echo "--- POST 响应体（API 返回给生产者的真值） ---"
r "jq -c '{id:.id, run_id:.run_id, payload_turn_status:.payload.turns[0].status, payload_total_in:.payload.total_input_tokens, row_total_in:.total_input_tokens, row_total_dur:.total_duration_ms, reported_at:.reported_at}' $ART/resp-e5-second.json"
echo "--- 同一条 trace 库内真值（psql） ---"
r "$PSQL -At -F'|' -c \"select id,run_id,total_input_tokens,total_output_tokens,total_duration_ms,payload->'turns'->0->>'status',payload->'turns'->0->>'duration_ms',payload->'turns'->0->>'input_tokens',reported_at from traces where session_id='audit-probe-e5-resp-vs-db'\""
echo "--- GET /api/v1/traces/:id 真值（读路径） ---"
r "TRACE_ID=\$(jq -r .id $ART/resp-e5-second.json); curl -s -H 'x-admin-token: a723309e828ea8366cc6d22380ab31b3b74f0b171f041c5a4c7e07ff2b03c16c' $API/api/v1/traces/\$TRACE_ID | jq -c '{run_id,row_total_in:.total_input_tokens,row_total_dur:.total_duration_ms,payload_total_in:.payload.total_input_tokens,payload_total_dur:.payload.total_duration_ms,turn_status:.payload.turns[0].status,reported_at}'"

echo
echo "### E6 total_* 口径切换：首次上报 total 与 turns 求和矛盾 → 追加新轮后库内 total 是否被重算"
cat > "$ART/probe-e6-first.json" <<'JSON'
{"session_id":"audit-probe-e6-total-rule","project":"audit_probe","run_id":"run_probe_e6","turn_id":"turn_probe_e6_1","turns":[{"turn_id":"turn_probe_e6_1","status":"failed","duration_ms":5,"input_tokens":3,"output_tokens":4}],"total_input_tokens":999999,"total_output_tokens":1,"total_duration_ms":7,"reported_at_ms":1792754960002}
JSON
cat > "$ART/probe-e6-second.json" <<'JSON'
{"session_id":"audit-probe-e6-total-rule","run_id":"run_probe_e6","turn_id":"turn_probe_e6_2","turns":[{"turn_id":"turn_probe_e6_2","status":"succeeded","duration_ms":11,"input_tokens":5,"output_tokens":6}],"total_input_tokens":777777,"total_output_tokens":2,"total_duration_ms":13,"reported_at_ms":1792755000002}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e6-first.json $API/api/v1/traces -o $ART/resp-e6-first.json -w 'first http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select total_input_tokens, total_output_tokens, total_duration_ms, (select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t), (select sum((t->>'duration_ms')::int) from jsonb_array_elements(payload->'turns') t) from traces where session_id='audit-probe-e6-total-rule'\""
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e6-second.json $API/api/v1/traces -o $ART/resp-e6-second.json -w 'second http=%{http_code}\\n'"
echo "--- 追加新轮后：库行列 vs payload 内 total vs turns 求和 ---"
r "$PSQL -At -F'|' -c \"select total_input_tokens, total_output_tokens, total_duration_ms, payload->>'total_input_tokens', payload->>'total_duration_ms', (select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t), (select sum((t->>'duration_ms')::int) from jsonb_array_elements(payload->'turns') t), jsonb_array_length(payload->'turns') from traces where session_id='audit-probe-e6-total-rule'\""

echo
echo "### E7 覆盖式 upsert：同一 session 两个不同 run 的轮次如何并存（run_id 列被谁代表）"
cat > "$ART/probe-e7-runA.json" <<'JSON'
{"session_id":"audit-probe-e7-two-runs","project":"audit_probe","run_id":"run_probe_e7_A","turn_id":"turn_probe_e7_a1","turns":[{"turn_id":"turn_probe_e7_a1","run_id":"run_probe_e7_A","status":"failed"}],"total_input_tokens":10,"total_output_tokens":1,"total_duration_ms":100,"reported_at_ms":1792754960002}
JSON
cat > "$ART/probe-e7-runB.json" <<'JSON'
{"session_id":"audit-probe-e7-two-runs","project":"audit_probe","run_id":"run_probe_e7_B","turn_id":"turn_probe_e7_b1","turns":[{"turn_id":"turn_probe_e7_b1","run_id":"run_probe_e7_B","status":"succeeded"}],"total_input_tokens":20,"total_output_tokens":2,"total_duration_ms":200,"reported_at_ms":1792755000002}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e7-runA.json $API/api/v1/traces -o $ART/resp-e7-A.json -w 'A http=%{http_code}\\n'"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e7-runB.json $API/api/v1/traces -o $ART/resp-e7-B.json -w 'B http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select count(*), max(run_id), max(turn_id), payload->'turns'->0->>'run_id', payload->'turns'->1->>'run_id', jsonb_array_length(payload->'turns'), total_input_tokens from traces where session_id='audit-probe-e7-two-runs' group by payload, total_input_tokens\""
echo "--- 同一 session 是否只有一行（session_id 唯一） ---"
r "$PSQL -At -c \"select count(*) from traces where session_id='audit-probe-e7-two-runs'\""

echo
echo "############ B2-clean. 补测行清理 ############"
r "$PSQL -c \"delete from traces where session_id like 'audit-probe-%'\""
r "$PSQL -At -c \"select count(*) from traces where session_id like 'audit-probe-%'\""
r "$PSQL -At -c \"select count(*) from traces where session_id like 'audit-probe-%' or session_id='audit-probe-e0'\" | tee $ART/audit-probe-count-final.txt"
r "$PSQL -At -c \"select md5(payload::text) from traces where id='$BASE'\" | tee $ART/baseline-md5-after2.txt"
r "diff $ART/baseline-md5-before.txt $ART/baseline-md5-after2.txt && echo 'BASELINE_UNCHANGED_OK exit=0'"
r "$PSQL -At -c \"select count(*) from traces where session_id like 'thread_ref_v1_%'\" | tee $ART/thread-ref-count-after2.txt"
r "diff $ART/thread-ref-count-before.txt $ART/thread-ref-count-after2.txt && echo 'THREAD_REF_COUNT_UNCHANGED_OK exit=0'"

echo
echo "############ C2. 控制台渲染证据（按出现次数计，dist 为单行压缩文件） ############"
r "grep -o 'tool_calls' $ART/console-index.html | wc -l"
r "grep -o 'arguments_summary' $ART/console-index.html | wc -l"
r "grep -o '错误 error' $ART/console-index.html | wc -l"
r "grep -o '输出 output' $ART/console-index.html | wc -l"
r "grep -o '参数 arguments' $ART/console-index.html | wc -l"
r "grep -o '执行轨迹' $ART/console-index.html | wc -l"
r "grep -o '工具调用' $ART/console-index.html | wc -l"
r "grep -o 'Payload 详情' $ART/console-index.html | wc -l"
echo "--- turn 级字段是否有渲染：attempt_index / degraded / status（turn 头只渲染 phase/sequence/model/duration） ---"
r "grep -o 'attempt_index' $ART/console-index.html | wc -l"
r "grep -o 'degraded' $ART/console-index.html | wc -l"
r "grep -o 'cache_hit_tokens' $ART/console-index.html | wc -l"
r "grep -o 'turn.error' $ART/console-index.html | wc -l"
echo "--- dist 产物与源码同源（index.html 由 vite 单文件内联） ---"
r "sha256sum web/dist/index.html $ART/console-index.html"
echo
echo "############ 补测收据结束 $(date -Iseconds) ############"
