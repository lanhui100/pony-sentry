#!/usr/bin/env bash
# verifier (T4) 独立收据脚本 —— 我自己重跑，不复制 T2/T3 任何输出。
# 只读业务数据；实验写入仅限 session_id 前缀 audit-probe-，结束即删并留收据。
set -uo pipefail
cd /home/dm/pony-sentry
OUT=.dev-team/audit-trace-cards/40-verify
ART=$OUT/artifacts
mkdir -p "$ART"

export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
PSQL="psql -h 10.43.206.170 -U job_copilot -d ponysentry -X"
BASE=ca0b5494-ae1b-47db-8e00-c9e7abbb6779
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
ADMIN=a723309e828ea8366cc6d22380ab31b3b74f0b171f041c5a4c7e07ff2b03c16c
API=http://127.0.0.1:3000

r() { # 打印命令原文并原样输出 stdout/stderr，最后打印 exit code
  echo
  echo "\$ $1"
  bash -c "$1" 2>&1
  echo "[exit $?]"
}

echo "=============================================================="
echo "# 02-independent-receipts — verifier 独立复现（自跑，未复制 T2/T3）"
echo "# 采集时间: $(date -Iseconds)   主机: $(hostname)   cwd: $(pwd)"
echo "# 服务: pid $(pgrep -f '/usr/local/bin/pony-sentry-server' | head -1) /usr/local/bin/pony-sentry-server 监听 :3000"
echo "# git HEAD: $(git log -1 --format='%H %ci')"
echo "# 二进制 mtime: $(stat -c '%y' /usr/local/bin/pony-sentry-server)"
echo "=============================================================="

echo
echo "############ A. 库行 + payload 键全集（psql/jq，独立查询） ############"

r "$PSQL -x -c \"select id,session_id,run_id,turn_id,environment,release,project,eval_status,total_input_tokens,total_output_tokens,total_duration_ms,reported_at,created_at,updated_at,octet_length(payload::text) as payload_bytes, md5(payload::text) as payload_md5 from traces where id='$BASE'\""

echo
echo "--- A2 payload 顶层键（jsonb_object_keys, 排序） ---"
r "$PSQL -At -c \"select k from traces, jsonb_object_keys(payload) k where id='$BASE' order by k\""

echo
echo "--- A3 turns[0] 键全集 / tool_calls[] 键并集 ---"
r "$PSQL -At -c \"select jsonb_object_keys(payload->'turns'->0) from traces where id='$BASE'\" | sort -u"
r "$PSQL -At -c \"select distinct jsonb_object_keys(tc) from traces, jsonb_array_elements(payload->'turns') t, jsonb_array_elements(t->'tool_calls') tc where id='$BASE' order by 1\""

echo
echo "--- A4 结构计数与状态分布（turns/tool_calls/status/tool error 空值） ---"
r "$PSQL -c \"select jsonb_array_length(payload->'turns') as turns, (select count(*) from jsonb_array_elements(payload->'turns') t, jsonb_array_elements(t->'tool_calls') tc) as tool_calls, (select string_agg(t->>'status','|') from jsonb_array_elements(payload->'turns') t) as turn_status, (select string_agg(coalesce(tc->>'status','null'),'|') from jsonb_array_elements(payload->'turns') t, jsonb_array_elements(t->'tool_calls') tc) as tool_status, (select count(*) from jsonb_array_elements(payload->'turns') t, jsonb_array_elements(t->'tool_calls') tc where tc->'error' is null or tc->'error'='null'::jsonb) as tool_error_null from traces where id='$BASE'\""

echo
echo "--- A5 GET API 返回体 vs 库 payload 键集合一致性 ---"
r "curl -s -H 'x-admin-token: $ADMIN' $API/api/v1/traces/$BASE -o $ART/api-get-trace.json -w 'http=%{http_code}\\n'"
r "jq -r 'keys[]' $ART/api-get-trace.json | tr '\\n' ' '; echo"
r "jq -r '.payload|keys[]' $ART/api-get-trace.json | tr '\\n' ' '; echo"
r "jq -e '.payload.turns[0].tool_calls|length' $ART/api-get-trace.json"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?session_id=thread_ref_v1_e9b4ec395c96d8913c106ef8' -o $ART/api-list-session.json -w 'http=%{http_code}\\n'; jq -r 'length' $ART/api-list-session.json"

echo
echo "--- A6 List 接口筛选能力实测：run_id / turn_id / 时间窗参数是否被接受 ---"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?run_id=run_83c1879e338d442f953a5dd4b8d1646a' -o $ART/api-list-runid.json -w 'http=%{http_code}\\n'; jq -r 'length' $ART/api-list-runid.json"
r "curl -s -H 'x-admin-token: $ADMIN' '$API/api/v1/traces?since=2026-10-23T00:00:00Z' -o $ART/api-list-since.json -w 'http=%{http_code}\\n'; jq -r 'length' $ART/api-list-since.json"

echo
echo "############ B. 真实 HTTP 上报实验（POST $API/api/v1/traces） ############"
echo "### 实验前基线护栏：目标行 md5 与行数（实验后必须不变）"
r "$PSQL -At -c \"select md5(payload::text) from traces where id='$BASE'\" | tee $ART/baseline-md5-before.txt"
r "$PSQL -At -c \"select count(*) from traces where session_id like 'thread_ref_v1_%'\" | tee $ART/thread-ref-count-before.txt"
r "$PSQL -At -c \"select count(*) from traces where session_id like 'audit-probe-%'\""

# ---------- E0: session_id 缺失 ----------
cat > "$ART/probe-e0-missing-session.json" <<'JSON'
{"project":"audit_probe","turns":[{"turn_id":"t-e0","status":"failed"}]}
JSON
echo
echo "### E0 缺 session_id/thread_id 的上报（预期被拒）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e0-missing-session.json $API/api/v1/traces -w '\\nhttp=%{http_code}\\n'"

# ---------- E1: 未知顶层字段 ----------
cat > "$ART/probe-e1-unknown-top.json" <<'JSON'
{
  "session_id": "audit-probe-e1-unknown-top",
  "project": "audit_probe",
  "run_id": "run_probe_e1",
  "turn_id": "turn_probe_e1_1",
  "environment": "development",
  "release": "dev",
  "future_field_cost_usd": 12.5,
  "unknown_nested_object": {"a": 1, "b": [1,2,3]},
  "error": {"type": "ValueError", "message": "expected value at line 1 column 1", "stack": "..."},
  "turns": [
    {
      "turn_id": "turn_probe_e1_1",
      "status": "failed",
      "unknown_turn_field": "KEEP_ME_TURN_LEVEL",
      "duration_ms": 120272,
      "tool_calls": [
        {"tool_name": "format_jd_to_json", "status": "error", "error": null, "retry_count": 3, "unknown_call_field": "KEEP_ME_CALL_LEVEL"}
      ]
    }
  ],
  "total_duration_ms": 120272,
  "reported_at_ms": 1792754960002
}
JSON
echo
echo "### E1 未知顶层字段（future_field_cost_usd / unknown_nested_object / error）是否保留"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e1-unknown-top.json $API/api/v1/traces -o $ART/resp-e1.json -w 'http=%{http_code}\\n'"
r "jq -r '.payload|keys[]' $ART/resp-e1.json | tr '\\n' ' '; echo"
r "jq -r '.payload.future_field_cost_usd, .payload.unknown_nested_object, .payload.error' $ART/resp-e1.json"
r "jq -r '.payload.turns[0].unknown_turn_field, .payload.turns[0].tool_calls[0].retry_count, .payload.turns[0].tool_calls[0].unknown_call_field' $ART/resp-e1.json"
r "$PSQL -At -c \"select payload ? 'future_field_cost_usd', payload ? 'error', payload->'turns'->0 ? 'unknown_turn_field' from traces where session_id='audit-probe-e1-unknown-top'\""

# ---------- E2: turns 无 schema 校验 ----------
cat > "$ART/probe-e2-bad-turns.json" <<'JSON'
{
  "session_id": "audit-probe-e2-bad-turns",
  "project": "audit_probe",
  "run_id": "run_probe_e2",
  "turns": [
    {"turn_id": "turn_probe_e2_1", "status": 12345, "tool_calls": "NOT_AN_ARRAY", "input_tokens": "NOT_A_NUMBER", "duration_ms": -1},
    "BARE_STRING_TURN",
    {},
    {"turn_id": "turn_probe_e2_1", "status": "failed", "dup": true}
  ],
  "total_input_tokens": -5
}
JSON
echo
echo "### E2 turns 元素类型/枚举/负数是否被强校验（含重复 turn_id、裸字符串元素）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e2-bad-turns.json $API/api/v1/traces -o $ART/resp-e2.json -w 'http=%{http_code}\\n'"
r "jq -c '.payload.turns' $ART/resp-e2.json"
r "jq -r '.total_input_tokens' $ART/resp-e2.json"
r "$PSQL -At -c \"select jsonb_typeof(payload->'turns'), jsonb_array_length(payload->'turns'), payload->'turns'->0->>'status', jsonb_typeof(payload->'turns'->0->'tool_calls'), payload->'turns'->1 from traces where session_id='audit-probe-e2-bad-turns'\""

echo
echo "### E2b turns 不是数组（serde 层唯一会拒的形态）"
cat > "$ART/probe-e2b-turns-object.json" <<'JSON'
{"session_id":"audit-probe-e2b","project":"audit_probe","turns":{"turn_id":"x"}}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e2b-turns-object.json $API/api/v1/traces -w '\\nhttp=%{http_code}\\n'"
cat > "$ART/probe-e2c-root-not-object.json" <<'JSON'
["session_id","audit-probe-e2c"]
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e2c-root-not-object.json $API/api/v1/traces -w '\\nhttp=%{http_code}\\n'"

# ---------- E3: 同一 turn 二次上报终态 ----------
cat > "$ART/probe-e3-first.json" <<'JSON'
{
  "session_id": "audit-probe-e3-remerge",
  "project": "audit_probe",
  "run_id": "run_probe_e3_A",
  "turn_id": "turn_probe_e3_1",
  "environment": "development",
  "release": "dev",
  "turns": [
    {"turn_id": "turn_probe_e3_1", "status": "failed", "error": {"type": "TimeoutError", "message": "OLD_FAILURE"}, "duration_ms": 120000, "input_tokens": 100, "output_tokens": 10, "attempt_index": 1}
  ],
  "total_input_tokens": 100,
  "total_output_tokens": 10,
  "total_duration_ms": 120000,
  "reported_at_ms": 1792754960002
}
JSON
cat > "$ART/probe-e3-second.json" <<'JSON'
{
  "session_id": "audit-probe-e3-remerge",
  "project": "audit_probe",
  "run_id": "run_probe_e3_B",
  "turn_id": "turn_probe_e3_1",
  "environment": "development",
  "release": "dev",
  "turns": [
    {"turn_id": "turn_probe_e3_1", "status": "succeeded", "error": null, "duration_ms": 500, "input_tokens": 999, "output_tokens": 99, "attempt_index": 2}
  ],
  "total_input_tokens": 999,
  "total_output_tokens": 99,
  "total_duration_ms": 500,
  "reported_at_ms": 1792755000002
}
JSON
echo
echo "### E3 同一 session+turn 二次上报：终态取旧值还是新值"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e3-first.json $API/api/v1/traces -o $ART/resp-e3-first.json -w 'first http=%{http_code}\\n'"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e3-second.json $API/api/v1/traces -o $ART/resp-e3-second.json -w 'second http=%{http_code}\\n'"
r "jq -r '.payload.turns|length' $ART/resp-e3-second.json"
r "jq -c '{status:.payload.turns[0].status, err:.payload.turns[0].error, dur:.payload.turns[0].duration_ms, in:.payload.turns[0].input_tokens, attempt:.payload.turns[0].attempt_index}' $ART/resp-e3-second.json"
r "jq -c '{row_run_id:.run_id, row_turn_id:.turn_id, row_total_in:.total_input_tokens, row_total_out:.total_output_tokens, row_total_dur:.total_duration_ms}' $ART/resp-e3-second.json"
r "$PSQL -At -F'|' -c \"select run_id, turn_id, payload->'turns'->0->>'run_id', payload->'turns'->0->>'status', payload->'turns'->0->>'duration_ms', total_input_tokens, total_output_tokens, total_duration_ms, reported_at from traces where session_id='audit-probe-e3-remerge'\""
echo "--- E3 三次上报（再补一个 succeeded 增量轮）观察合并顺序 ---"
cat > "$ART/probe-e3-third.json" <<'JSON'
{
  "session_id": "audit-probe-e3-remerge",
  "run_id": "run_probe_e3_C",
  "turn_id": "turn_probe_e3_1",
  "turns": [{"turn_id": "turn_probe_e3_1", "status": "interrupted", "duration_ms": 7, "input_tokens": 1, "output_tokens": 1}],
  "total_input_tokens": 1, "total_output_tokens": 1, "total_duration_ms": 7
}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e3-third.json $API/api/v1/traces -o $ART/resp-e3-third.json -w 'third http=%{http_code}\\n'"
r "jq -c '.payload.turns' $ART/resp-e3-third.json"
r "jq -c '{row_total_in:.total_input_tokens,row_total_dur:.total_duration_ms}' $ART/resp-e3-third.json"

# ---------- E4: total_* 与 turns 不一致 ----------
cat > "$ART/probe-e4-total-mismatch.json" <<'JSON'
{
  "session_id": "audit-probe-e4-total-mismatch",
  "project": "audit_probe",
  "run_id": "run_probe_e4",
  "turn_id": "turn_probe_e4_1",
  "turns": [
    {"turn_id": "turn_probe_e4_1", "status": "failed", "duration_ms": 5, "input_tokens": 3, "output_tokens": 4}
  ],
  "total_input_tokens": 999999,
  "total_output_tokens": 1,
  "total_duration_ms": 7
}
JSON
echo
echo "### E4 total_* 与 turns 求和不一致时，库行取哪个口径（首次上报）"
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e4-total-mismatch.json $API/api/v1/traces -o $ART/resp-e4-first.json -w 'http=%{http_code}\\n'"
r "jq -c '{row_total_in:.total_input_tokens, row_total_out:.total_output_tokens, row_total_dur:.total_duration_ms, turns_sum_in:([.payload.turns[].input_tokens]|add), payload_total_in:.payload.total_input_tokens}' $ART/resp-e4-first.json"
r "$PSQL -At -F'|' -c \"select total_input_tokens, total_output_tokens, total_duration_ms, (select sum((t->>'input_tokens')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_in, (select sum((t->>'output_tokens')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_out, (select sum((t->>'duration_ms')::int) from jsonb_array_elements(payload->'turns') t) as turns_sum_dur from traces where session_id='audit-probe-e4-total-mismatch'\""
echo "### E4b 同一 session 追加第二轮后口径是否切换（合并路径重算 total）"
cat > "$ART/probe-e4-second-turn.json" <<'JSON'
{
  "session_id": "audit-probe-e4-total-mismatch",
  "run_id": "run_probe_e4",
  "turn_id": "turn_probe_e4_2",
  "turns": [{"turn_id": "turn_probe_e4_2", "status": "succeeded", "duration_ms": 11, "input_tokens": 5, "output_tokens": 6}],
  "total_input_tokens": 777777, "total_output_tokens": 2, "total_duration_ms": 13
}
JSON
r "curl -s -X POST -H 'x-client-token: $CLIENT' -H 'content-type: application/json' --data @$ART/probe-e4-second-turn.json $API/api/v1/traces -o $ART/resp-e4-second.json -w 'http=%{http_code}\\n'"
r "jq -c '{turns:([.payload.turns[]|{id:.turn_id,in:.input_tokens}]), row_total_in:.total_input_tokens, payload_total_in:.payload.total_input_tokens, payload_total_dur:.payload.total_duration_ms, row_total_dur:.total_duration_ms}' $ART/resp-e4-second.json"

echo
echo "############ B-clean. 实验行清理与护栏收据 ############"
r "$PSQL -c \"select session_id, id from traces where session_id like 'audit-probe-%' order by session_id\""
r "$PSQL -c \"delete from traces where session_id like 'audit-probe-%'\""
r "$PSQL -At -c \"select count(*) from traces where session_id like 'audit-probe-%'\""
r "$PSQL -At -c \"select md5(payload::text) from traces where id='$BASE'\" | tee $ART/baseline-md5-after.txt"
r "diff <(cat $ART/baseline-md5-before.txt) <(cat $ART/baseline-md5-after.txt) && echo 'BASELINE_UNCHANGED_OK exit=0'"
r "$PSQL -At -c \"select count(*) from traces where session_id like 'thread_ref_v1_%'\" | tee $ART/thread-ref-count-after.txt"
r "diff <(cat $ART/thread-ref-count-before.txt) <(cat $ART/thread-ref-count-after.txt) && echo 'THREAD_REF_COUNT_UNCHANGED_OK exit=0'"

echo
echo "############ C. 控制台渲染证据（静态产物 + 关键字） ############"
r "curl -s $API/ -o $ART/console-index.html -w 'http=%{http_code} bytes=%{size_download}\\n'"
r "stat -c '%y %s %n' web/dist/index.html $ART/console-index.html"
r "for k in 'tool_calls' 'arguments_summary' 'output' \"call.error\" 'trace' 'session_id' '步骤/工具' '输出 output' '错误 error'; do printf '%-22s %s\\n' \"\$k\" \"\$(grep -c -- \"\$k\" $ART/console-index.html)\"; done"
r "grep -o 'tool_calls[^\"]\\{0,40\\}' $ART/console-index.html | head -5"
echo
echo "############ D. 源码定位（运行中二进制 = git HEAD 编译；行号可对源码） ############"
r "grep -n 'pub turns: Option<Vec<serde_json::Value>>' crates/server/src/routes.rs"
r "grep -n 'pub struct AgentTraceIngestRequest' crates/server/src/routes.rs"
r "grep -n 'ON CONFLICT (session_id) DO UPDATE' crates/core/src/pg_repository.rs"
r "grep -n 'merge_turns\\|旧轮保留' crates/core/src/trace_merge.rs"
r "grep -n 'call.error\\|call.output\\|arguments_summary' web/src/App.vue | head -12"
echo
echo "############ 收据结束 $(date -Iseconds) ############"
