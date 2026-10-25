#!/usr/bin/env bash
# wave-9 issue 治理与分析接口 —— 生产实测收据（只读为主 + 一次可逆归档/恢复 + 一次 ingest 冒烟）
#
# 凭据纪律：ADMIN_TOKEN / CLIENT_TOKEN / DATABASE_URL 只存在于本进程 shell 变量，
# 绝不打印、绝不落盘（本脚本输出只含状态码、响应头与响应体，请求头不回显）。
# 用法：bash .dev-team/receipts/run-wave9-curl-receipts.sh | tee .dev-team/receipts/curl-wave9.txt
set -uo pipefail
set -a
. <(sudo -n cat /etc/pony-sentry/pony-sentry.env)
set +a
BASE="${BASE_URL:-http://127.0.0.1:3000}"
AUTH=(-H "x-admin-token: ${ADMIN_TOKEN}")
J='python3 -c'

say() { printf '\n### %s [%s]\n' "$1" "$(date -Is)"; }
req() { # req <label> <curl args...>
  local label="$1"; shift
  local body; body="$(mktemp)"
  local head; head="$(mktemp)"
  local code
  code="$(curl -s -o "$body" -D "$head" -w '%{http_code}' "$@")"
  echo "--- $label -> HTTP $code"
  grep -iE '^(x-total-count|x-limit|x-offset|x-has-more|x-archived-mode|x-project-filter|x-next-cursor):' "$head" | tr -d '\r' | sed 's/^/    /'
  echo "    body: $(head -c 400 "$body")"
  rm -f "$body" "$head"
  return 0
}
reqjson() { # reqjson <label> <jq-py expr> <curl args...>
  local label="$1"; local expr="$2"; shift 2
  local body; body="$(mktemp)"
  local code
  code="$(curl -s -o "$body" -w '%{http_code}' "$@")"
  echo "--- $label -> HTTP $code"
  python3 -c "$expr" < "$body" 2>&1 | sed 's/^/    /'
  rm -f "$body"
  return 0
}

say "0. 服务与版本面"
curl -s "$BASE/healthz"; echo
echo "pid=$(pgrep -f '/usr/local/bin/pony-sentry-server' | head -1) bin_mtime=$(stat -c '%y' /usr/local/bin/pony-sentry-server)"
echo "systemd: $(systemctl is-active pony-sentry.service) / 自启: $(systemctl is-enabled pony-sentry.service)"

say "1. 验收 1：列表不再被 ignored 噪声淹没（project=pony-agent&status=unresolved → 只应 2 条 unresolved）"
reqjson "issues?project=pony-agent&status=unresolved" \
 'import json,sys,collections; d=json.load(sys.stdin); print("rows =", len(d)); print("status 分布 =", dict(collections.Counter(i["status"] for i in d))); print("ignored 泄漏 =", sum(1 for i in d if i["status"]=="ignored"))' \
 "${AUTH[@]}" "$BASE/api/v1/issues?project=pony-agent&status=unresolved"

say "2. 列表：project 过滤显式语义 + total 头（缺省 project 不再是「50 = 总数」的误读）"
req "issues?project=pony-agent（含 6 个响应头）" "${AUTH[@]}" "$BASE/api/v1/issues?project=pony-agent"
req "issues?limit=5（X-Total-Count 不应随 limit 变化）" "${AUTH[@]}" "$BASE/api/v1/issues?limit=5"
req "issues?limit=5&envelope=1" "${AUTH[@]}" "$BASE/api/v1/issues?limit=5&envelope=1"
req "issues?archived=only" "${AUTH[@]}" "$BASE/api/v1/issues?archived=only"
req "issues?status=bogus（应为 400，旧行为是静默忽略）" "${AUTH[@]}" "$BASE/api/v1/issues?status=bogus"
req "issues?limit=-5&offset=-10（B4：应为 400）" "${AUTH[@]}" "$BASE/api/v1/issues?limit=-5&offset=-10"

say "3. 验收 2：stats 返回 11 / 185 与分组值（含 stored_rows 行口径 196）"
reqjson "stats?project=pony-agent" \
 'import json,sys; d=json.load(sys.stdin); i=d["issues"]; e=d["events"]; print("issues.total =", i["total"]); print("events.total =", e["total"], "basis =", e["basis"], "stored_rows =", e["stored_rows"]); print("by_status =", i["by_status"]); print("by_level =", i["by_level"]); print("by_environment =", i["by_environment"]); print("series.len =", len(d["series"])); print("sum(series.events) =", sum(p["events"] for p in d["series"])); print("totals(附加) =", d.get("totals"))' \
 "${AUTH[@]}" "$BASE/api/v1/stats?project=pony-agent"

say "4. 验收 3：单 issue 事件分页（limit 生效 + cursor）"
ISSUE_ID="$(curl -s "${AUTH[@]}" "$BASE/api/v1/issues?project=pony-agent&status=ignored&limit=1" | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["id"])')"
echo "ignored issue id = $ISSUE_ID"
reqjson "issues/$ISSUE_ID/events?limit=5（必须恰好 5 行）" \
 'import json,sys; d=json.load(sys.stdin); print("rows =", len(d)); print("ids =", [x["id"][:8] for x in d])' \
 "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?limit=5"
reqjson "issues/$ISSUE_ID/events?limit=5&envelope=1（cursor 翻页）" \
 'import json,sys; d=json.load(sys.stdin); print("rows =", len(d["items"]), "has_more =", d["has_more"], "next_cursor =", (d["next_cursor"] or "")[:8] + "…<base64url cursor>", "total =", d["total"])' \
 "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?limit=5&envelope=1"
CURSOR="$(curl -s "${AUTH[@]}" -o /dev/null -D - "$BASE/api/v1/issues/$ISSUE_ID/events?limit=5" | grep -i '^x-next-cursor:' | tr -d '\r' | cut -d' ' -f2)"
if [ -n "${CURSOR:-}" ]; then
  reqjson "events?limit=5&cursor=<第 1 页游标>（第 2 页，无重叠）" \
   'import json,sys; d=json.load(sys.stdin); print("rows =", len(d), "ids =", [x["id"][:8] for x in d])' \
   "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?limit=5&cursor=$CURSOR"
fi
req "events?limit=0（应为 400）" "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?limit=0"
req "events?limit=99999（应为 200 且 X-Limit=100）" "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?limit=99999"
req "events?cursor=!!!bad!!!（应为 400 invalid cursor）" "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?cursor=!!!bad!!!"

say "5. 验收 4：归档不是删除（dry_run + 单条归档/恢复往返）"
reqjson "POST /issues/archive dry_run=1（project=pony-agent statuses=[ignored,resolved] older_than_days=1）" \
 'import json,sys; d=json.load(sys.stdin); print("dry_run =", d["dry_run"], "matched =", d["matched"], "archived =", d["archived"], "criteria =", d["criteria"])' \
 "${AUTH[@]}" -H 'content-type: application/json' -X POST \
 -d '{"project":"pony-agent","statuses":["ignored","resolved"],"older_than_days":1,"dry_run":true}' \
 "$BASE/api/v1/issues/archive"
echo "dry_run 后 archived_at 非空行数（应仍为 0）：$(curl -s "${AUTH[@]}" "$BASE/api/v1/issues?archived=only&project=pony-agent" | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))')"
BEFORE="$(curl -s "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID")"
echo "归档前: $(python3 -c 'import json,sys;d=json.loads(sys.argv[1]);print("count=",d["count"],"status=",d["status"],"archived_at=",d["archived_at"])' "$BEFORE")"
reqjson "POST /issues/$ISSUE_ID/archive" 'import json,sys; d=json.load(sys.stdin); print("archived_at =", d["archived_at"], "count =", d["count"], "status =", d["status"], "archive_restored_at =", d.get("archive_restored_at"))' \
 "${AUTH[@]}" -X POST "$BASE/api/v1/issues/$ISSUE_ID/archive"
reqjson "POST /issues/$ISSUE_ID/archive（重复幂等：archived_at 不变）" 'import json,sys; d=json.load(sys.stdin); print("archived_at =", d["archived_at"])' \
 "${AUTH[@]}" -X POST "$BASE/api/v1/issues/$ISSUE_ID/archive"
req "archived=only&project=pony-agent（归档项可查询）" "${AUTH[@]}" "$BASE/api/v1/issues?archived=only&project=pony-agent"
reqjson "归档后事件总数（应仍为 20 条，归档不删事件）" 'import json,sys; d=json.load(sys.stdin); print("rows =", len(d))' "${AUTH[@]}" "$BASE/api/v1/issues/$ISSUE_ID/events?limit=100"
reqjson "POST /issues/$ISSUE_ID/unarchive（可恢复）" 'import json,sys; d=json.load(sys.stdin); print("archived_at =", d["archived_at"], "archive_restored_at =", d.get("archive_restored_at"))' \
 "${AUTH[@]}" -X POST "$BASE/api/v1/issues/$ISSUE_ID/unarchive"
echo "恢复后 archived=only 行数（应回到 0）：$(curl -s "${AUTH[@]}" "$BASE/api/v1/issues?archived=only&project=pony-agent" | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))')"

say "6. 验收 5：治理指标（ingest 收到/接受/抑制）可查询"
reqjson "GET /api/v1/ingest/stats?window=24h" \
 'import json,sys; d=json.load(sys.stdin); print("window =", d["window"], "from =", d["from"], "to =", d["to"]); print("totals =", d["totals"]); print("by_project =", d["by_project"]); print("series =", d["series"])' \
 "${AUTH[@]}" "$BASE/api/v1/ingest/stats?window=24h"
req "GET /api/v1/ingest/stats?window=bogus（应为 400）" "${AUTH[@]}" "$BASE/api/v1/ingest/stats?window=bogus"
req "GET /api/v1/issues/archive（静态段优先 → 405）" "${AUTH[@]}" "$BASE/api/v1/issues/archive"
req "PATCH /api/v1/issues/archive（405）" "${AUTH[@]}" -X PATCH -H 'content-type: application/json' -d '{}' "$BASE/api/v1/issues/archive"

say "7. 验收 6：重启后 ingest 冒烟 —— event_id 幂等（created → duplicate）"
SMOKE_EID="wave9-smoke-$(date +%s)"
PAYLOAD="$(python3 - "$SMOKE_EID" <<'PY'
import json,sys
eid=sys.argv[1]
print(json.dumps({
  "event_id": eid,
  "message": "Wave9 governance smoke: archive/stats/pagination receipt",
  "level": "error",
  "platform": "rust",
  "project": "pony-sentry-governance-smoke",
  "environment": "dev",
  "release": "wave9-smoke",
  "exception": {"error_type": "Wave9GovernanceSmoke", "values": [{"type": "Wave9GovernanceSmoke", "value": "smoke " + eid}]},
  "stacktrace": {"frames": [{"filename": "smoke/wave9.rs", "function": "wave9_smoke", "lineno": 42}]},
}, ensure_ascii=False))
PY
)"
reqjson "POST /api/v1/ingest（首次，应 201 created）" \
 'import json,sys; d=json.load(sys.stdin); print("result =", d["result"], "duplicate =", d["duplicate"], "status =", d["status"], "event_id =", d["event_id"], "count =", d["count"], "issue_id =", d["issue_id"])' \
 "${AUTH[@]}" -H 'content-type: application/json' -X POST -d "$PAYLOAD" "$BASE/api/v1/ingest"
reqjson "POST /api/v1/ingest（同 event_id 重放，应 200 duplicate 且 count 不增）" \
 'import json,sys; d=json.load(sys.stdin); print("result =", d["result"], "duplicate =", d["duplicate"], "count =", d["count"])' \
 "${AUTH[@]}" -H 'content-type: application/json' -X POST -d "$PAYLOAD" "$BASE/api/v1/ingest"
reqjson "治理指标：created/duplicate/received 应各 +1（冒烟实证旁路计数生效）" \
 'import json,sys; d=json.load(sys.stdin); print("totals =", d["totals"]); print("by_project(smoke) =", [p for p in d["by_project"] if (p["project"] or "")=="pony-sentry-governance-smoke"])' \
 "${AUTH[@]}" "$BASE/api/v1/ingest/stats?window=24h"

say "8. 冒烟产物清理（避免污染列表）：PATCH ignored → 归档"
SMOKE_ISSUE="$(curl -s "${AUTH[@]}" "$BASE/api/v1/issues?project=pony-sentry-governance-smoke" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d[0]["id"] if d else "")')"
if [ -n "$SMOKE_ISSUE" ]; then
  req "PATCH issues/$SMOKE_ISSUE {status:ignored}" "${AUTH[@]}" -X PATCH -H 'content-type: application/json' -d '{"status":"ignored"}' "$BASE/api/v1/issues/$SMOKE_ISSUE"
  reqjson "POST issues/$SMOKE_ISSUE/archive" 'import json,sys; d=json.load(sys.stdin); print("archived_at =", d["archived_at"], "status =", d["status"])' "${AUTH[@]}" -X POST "$BASE/api/v1/issues/$SMOKE_ISSUE/archive"
else
  echo "    （未找到冒烟 issue，跳过清理）"
fi

say "9. 收尾：pony-agent 仍在列表且统计未变（归档/恢复往返无副作用）"
reqjson "stats?project=pony-agent（收尾复核）" \
 'import json,sys; d=json.load(sys.stdin); print("issues.total =", d["issues"]["total"], "events.total =", d["events"]["total"], "stored_rows =", d["events"]["stored_rows"], "by_status =", d["issues"]["by_status"])' \
 "${AUTH[@]}" "$BASE/api/v1/stats?project=pony-agent"
echo "# 收据结束 $(date -Is)"
