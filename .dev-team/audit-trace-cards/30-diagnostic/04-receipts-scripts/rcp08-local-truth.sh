#!/usr/bin/env bash
set -u
POD=${POD:-job-copilot-postgres-0}; NS=${NS:-dev}
RUN=${RUN:-run_83c1879e338d442f953a5dd4b8d1646a}
kubectl exec -n "$NS" "$POD" -- psql -U job_copilot -d job_copilot -At -F'|' -c "
select span_type||'|'||name||'|'||status||'|'||coalesce(duration_ms::text,'-')||'|'||coalesce(output_summary->>'prompt_tokens','NULL')
     ||'|'||coalesce(output_summary->>'completion_tokens','NULL')||'|'||coalesce(output_summary->>'cached_input_tokens','NULL')
     ||'|'||coalesce(error_type,'NULL')||'|'||to_char(started_at,'HH24:MI:SS.MS')
from v2_agent_observability_span where run_id='$RUN' order by started_at;"
echo "--- events ---"
kubectl exec -n "$NS" "$POD" -- psql -U job_copilot -d job_copilot -At -F'|' -c "
select sequence_no, event_type, event_name, status, to_char(occurred_at,'HH24:MI:SS.MS')
from v2_agent_observability_event where run_id='$RUN' order by sequence_no;"
echo "--- token 对账 ---"
kubectl exec -n "$NS" "$POD" -- psql -U job_copilot -d job_copilot -At -F'|' -c "
select count(*), count(*) filter (where status='error'),
       sum((output_summary->>'prompt_tokens')::int), sum((output_summary->>'completion_tokens')::int), sum((output_summary->>'cached_input_tokens')::int)
from v2_agent_observability_span where run_id='$RUN' and span_type='llm';"
