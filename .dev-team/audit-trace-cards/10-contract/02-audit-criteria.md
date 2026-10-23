# trace 采集审核判据（T2/T3 共同口径）

> T1 · protocol-scribe · 2026-10-23。唯一锚点：`00-baseline/00-brief.md` §4（错/漏/优化三维度）+ §5（证据标准）。
> 用法：T2（pony-sentry 侧）与 T3（诊断充分性 + 生产者语义）**逐条执行**本清单，不得另立判据；
> 结论卡必须写 `判据 ID + PASS/FAIL + 机器收据 + 严重度`。路径缩写沿用 `01-field-matrix.md` §0.1。

---

## 0. 执行约定

### 0.1 环境与变量（复制即用）

```bash
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
PSQL="psql -h 10.43.206.170 -U job_copilot -d ponysentry -tA"
ADMIN=a723309e828ea8366cc6d22380ab31b3b74f0b171f041c5a4c7e07ff2b03c16c
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
BASE=http://127.0.0.1:3000
TID=ca0b5494-ae1b-47db-8e00-c9e7abbb6779
SID=thread_ref_v1_e9b4ec395c96d8913c106ef8
RID=run_83c1879e338d442f953a5dd4b8d1646a

# 统一取数：接口真值（与库逐键相等，见 01 §0.3）
curl -s -H "x-admin-token: $ADMIN" "$BASE/api/v1/traces/$TID" > /tmp/t.json
jq -e . /tmp/t.json > /dev/null && echo "receipt ok"
```

### 0.2 判定规则与严重度

- **PASS / FAIL**：命令输出满足该条 `PASS 判据` 即记 PASS，否则 FAIL；命令自身报错（如 `jq` 非 0 退出）记
  **BLOCKED**，必须换等价命令重试，不得默认放行。
- **严重度**（与简报 §4 一致，不得自创第四级）：
  - `P0` 阻断诊断：缺了它，本次事故**无法回答**"为什么失败"；
  - `P1` 显著削弱：能回答但需跨系统拼凑/靠猜；
  - `P2` 体验与效率：不影响结论，影响检索/复盘/告警成本。
- **每条结论的 5 要素**（简报 §4 强制）：`判定` + `严重度` + `证据`（path:line 或收据）+ `影响`（用本次事故说"因此无法回答哪个问题"）+ `建议`（最小改动 / 成本 / 是否破坏既有契约）。
- **禁止**：把简报 §8 的既有设计判为缺陷（见本文件 §4）；把 `[NON-MACHINE]` 项当成机器结论。

### 0.3 写探针的唯一约束（沿用简报 §9）

实验行 `session_id` 必须以 `audit-probe-` 开头，用完**按本卡前缀精确删除**并留收据：

```bash
$PSQL -c "delete from traces where session_id like 'audit-probe-<yourname>-%';"
$PSQL -c "select count(*) from traces where session_id like 'audit-probe-<yourname>-%';"   # 期望 0
```

> 警告（已实测）：**不要**用 `like 'audit-probe-%'` 这类跨成员前缀删除——本卡探针行曾被他成员误删
> （见 `03-probe-receipts.md` §4）。删除前先 `select session_id` 列出目标行。

---

## 1. 错（Wrong）判据

> 定义：trace 记录的内容与事实不符，或语义自相矛盾。**必须能指出"哪个字段与哪个事实冲突"**，否则归 M。

### W1 · `status=success` 但 `output` 是失败文案（语义矛盾）

- **判定**：`status` 与 `output` 的语义方向必须一致。
  ```bash
  jq -r '[.payload.turns[].tool_calls[]
          | select(.status=="success")
          | select((.output // "") | test("失败|错误|error|Error|超时|timeout|异常"))]
         | {count:length, samples:[.[]|{tool_name, status, head:(.output[0:40])}]}' /tmp/t.json
  ```
- **PASS**：`count==0`。**FAIL**：`count>0`（需人工复核文案是否为业务失败语义）。
- **本次预判**：**FAIL**，`count=2`（`format_jd_to_json` 第 1、2 条，output 以「职位解析失败（原因: 模型返回空结果）」开头，而 status=success）。
- **严重度**：`P0`（读 `status=success` 会得出"工具正常"的错误结论，直接误导 triage）。
- **影响**：因此无法回答"3 次工具调用里到底几次真失败"——按 status 统计是 1 次，按 output 文案是 3 次全败。
- **归属**：T2（消费者零一致性校验）+ T3（生产者 status 来源 = 中间件 outcome，`tool_observability_middleware.py:111`）。

### W2 · 顶层 `total_*` / `attempt_index` 口径漂移

- **判定**：首次上报与合并后的同名字段必须可用同一口径解释；`attempt_index` 必须有定义。
  ```bash
  # (a) 列值 vs payload 值 vs turns 求和
  $PSQL -c "select total_duration_ms as col,
                   (payload->>'total_duration_ms')::bigint as payload_val,
                   (select sum((t->>'duration_ms')::bigint)
                      from jsonb_array_elements(payload->'turns') t) as turn_sum,
                   (select sum((c->>'duration_ms')::bigint)
                      from jsonb_array_elements(payload->'turns') t,
                           jsonb_array_elements(coalesce(t->'tool_calls','[]'::jsonb)) c) as tool_sum
              from traces where id='$TID';"
  # (b) attempt_index 的语义定义（契约文本）
  grep -rn "attempt_index" /home/dm/job_copilot/.dev-team/contracts-pony-sentry-trace.md | head
  grep -rn "attempt_index" /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py
  ```
- **PASS**：口径在契约中有定义且三者可解释（如 `col==payload_val`，且 turn_sum 的语义有明文）。
  **FAIL**：同名字段在不同路径下取值口径不同（wall-clock vs 求和），或 `attempt_index` 无定义。
- **本次预判**：**FAIL**。(a) 三者相等（120272/120272/120272，tool_sum=98602 < 120272：工具外耗时 21670ms **无字段承载**）；
  (b) `PY:242` 只透传，`CD` 无定义；样本 `attempt_index=1` 却在同一 turn 内出现同一工具 **3 次**调用。
  合并改写历史值有一手收据：`03-probe-receipts.md` P5（旧 `total_duration_ms=1` → 新 `500`，旧值不可复现）。
- **严重度**：`P1`。**影响**：无法判断"120s 是模型耗时、工具耗时还是空转"，也无法判断这是第 1 次尝试还是重试后的末次。
- **归属**：T2（合并重算 + 列反写，`T:20-49`、`P:636-657`）+ T3（生产者多轮求值 `PY:356-368`）。

### W3 · `run_id` / `turn_id` 列被合并覆盖，与 payload 内 turn 事实失配

- **判定**：列声明的 run/turn 必须能在 payload.turns 中找到，且不得因二次上报丢失首个 run 的标识。
  ```bash
  $PSQL -c "select count(*) filter (where coalesce(run_id,'') is distinct from coalesce(payload->>'run_id',''))
                   as run_mismatch,
                   count(*) filter (where coalesce(turn_id,'') is distinct from coalesce(payload->>'turn_id',''))
                   as turn_mismatch, count(*) from traces;"
  ```
- **PASS**：`run_mismatch=turn_mismatch=0` **且** 多轮行的每个 turn 都能反查所属 run。**FAIL**：任一不满足。
- **本次预判**：**FAIL（机制级）**。目标行为单次上报、列与 payload 一致；但 `P:665-677` 的
  `run_id=EXCLUDED.run_id` 使二次上报直接覆盖：探针 P5 后列 `run_id=run_second`、`turn_id=NULL`，而 payload 内含
  `turn_p1`/`turn_p2` 两轮。全表口径命令预期 `>0`（T2 需在写探针后重跑确认）。
- **严重度**：`P1`。**影响**：因此无法按 run_id 精确定位"某次 run 的那一轮"（列只剩最后上报值），事故 run 与同会话其它 run 不可区分。
- **归属**：T2。

### W4 · `session_id` 列与 `payload.session_id` 不一致（`thread_id` 兜底路径）

- **判定**：两处必须相等，或 payload 必须显式记录"本行来自 thread_id 兜底"。
  ```bash
  $PSQL -c "select count(*) filter (where payload->>'session_id' is null) as payload_null_sid,
                   count(*) filter (where coalesce(payload->>'session_id', session_id) is distinct from session_id)
                   as mismatch, count(*) from traces;"
  ```
- **PASS**：`mismatch=0` 且不存在 `payload_null_sid>0` 的隐式兜底行。**FAIL**：否则。
- **本次预判**：**FAIL**（`R:1202-1211` 允许兜底；探针 P10 实测列=thread_id、`payload.session_id=null`、
  `payload.thread_id` 有值 → 同一行的"会话标识"有两个家的不同答案）。
- **严重度**：`P1`。**影响**：因此无法判断某行 trace 的 session 到底是 `thread_ref` 还是原始 thread_id，跨行聚合会漏。
- **归属**：T2。

### W5 · `eval_status` 双写不一致（列 vs payload）

- **判定**：`payload.eval_status` 与列必须一致，或明确"payload 不承载该字段"。
  ```bash
  $PSQL -c "select count(*) filter (where coalesce(payload->>'eval_status','<missing>') <> eval_status) as mismatch,
                   count(*) from traces;"
  ```
- **PASS**：`mismatch=0`（或 payload 不含该键且文档声明）。**FAIL**：payload 含该键且与列恒不等。
- **本次预判**：**FAIL**。目标行 payload `eval_status=null`（`R:1196` 注入）而列为 `unreviewed`（`01-db-truth.txt:5`）；
  `P:778` PATCH 只改列不动 payload。
- **严重度**：`P2`。**影响**：不影响本次根因，但复核者从 payload 读标注状态会恒得 `null`。
- **归属**：T2。

### W6 · 无绝对事件时间戳，trace 无法与日志/前端事件对齐

- **判定**：除"上报时刻"外，必须存在能把 trace 锚到事故窗口的时间证据（turn/tool 级起止或 trace 起始时刻）。
  ```bash
  # payload 内的绝对时间键（期望 PASS 时 >=1）
  jq -r '[.. | objects | keys[] | select(test("^(ts|start|end|timestamp|.*_at|.*_ms)$"))] | unique' /tmp/t.json
  # 与 reported_at 的对照
  jq -r '.payload.reported_at_ms as $ms
         | "reported_at=\($ms|./1000|todate)  turns[0].duration_ms=\(.payload.turns[0].duration_ms)"' /tmp/t.json
  ```
- **PASS**：payload 内存在 >=1 个绝对时间键（且与事故窗口可对齐）。**FAIL**：只有 `reported_at_ms`。
- **本次预判**：**FAIL**。只有 `reported_at_ms=1792754960002`（19:29:20.002 CST，**上报**时刻），
  `turns[0].duration_ms=120272` 是相对量；事故窗口 19:27–19:29 与
  `/var/log/job-copilot-dev/api-v2-dev.log:187617,187692` 在 trace 内无锚点。
- **严重度**：`P1`。**影响**：因此无法回答"这次 120s 超时是 19:27:20 起还是 19:28:40 起"，也无法把 trace 与同一秒的服务日志/前端 sse 事件对齐。
- **归属**：T3（生产者不采集时间事实）+ T2（无时间段落库/索引）。

### W7 · `tool_calls[].status` 值域坍缩为三值，失败类型不保真

- **判定**：不同失败类型（timeout / 校验失败 / 模型空结果 / 网络）在 trace 中必须可区分。
  ```bash
  # 生产者归一规则（静态）
  sed -n '272,300p' /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py
  # 机器复现（纯函数，不写库）
  cd /home/dm/job_copilot/api_v2 && .venv/bin/python -c "
  from src.modules.agent.pony_sentry_trace.payload import build_trace_payload
  kw=dict(thread_id='thread-abc12345',run_id='run-abc12345',trace_id='trace-abc12345',status='failed',
          environment='development',release='1.2.3',turn_id='turn-abc12345',reported_at_ms=1700000000000,
          tool_calls=[{'tool_name':'format_jd_to_json','status':'timeout','error':'TimeoutError'}])
  print(build_trace_payload(**kw)['turns'][0]['tool_calls'][0]['status'])" 2>/dev/null | tail -1
  ```
- **PASS**：上游不同 outcome 落到不同的 `status` 字面量（且 `CD` 有值域表）。**FAIL**：多类失败坍缩为同一字面量。
- **本次预判**：**FAIL**。`PY:275-280` 只留 `success|rejected|error`；机器复现输出 `error`（`03-probe-receipts.md` R2）。
- **严重度**：`P0`。**影响**：因此无法回答"第 3 次调用是超时、是模型空结果，还是工具自身抛异常"——本次三者皆有嫌疑。
- **归属**：T3 主判，T2 记录"消费者接受任意 status 字面量、无值域校验"作为放大因素。

### W8 · 缺省键 / 显式空值 / 收敛占位不可区分

- **判定**：`键缺失` 必须表达一种确定语义；不得与"有键但值为空/占位"混用。
  ```bash
  # (a) status=error 的调用是否缺 output 键
  jq -r '[.payload.turns[].tool_calls[] | select(.status=="error")
          | {has_output:has("output"), error:.error, has_error_key:has("error")}]' /tmp/t.json
  # (b) step 是否被收敛为占位
  jq -r '.payload.turns[0].step' /tmp/t.json
  # (c) 条件键集合（interrupt / rejection_code / validator）在 payload 中的存在性
  jq -r '[.payload.turns[] | {has_interrupt:has("interrupt")}]' /tmp/t.json
  ```
- **PASS**：缺失语义在契约中枚举（如"键不出现 = 无输出"），且收敛占位（`other`）带原值或原因。
  **FAIL**：靠键缺失表达状态且无文档，或 `other` 覆盖了可诊断的真实值。
- **本次预判**：**FAIL**。(a) `status=error` 的调用**无 `output` 键**（`PY:307-308` 空值不写键）→ "工具无输出"与"没采集到"不可区分；
  (b) `step="other"`，真实节点名（`embedded_runtime`）无任何残留；`PY:296` 同时把 `error` 写成 `null`
  → `error:null` 与"无 error 键"不可区分。
- **严重度**：`P1`。**影响**：因此无法判断第 3 次调用是"返回空"还是"根本没到返回阶段"。
- **归属**：T3（生产者键语义）+ T2（无 schema 无法把缺失变成显式 `null`/占位）。

---

## 2. 漏（Missing）判据

> 定义：为定位该类失败所必需、但 trace 中完全缺失的信息。判 FAIL 时必须指出**"该字段应当存在的位置"**。

### M1 · 工具错误详情缺失（`error` 恒 null）

- **判定**：`status=error` 的 tool_call 必须携带 `error`（类型/消息）或至少 `error_type`。
  ```bash
  jq -r '[.payload.turns[].tool_calls[] | select(.status=="error")
          | {tool_name, error, has_output:has("output"), has_error_type:has("error_type")}]' /tmp/t.json
  # 全 payload 的错误类键（期望存在 >=1）
  jq -r '[paths | map(tostring) | join(".") | select(test("error_type|error_message|stacktrace|exception|stack"))] | length' /tmp/t.json
  ```
- **PASS**：每个 `status=error` 调用都有非空 `error` 或 `error_type`。**FAIL**：`error` 为 null 且无替代字段。
- **本次预判**：**FAIL**。3 条 `error` 全 `null`；全 payload 无 `error_type`/`stacktrace` 类键（计数 0）。
  事故真因 `ValueError: expected value at line 1 column 1` 在 trace 中**零痕迹**（只在 job_copilot 日志）。
- **严重度**：`P0`。**影响**：因此**无法回答"这次 run 为什么失败"**——这是本样本最致命的缺口。
- **归属**：T3（`EX:296` 只接收上游 `error_message`，中间件 `tool_observability_middleware.py:115` 该值为 None）
  + T2（无校验允许 error 缺失）。

### M2 · LLM 调用级 span 缺失（只有 turn 级聚合）

- **判定**：本次失败发生在模型结构化输出（空 JSON），trace 必须能定位"哪一次模型调用、返回了什么"。
  ```bash
  # 是否存在 span/llm_call 级对象（期望 PASS 时 >=1）
  jq -r '[.. | objects | select(has("span_id") or has("llm_call_id") or has("finish_reason") or has("prompt"))] | length' /tmp/t.json
  # 现有粒度
  jq -r '.payload.turns[0] | {model, input_tokens, output_tokens, cache_hit_tokens}' /tmp/t.json
  ```
- **PASS**：存在按调用粒度的 span（含 model/latency/finish_reason/output 摘要）。**FAIL**：仅 turn 级聚合。
- **本次预判**：**FAIL**（计数 0；只有 `model=workbuddy/deepseek-v4.1-flash` + 聚合 token 36612/1368/23936）。
- **严重度**：`P0`。**影响**：因此无法回答"空 JSON 是哪一步、第几次模型调用产生的"——而这是事故根因。
- **归属**：T3（生产者 `EX` 只聚合 `_RUN_LLM_DETAILS`）+ T2（`turns` 无嵌套 span 的 schema 位）。

### M3 · 重试语义缺失

- **判定**：同名工具多次调用必须能读出"这是第几次重试、上一次为何失败、是否有 backoff/上限"。
  ```bash
  jq -r '[.payload.turns[].tool_calls[] | .tool_name] | group_by(.) | map({tool:.[0], calls:length})' /tmp/t.json
  jq -r '.payload.turns[0].attempt_index' /tmp/t.json
  jq -r '[.. | objects | select(has("retry_index") or has("retry_of") or has("retry_count") or has("attempt"))] | length' /tmp/t.json
  ```
- **PASS**：重复调用带 retry 语义字段（或 `attempt_index` 明确表示工具重试且逐次递增）。**FAIL**：无任何 retry 字段，`attempt_index` 恒 1。
- **本次预判**：**FAIL**。`format_jd_to_json` × 3（`attempt_index=1`），无 `retry_*` 字段（计数 0）。
- **严重度**：`P1`。**影响**：因此无法回答"3 轮重试是框架自动重试还是模型自己重复调用、重试间隔多久"。
- **归属**：T3。

### M4 · 超时语义缺失（120s 节点超时在 trace 内不可见）

- **判定**：`duration_ms` 触达阈值时必须能读出"超时"，以及超时的节点/阶段/阈值。
  ```bash
  jq -r '.payload.turns[0] | {status, duration_ms, step,
          timeout_marked: (has("timeout") or has("timed_out") or has("timeout_ms") or has("deadline_ms"))}' /tmp/t.json
  # 生产者是否在 turns 上采集 timeout 事实
  grep -nE "timeout|timed_out|deadline" /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py
  ```
- **PASS**：`duration_ms >= 阈值` 时 `timeout_marked=true` 且有节点名。**FAIL**：无任何超时标记/阈值/节点。
- **本次预判**：**FAIL**。`duration_ms=120272`（≈embedded_runtime 120s 超时），`step="other"`，
  生产者 `PY` 中 `timeout` 仅出现在状态白名单注释（`PY:33-34`）而非字段，`turns[]` 无超时键。
- **严重度**：`P0`（本样本的失败直接表现为 120s 超时）。**影响**：因此无法回答"超时发生在哪个节点、阈值多少、是模型侧还是工具侧"。
- **归属**：T3 主判，T2 记录无 schema 位。

### M5 · 父子 / 层级关系缺失

- **判定**：tool_call 应能挂到其所属 step/node/span，至少能回答"这 3 次调用属于哪个节点"。
  ```bash
  jq -r '[.. | objects | select(has("parent_span_id") or has("parent_id") or has("step_id") or has("node"))] | length' /tmp/t.json
  ```
- **PASS**：`>=1`（有可见层级）。**FAIL**：`0`（扁平）。
- **本次预判**：**FAIL**（计数 0；`tool_calls` 仅扁平挂在 `turns[0]`，无节点归属）。
- **严重度**：`P1`。**影响**：因此无法回答"这 3 次工具调用属于 `embedded_runtime` 节点还是 `format` 节点"。
- **归属**：T3。

### M6 · 输入上下文缺失（turn 文本 + 入参截断无标记）

- **判定**：判断"输入是否合法/完整"需要原始输入；R19/R20 承诺的 `input_text/output_text` 必须存在，且截断必须留标记。
  ```bash
  jq -r '.payload.turns[0] | {has_input_text:has("input_text"), has_output_text:has("output_text")}' /tmp/t.json
  jq -r '[.payload.turns[].tool_calls[].arguments_summary | length]' /tmp/t.json    # 1000 = 触顶截断
  grep -n "1000\|INPUT_TEXT_MAX_CHARS\|OUTPUT_TEXT_MAX_CHARS" \
    /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py
  ```
- **PASS**：`input_text` 存在（或明确"本 run 无文本"）且截断有标记位。**FAIL**：两键皆缺，且截断静默。
- **本次预判**：**FAIL**。`has_input_text=false`、`has_output_text=false`（但 `CT:754-769` 的 R19/R20 要求其存在）；
  `arguments_summary` 上限 1000 字符（`PY:282`）**无截断标记得知**（本次 517/513/334 未触顶，但机制仍在）。
- **严重度**：`P1`。**影响**：因此无法回答"模型拿到的 JD 文本是否完整、空 JSON 是否因为输入被截断"。
- **归属**：T3（`EX:294-313` 本 run 未收集文本）+ T2（无截断标记/字节数）。

### M7 · 请求 / 事故关联键缺失

- **判定**：trace 应能与同一事故的其它信号（events 表的 sse-error、issue、request_id、用户）互查。
  ```bash
  jq -r '[.. | objects | select(has("request_id") or has("event_id") or has("issue_id") or has("user_id"))] | length' /tmp/t.json
  $PSQL -c "\d traces" | grep -Ei "request|issue|event|user" || echo "NO_LINK_COLUMN"
  ```
- **PASS**：`>=1` 关联键（payload 或列）。**FAIL**：0 且表无关联列。
- **本次预判**：**FAIL**（payload 计数 0；`DB:2-18` 无任何关联列）。前端 `sse-event-error` 事件在 `events` 表侧，
  与 trace 无 join 键。
- **严重度**：`P2`。**影响**：因此无法回答"这个 trace 对应哪条前端错误事件、哪个用户的哪次点击"。
- **归属**：T3 + T2（schema 位）。

### M8 · 生产者侧丢弃 / 采样不可上报（trace 缺失无痕）

- **判定**：采样丢弃、队列满丢最旧、投递失败必须留下可关联的计数或痕迹。
  ```bash
  grep -n "_DROPPED\|queue.maxlen\|_is_status_sampled\|logger.warning" \
    /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/exporter.py | head -20
  # 这些计数是否随 payload 外发（期望 PASS 时 payload 含 dropped/sampled 字段）
  jq -r 'has("dropped") or (.payload|has("dropped") or has("sampled"))' /tmp/t.json
  ```
- **PASS**：丢弃/采样在 payload 或服务端有可统计痕迹（计数入指标或字段）。**FAIL**：只有进程内日志。
- **本次预判**：**FAIL**（`EX` 的 `_DROPPED` 仅进程内变量 + `logger.warning`；payload 无 `dropped/sampled` 键）。
- **严重度**：`P1`。**影响**：因此无法回答"某次 run 到底上报过没有、是被采样丢的还是队列丢的"——事故期间若 trace 缺失将无从判断。
- **归属**：T3 主判，T2 记录服务端无对应接收侧计数（见 I7）。

---

## 3. 优化（Improve）判据

> 定义：数据能存，但不可检索 / 不可比较 / 不可告警 / 易被静默改写。判 FAIL 时必须给出**最小改动方向**（不实施）。

### I1 · 不能按 `run_id` / `turn_id` / 时间窗检索（P0）

- **判定**：事故复盘的第一动作是"按 run_id 找到这条 trace"。
  ```bash
  ALL=$($PSQL -c "select count(*) from traces;")
  N=$(curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?run_id=$RID&limit=100" | jq length)
  echo "all=$ALL returned=$N"
  grep -n "pub struct TraceQuery" -A10 /home/dm/pony-sentry/crates/server/src/routes.rs
  grep -n "idx_traces_" /home/dm/pony-sentry/migrations/postgres/0003_add_traces.sql
  ```
- **PASS**：`run_id` 过滤生效（`returned < all` 且命中 1 行）或列表含 `turn_id`/时间窗参数。**FAIL**：未知参数被静默忽略，返回全量。
- **本次预判**：**FAIL**。`returned == all`（实测 43 == 43）；`R:372-380` 无 `run_id/turn_id/from/to`；
  `DB:23-27` 无 `run_id`/`turn_id` 索引。
- **严重度**：`P0`。**影响**：因此无法回答"给我看 `run_83c1…` 这条 trace"——只能按 session_id 或翻全库。
- **归属**：T2。**最小改动**：`TraceQuery` 增 `run_id/turn_id/from/to` + `DB` 加索引；契约影响：纯增量，不破坏既有响应。

### I2 · 未知顶层键被静默丢弃（P1）

- **判定**：新增字段（如 `schema_version`）必须被保留或显式 422，不得"发出去 201、查回来没有"。
  ```bash
  curl -s -o /tmp/p1.json -w '%{http_code}\n' -X POST "$BASE/api/v1/traces" \
    -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
    -d '{"session_id":"audit-probe-contract-i2","schema_version":"v9","future_key":{"a":1},
         "turns":[{"turn_id":"t","unknown_nested":1}]}'
  jq -r '.payload | {has_schema_version:has("schema_version"), has_future:has("future_key"),
                     nested_kept:.turns[0].unknown_nested}' /tmp/p1.json
  $PSQL -c "delete from traces where session_id='audit-probe-contract-i2';"
  ```
- **PASS**：`has_schema_version=true`（保留）或返回 422（拒绝）。**FAIL**：201 且键丢失。
- **本次预判**：**FAIL**（实测 201、两键皆丢、nested 键保留 → 顶层丢失而嵌套保留，行为不自洽；`R:1196` 结构体重序列化）。
- **严重度**：`P1`。**影响**：因此无法回答"生产者发的新字段为什么在库里看不到"——协议演进会静默失效。
- **归属**：T2。**最小改动**：`AgentTraceIngestRequest` 存原始 `Value` + 显式 `deny_unknown_fields` 或白名单告警。

### I3 · `turns` / `tool_calls` 无 schema 校验（P0）

- **判定**：空对象、缺 `tool_name`、`status` 非法字面量都不应静默入库。
  ```bash
  curl -s -o /tmp/p3.json -w '%{http_code}\n' -X POST "$BASE/api/v1/traces" \
    -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
    -d '{"session_id":"audit-probe-contract-i3","turns":[{"tool_calls":[{}]}]}'
  jq -c '.payload.turns' /tmp/p3.json
  $PSQL -c "delete from traces where session_id='audit-probe-contract-i3';"
  sed -n '360,365p' /home/dm/pony-sentry/crates/server/src/routes.rs   # turns: Option<Vec<serde_json::Value>>
  ```
- **PASS**：非法元素 422 或落库时被规范化（补 `tool_name/status` 缺省）。**FAIL**：201 且原样入库。
- **本次预判**：**FAIL**（`turns:[{"tool_calls":[{}]}]` → 201，库内保存空工具对象；`turns:[]` 同样 201，见 P4/P11）。
- **严重度**：`P0`。**影响**：因此无法回答"库里的 trace 是否可信"——脏数据与正常数据无区分，UI 只能靠 `v-if` 兜底渲染（`UI:1379`）。
- **归属**：T2。**最小改动**：`turns` 改强类型结构体（`Turn`/`ToolCall`）+ 未知键 `warn`；契约影响：**破坏性**（需与生产者同步版本）。

### I4 · 合并策略「旧轮胜出」吞掉迟到补全（P0）

- **判定**：同 `turn_id` 的再次上报若带来新信息（如 error 详情），必须合并而非丢弃。
  ```bash
  S=audit-probe-contract-i4
  curl -s -o /dev/null -X POST "$BASE/api/v1/traces" -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
    -d "{\"session_id\":\"$S\",\"turns\":[{\"turn_id\":\"t1\",\"status\":\"success\"}]}"
  curl -s -o /dev/null -X POST "$BASE/api/v1/traces" -H "x-client-token: $CLIENT" -H 'content-type: application/json' \
    -d "{\"session_id\":\"$S\",\"turns\":[{\"turn_id\":\"t1\",\"status\":\"failed\",
         \"tool_calls\":[{\"tool_name\":\"format_jd_to_json\",\"status\":\"error\",\"error\":\"ValueError: x\"}]}]}"
  curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?session_id=$S" | jq -c '.[0].payload.turns'
  $PSQL -c "delete from traces where session_id='$S';"
  ```
- **PASS**：输出含 `status:"failed"` 与 `error:"ValueError: x"`。**FAIL**：仍是旧轮 `status:"success"` 无 error。
- **本次预判**：**FAIL**（实测输出 `[{"status":"success","tool_calls":[...],"turn_id":"t1"}]`，新信息全丢；`T:69-83`）。
- **严重度**：`P0`。**影响**：因此无法回答"错误详情是后来补报的、还是从一开始就没有"——补报通道存在但语义上是黑洞。
- **归属**：T2。**最小改动**：`merge_turns` 改字段级 merge（新轮非 None 覆盖旧轮）；契约影响：**语义变更**（需 ADR）。

### I5 · 控制台不展示 `turn.status` / `attempt_index` / `degraded` / `run_id`

- **判定**：诊断所需字段必须可见（本次失败 turn 的 `status=failed` 在 UI 上不可见）。
  ```bash
  cd /home/dm/pony-sentry && grep -n "turn\.status\|turn\.attempt_index\|turn\.degraded\|selectedTrace\.run_id\|selectedTrace\.turn_id" web/src/App.vue || echo "NO_HITS"
  ```
- **PASS**：`>=1` 命中。**FAIL**：`NO_HITS`。
- **本次预判**：**FAIL**（`NO_HITS`；`UI:769` 只渲染 `model/step/duration`，`UI:875` error 仅非空才显示 →
  第 3 条失败调用在 UI 上看不到任何失败原因）。
- **严重度**：`P1`。**影响**：因此无法回答"在控制台上如何一眼看出这轮是 failed"——必须展开 JSON 手读。
- **归属**：T2。**最小改动**：turn 行加 `status` 徽章 + 抽屉头加 `run_id/turn_id`；契约影响：无（纯前端）。

### I6 · `stats_incomplete` 徽章空转（UI 有渲染、无数据源）

- **判定**：UI 读取的字段必须有生产者。
  ```bash
  cd /home/dm/pony-sentry && grep -rn "stats_incomplete" web/src crates | sed -n '1,10p'
  ```
- **PASS**：`>=2` 处命中（UI + 后端/生产者）。**FAIL**：仅 1 处（UI）。
- **本次预判**：**FAIL**（仅 `UI:702`）。
- **严重度**：`P2`。**影响**：因此无法回答"token 统计缺失时用户是否被提示"——徽章永不出现。
- **归属**：T2。

### I7 · trace 侧零指标 / 零告警

- **判定**：写入量、合并次数、422 拒绝、payload 体量必须可观测。
  ```bash
  curl -s "$BASE/metrics" | grep -c "pony_sentry_trace" ; echo "(期望 >=1)"
  curl -s "$BASE/metrics" | grep "pony_sentry_" | awk -F'{' '{print $1}' | sort -u
  ```
- **PASS**：存在 `pony_sentry_trace*` 指标族。**FAIL**：`0`。
- **本次预判**：**FAIL**（`grep -c` → 0；`/metrics` 只有 issues/events/ingest_requests/level_degraded，`R:746-778`）。
- **严重度**：`P1`。**影响**：因此无法回答"采集是不是在丢、丢了多少"——只能靠人肉比对 job_copilot 日志与库。
- **归属**：T2。**最小改动**：`pony_sentry_trace_ingest_total{result}` + `payload_bytes` 直方图；契约影响：无。

### I8 · trace 与 issue/event 无联动（不可一键转缺陷）

- **判定**：trace 应可作为缺陷证据源被引用。
  ```bash
  cd /home/dm/pony-sentry && grep -rn "source_kind" crates/core/src/models.rs crates/server/src/routes.rs | head -5
  $PSQL -c "\d traces" | grep -Ei "issue|event" || echo "NO_LINK"
  ```
- **PASS**：存在 `trace→issue` 关联（列或 API）。**FAIL**：无。
- **本次预判**：**FAIL**（`NO_LINK`；`M:99` 的 `source_kind` 属 events 通道，trace 未纳入）。
- **严重度**：`P2`。**影响**：因此无法回答"这条 trace 是否已被当作缺陷跟踪"。
- **归属**：T2。

### I9 · 体量与截断不可见

- **判定**：payload 体积/截断状态应可查（2MB 硬限 `R:422`，生产者多处截断）。
  ```bash
  $PSQL -c "select pg_column_size(payload) as bytes, length(payload::text) as text_len
              from traces where id='$TID';"
  $PSQL -c "select column_name from information_schema.columns
             where table_name='traces' and column_name like '%bytes%';"   # 期望非空
  grep -n "\[:1000\]\|\[:5000\]\|\[:500\]" /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py
  ```
- **PASS**：存在体量列或截断标记。**FAIL**：两者皆无（只能现场算）。
- **本次预判**：**FAIL**。目标行体积均需现场计算，且**口径本身不一致**：实测 `pg_column_size(payload)=2641`、
  `octet_length(payload::text)=5821`（差 2.2×，TOAST 压缩态可变），基线 `payload_bytes=3405`（`01-db-truth.txt:5`）
  与二者皆不等 → 任何"payload 体积"结论必须注明所用表达式。`PY:282/286/296` 三处静默截断，无标记位。
- **严重度**：`P2`。**影响**：因此无法回答"这条 trace 有没有被截断过"。
- **归属**：T2（服务端）+ T3（生产者）。

### I10 · 无失败维度检索（status 只能从 JSONB 里挖）

- **判定**：应能直接查"所有 failed 的 turn"或"含 error 工具的 trace"。
  ```bash
  curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?project=job_copilot&limit=5" | jq length   # 现有维度
  $PSQL -c "select count(*) from traces where payload::text like '%\"status\": \"error\"%';"          # 全表扫
  $PSQL -c "select indexname, indexdef from pg_indexes where tablename='traces';" | grep -i gin || echo "NO_GIN_INDEX"
  ```
- **PASS**：存在 status 维度过滤（列/生成列/GIN 索引）。**FAIL**：只能全表 `like` 扫。
- **本次预判**：**FAIL**（`NO_GIN_INDEX`；`M:188-197` 无 status 过滤维度）。
- **严重度**：`P2`。**影响**：因此无法回答"最近一周有多少 run 的某个工具失败"。
- **归属**：T2。

### I11 · 类型漂移 = 整条 422 丢弃（无降级/隔离/重投）

- **判定**：单个字段类型漂移不应导致整条 trace 永久丢失（对比：events 通道有降级，`V:51-60`）。
  ```bash
  for body in '{"session_id":"audit-probe-contract-i11a","total_duration_ms":"120272"}' \
              '{"session_id":"audit-probe-contract-i11b","eval_status":"bogus"}' \
              '{"session_id":"audit-probe-contract-i11c","tags":{"app":1}}'; do
    curl -s -o /dev/null -w "%{http_code} " -X POST "$BASE/api/v1/traces" \
      -H "x-client-token: $CLIENT" -H 'content-type: application/json' -d "$body"
  done; echo
  $PSQL -c "delete from traces where session_id like 'audit-probe-contract-i11%';"
  sed -n '51,60p' /home/dm/pony-sentry/crates/ingest/src/validate.rs    # events 侧的降级范式
  ```
- **PASS**：返回 2xx 且落库（值经归一/降级），或 422 同时写入拒绝计数与死信。**FAIL**：422 且无任何痕迹。
- **本次预判**：**FAIL**（三例均 422：`invalid type: string`、`unknown variant`、`invalid type: integer`；无拒绝计数、无死信，
  证据仅存在于响应体，客户端 `CL` 只判 201/200 会当失败重试）。
- **严重度**：`P1`。**影响**：因此无法回答"事故当时的 trace 是被采集到还是被服务端拒了"——一次字段漂移就永久丢证据。
- **归属**：T2。**最小改动**：trace ingest 走与 events 一致的"可截断则截断、不可则降级 + 计数"范式；契约影响：**行为变更**（需 ADR）。

---

## 4. 防假阳性：不得判为缺陷的既有设计（简报 §8 落地）

| 现象 | 结论 | 机器口径 |
| :-- | :-- | :-- |
| 字段被 `[REDACTED_SECRET]` / `[USER_HOME]` 替换 | **设计行为**（`S:116-175`），不判"丢失" | 本样本无替换：payload 与 raw 逐键相等（`01` §0.3） |
| `session_id=thread_ref_v1_*` 不可逆 | **设计行为**（`PY:219` / `CD` 规则 1）；可评"可追溯性代价"，不判错 | `grep -n "public_trace_thread_id" PY` |
| `release=dev` / `environment=development` | Tilt dev 正常值 | `01-db-truth.txt:5` |
| trace 90 天保留 | **已实现**（`crates/server/src/main.rs:46`） | `grep -n RETENTION_DAYS_TRACES crates/server/src/main.rs` |
| `tags` 只有 3 键（无 `schema_version`） | R11 明文不产出（`CD:150-151`） | `CT:283-296` |
| `extra` 无 `error_message` | R1 明文不外发（`CD:114-116`） | `CT:298-311` |
| `turns[].status` 出现 `other` | R6 白名单收敛**是**契约（`CD:132-136`）；但收敛后丢失可诊断性是 **W8**（记录"代价"，不是"违规"） | `CT:385-396` |

---

## 5. 本次样本预判汇总（供 T2/T3 对齐起点，非最终裁决）

`W2/W3/W4/W5` 的"机制级 FAIL"需写探针复验；其余为对目标行的直接判定。

| ID | 预判 | 严重度 | 关键证据（可直接重放） |
| :-- | :-- | :-- | :-- |
| W1 | FAIL | P0 | `jq` 命中 2 条 success+失败文案 |
| W2 | FAIL | P1 | 三值相等但 tool_sum=98602≠120272；`attempt_index=1` vs 3 次调用；P5 改写历史值 |
| W3 | FAIL（机制） | P1 | `P:665-677` + 探针 P5（列 `run_id=run_second`、`turn_id=NULL`） |
| W4 | FAIL（机制） | P1 | `R:1202-1211` + 探针 P10（`payload.session_id=null`） |
| W5 | FAIL | P2 | 目标行 payload `null` vs 列 `unreviewed` |
| W6 | FAIL | P1 | payload 无绝对时间键（计数 0） |
| W7 | FAIL | P0 | 纯函数复现 `timeout → error` |
| W8 | FAIL | P1 | 第 3 条调用无 `output` 键、`error=null`、`step="other"` |
| M1 | FAIL | P0 | 3×`error=null`；错误类键计数 0 |
| M2 | FAIL | P0 | span 类对象计数 0；仅 turn 级 token |
| M3 | FAIL | P1 | 同名工具 ×3、`retry_*` 计数 0 |
| M4 | FAIL | P0 | `duration_ms=120272`、无 timeout 键、`step=other` |
| M5 | FAIL | P1 | 层级键计数 0 |
| M6 | FAIL | P1 | `input_text/output_text` 缺失（违反 R19/R20） |
| M7 | FAIL | P2 | 关联键计数 0；表无关联列 |
| M8 | FAIL | P1 | `_DROPPED` 仅进程内；payload 无 dropped/sampled |
| I1 | FAIL | P0 | `?run_id=` 返回全量（43==43） |
| I2 | FAIL | P1 | 探针 P1：201 且 `schema_version` 丢失 |
| I3 | FAIL | P0 | 探针 P11：`tool_calls:[{}]` → 201 |
| I4 | FAIL | P0 | 探针 P6：新 error 被旧轮吞 |
| I5 | FAIL | P1 | `grep` NO_HITS |
| I6 | FAIL | P2 | `stats_incomplete` 仅 1 处 |
| I7 | FAIL | P1 | `/metrics` 无 trace 指标（0） |
| I8 | FAIL | P2 | NO_LINK |
| I9 | FAIL | P2 | 无 bytes 列；3 处静默截断 |
| I10 | FAIL | P2 | NO_GIN_INDEX |
| I11 | FAIL | P1 | 三例均 422 且无计数/死信 |

---

## 6. 判据 → 报告骨架映射（简报 §7）

| 报告章节 | 使用的判据 |
| :-- | :-- |
| 3 逐字段对照矩阵 | `01-field-matrix.md` 全部 39 字段行 + INV-1..INV-10 |
| 4 错（Wrong）清单 | W1–W8 |
| 5 漏（Missing）清单 | M1–M8 |
| 6 优化（Improve）清单 | I1–I11 |
| 7 路线图（P0/P1/P2） | P0 = W1,W7,M1,M2,M4,I1,I3,I4 是**finding 落盘前的预判**；最终分级（P0 仅 M-01）与逐条重排依据见报告 §9.2 |
| 8 跨项目契约衔接 | W7,W8,M1,M3,M6,M8,I2,I4,I11（生产者侧最小改动） |
| 10 附录可重放收据 | `03-probe-receipts.md` |

---

## 7. 未机器化项（必须显式声明）

- `[NON-MACHINE]` **"最小改动的成本估算"**：本文件只给方向；工时由 Lead 汇总时定，靠 review。
- `[NON-MACHINE]` **"建议是否可接受"**（是否值得破坏契约换诊断力）：架构取舍，靠 review + ADR。
- `[UNVERIFIED]` **W3/W4/I4 的机制级结论**：本次目标行是单次上报，机制 FAIL 由探针演示；对生产存量数据的
  影响面需 T2 用全表口径命令（已给）在真实库上确认。
- `[UNVERIFIED]` **M8 丢弃率**：生产者侧无持久化计数，无法量化，只能定性判"不可观测"。
