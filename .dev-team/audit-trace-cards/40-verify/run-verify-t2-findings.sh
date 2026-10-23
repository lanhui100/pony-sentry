#!/usr/bin/env bash
# verifier (T4) B4：对 T2 (auditor-core) findings 的逐条对抗复现
# 前缀仍为唯一 audit-probe-verify-9691a9*；只删自己前缀。
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
post() { curl -s -X POST -H "x-client-token: $CLIENT" -H 'content-type: application/json' --data "$1" "$API/api/v1/traces"; }

echo
echo "############ B4. 对 T2 findings 的对抗复现（W-01/W-02/W-03/W-04/W-05/M-01/M-03/I-03/I-04/I-05/I-09） ############"
echo "# 时间 $(date -Iseconds)  前缀 $P"

echo
echo "### [W-01] running→failed 反向：首报 running(无 tool_calls) → 二报 failed(带 error+tool_calls)"
cat > "$ART/b4-w01-first.json" <<JSON
{"session_id":"$P-w01","project":"audit_probe","run_id":"run_w01","turn_id":"turn_w01_1",
 "turns":[{"turn_id":"turn_w01_1","status":"running","duration_ms":1000}],"total_input_tokens":10,"total_output_tokens":1,"total_duration_ms":1000,"reported_at_ms":1792754960002}
JSON
cat > "$ART/b4-w01-second.json" <<JSON
{"session_id":"$P-w01","project":"audit_probe","run_id":"run_w01","turn_id":"turn_w01_1",
 "turns":[{"turn_id":"turn_w01_1","status":"failed","duration_ms":2000,"input_tokens":20,"output_tokens":2,
  "tool_calls":[{"tool_name":"t","status":"error","error":{"type":"ValueError","message":"boom"},"output":"OUT"}],
  "error":{"type":"ValueError","message":"terminal failure"}}],
 "total_input_tokens":20,"total_output_tokens":2,"total_duration_ms":2000,"reported_at_ms":1792755000002}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b4-w01-first.json $API/api/v1/traces -o /dev/null -w 'POST1 http=%{http_code}\\n'"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b4-w01-second.json $API/api/v1/traces -o /dev/null -w 'POST2 http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select payload->'turns'->0->>'status', coalesce(payload->'turns'->0->>'error','NULL'), coalesce(jsonb_array_length(payload->'turns'->0->'tool_calls'),-1), payload->'turns'->0->>'duration_ms', total_input_tokens, total_duration_ms from traces where session_id='$P-w01'\""
echo "→ 期望（W-01 成立）：status=running、error=NULL、tool_calls=0、duration=1000 保留；而 total_* 变成二报的 20/2000"

echo
echo "### [W-03] 二报不带 turns：total 取客户端值并被写入 payload.total_*，与既有 turns 求和矛盾"
cat > "$ART/b4-w03-first.json" <<JSON
{"session_id":"$P-w03","project":"audit_probe","run_id":"run_w03","turn_id":"turn_w03_1",
 "turns":[{"turn_id":"turn_w03_1","status":"failed","duration_ms":10,"input_tokens":5,"output_tokens":5}],
 "total_input_tokens":1000,"total_output_tokens":1000,"total_duration_ms":10,"reported_at_ms":1792754960002}
JSON
cat > "$ART/b4-w03-second-no-turns.json" <<JSON
{"session_id":"$P-w03","project":"audit_probe","run_id":"run_w03","turn_id":"turn_w03_1","tags":{"k":"v2"},
 "total_input_tokens":7777,"total_output_tokens":8888,"total_duration_ms":99,"reported_at_ms":1792755000002}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b4-w03-first.json $API/api/v1/traces -o /dev/null -w 'POST1 http=%{http_code}\\n'"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b4-w03-second-no-turns.json $API/api/v1/traces -o /dev/null -w 'POST2(no turns) http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select total_input_tokens, total_output_tokens, total_duration_ms, payload->>'total_input_tokens', payload->>'total_duration_ms', (select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_in, jsonb_array_length(payload->'turns') from traces where session_id='$P-w03'\""
echo "→ 期望（W-03 成立）：行 total=7777/8888/99 而 turns 求和=5，payload.total_input_tokens=7777（污染 payload）"

echo
echo "### [W-04] session 单行 + 列级 run_id/turn_id 被最后一次上报覆盖（已由 B3 E7 证实，此处补 turn_id 维度）"
r "$PSQL -At -F'|' -c \"select count(*) from (select session_id from traces where session_id like '$P-%' group by session_id having count(*)>1) s\""

echo
echo "### [W-05] reported_at_ms 无上下界：未来时间 9999999999999 是否被接受"
cat > "$ART/b4-w05-future.json" <<JSON
{"session_id":"$P-w05","project":"audit_probe","turn_id":"turn_w05_1","turns":[{"turn_id":"turn_w05_1","status":"failed"}],"reported_at_ms":9999999999999}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b4-w05-future.json $API/api/v1/traces -o /dev/null -w 'http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select reported_at, created_at, (reported_at - created_at) as skew from traces where session_id='$P-w05'\""

echo
echo "### [M-01] status=error 且无 error 字段仍被接受（消费者无契约校验）"
cat > "$ART/b4-m01.json" <<JSON
{"session_id":"$P-m01","project":"audit_probe","turns":[{"turn_id":"turn_m01_1","status":"failed","tool_calls":[{"tool_name":"format_jd_to_json","status":"error","tool_call_id":"c1"}]}]}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/b4-m01.json $API/api/v1/traces -o /dev/null -w 'http=%{http_code}\\n'"
r "$PSQL -At -F'|' -c \"select payload->'turns'->0->'tool_calls'->0->>'status', payload->'turns'->0->'tool_calls'->0->'error', payload->'turns'->0->'tool_calls'->0 ? 'error' from traces where session_id='$P-m01'\""

echo
echo "### [M-03] PATCH /traces/:id 的 note 是否落库（实验行自建→PATCH→查→删）"
r "SID=\$($PSQL -At -c \"select id from traces where session_id='$P-m01'\"); echo TRACE_ID=\$SID; curl -s -X PATCH -H 'x-admin-token: $ADMIN' -H 'content-type: application/json' -d '{\"eval_status\":\"triage_good\",\"note\":\"VERIFY_SHORT_NOTE_9691a9\"}' $API/api/v1/traces/\$SID -o $ART/b4-m03-patch.json -w 'PATCH http=%{http_code}\\n'; jq -c '{eval_status, has_note:(has(\"note\")), note:(.note//null)}' $ART/b4-m03-patch.json"
r "$PSQL -At -c \"select count(*) from traces where session_id='$P-m01' and payload::text like '%VERIFY_SHORT_NOTE_9691a9%'\""
echo "→ 期望（M-03 成立）：PATCH 200、eval_status 变更、note 在响应/库内均不存在"

echo
echo "### [I-04] limit 无 clamp / 无 payload 投影"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?limit=100000' -o $ART/b4-limit-100000.json -w 'http=%{http_code} bytes=%{size_download}\\n'; jq -r 'length' $ART/b4-limit-100000.json"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?limit=-5&offset=-3' -o $ART/b4-limit-negative.json -w 'limit=-5 http=%{http_code} bytes=%{size_download}\\n'; jq -r 'length' $ART/b4-limit-negative.json"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?limit=1' | jq -c '.[0]|keys'"

echo
echo "### [I-03] 未知查询参数被静默忽略（run_id / status / since / has_error）"
for q in "run_id=run_9691a9_e7_A" "status=failed" "since=2026-10-23T00:00:00Z" "has_error=true" "nonsense=x"; do
  r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?$q' -o /tmp/q.json -w 'q=$q http=%{http_code} '; jq -r 'length' /tmp/q.json"
done

echo
echo "### [I-05/I-09] 源码静态检查（turn.status / stats_incomplete）"
r "echo -n 'turn.status 命中: '; grep -c 'turn\\.status' web/src/App.vue || true"
r "echo -n 'stats_incomplete 前端命中: '; grep -c 'stats_incomplete' web/src/App.vue || true"
r "echo -n 'stats_incomplete 后端命中: '; grep -rn 'stats_incomplete' crates/ | wc -l"
r "echo -n 'attempt_index 前端命中: '; grep -c 'attempt_index' web/src/App.vue || true"
r "echo -n 'deny_unknown_fields 全仓: '; grep -rn 'deny_unknown_fields' crates/ | wc -l"
r "echo -n 'trace 指标(metrics) 命中 pow/pony_sentry_traces: '; grep -rn 'pony_sentry_traces' crates/ | wc -l"

echo
echo "############ B4-clean ############"
r "$PSQL -c \"delete from traces where session_id like '$P%'\""
r "$PSQL -At -c \"select count(*) from traces where session_id like '$P%'\""
r "$PSQL -At -c \"select count(*) from traces where session_id like 'audit-probe-%'\""
r "$PSQL -At -c \"select md5(payload::text) from traces where id='$BASE'\""
echo "############ B4 完毕 $(date -Iseconds) ############"
