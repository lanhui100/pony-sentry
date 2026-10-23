# 探针收据集（可重放）

> T1 · protocol-scribe · 采集时刻 `2026-10-23T19:54:07+08:00`。
> 用途：为 `01-field-matrix.md` 与 `02-audit-criteria.md` 的每条"机制级"结论提供**可原样重放**的机器收据。
> 全部命令只读目标行；写探针仅用 `audit-probe-contract-<ts>-*` 前缀，结束**精确前缀删除**并留计数收据。
> 目标行 `ca0b5494-ae1b-47db-8e00-c9e7abbb6779` 全程未被修改（收据 §5）。

---

## 0. 前置变量（与 02 §0.1 相同）

```bash
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
PSQL="psql -h 10.43.206.170 -U job_copilot -d ponysentry -tA"
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
BASE=http://127.0.0.1:3000
TID=ca0b5494-ae1b-47db-8e00-c9e7abbb6779
TS=$(date +%Y%m%d%H%M%S); P="audit-probe-contract-$TS"
post(){ curl -s -w '\nHTTP=%{http_code}\n' -X POST "$BASE/api/v1/traces" \
        -H "x-client-token: $CLIENT" -H 'content-type: application/json' -d "$1"; }
```

一次性跑完整套（等价于本文全部 P1–P15）：

```bash
bash /home/dm/pony-sentry/.dev-team/audit-trace-cards/10-contract/probe-suite.sh
```

> 随卡附 `probe-suite.sh`（与本次执行脚本逐字一致）。**它会创建并删除探针行**，重放前确认 `CLIENT` 指向本机 dev 服务。

---

## 1. 运行时事实（P1–P15 实抓输出）

```
### ENV
run_at=2026-10-23T19:54:07+08:00 prefix=audit-probe-contract-20261023195407
pre_count_all=37
```

### P1 · 未知顶层键被静默丢弃，嵌套未知键保留（→ I2 / INV-8）

```bash
post "{\"session_id\":\"$P-p1\",\"schema_version\":\"v9\",\"future_key\":{\"a\":1},
      \"turns\":[{\"turn_id\":\"t1\",\"status\":\"failed\",\"error_type\":\"ValueError\",
                  \"tool_calls\":[{\"tool_name\":\"format_jd_to_json\",\"status\":\"error\"}]}],
      \"total_duration_ms\":1}"
```

```
HTTP=201
payload = {"environment":null,"eval_status":null,"extra":null,"project":"job_copilot",
 "release":null,"reported_at_ms":1792756123000,"run_id":null,"session_id":"...-p1","tags":null,
 "thread_id":null,"total_duration_ms":1,"total_input_tokens":null,"total_output_tokens":null,
 "turn_id":null,
 "turns":[{"error_type":"ValueError","status":"failed",
           "tool_calls":[{"status":"error","tool_name":"format_jd_to_json"}],"turn_id":"t1"}]}
```

**读法**：`schema_version` / `future_key` 既未落库也未 422；`turns[0].error_type` 等**嵌套**未知键反而完好保留
→ 顶层走结构体重序列化（`R:1196`），嵌套走 `serde_json::Value`（`R:362`），行为不自洽。

### P2 · `eval_status` 非法枚举 → 整条 422（→ I11 / W5）

```
Failed to deserialize the JSON body into the target type: eval_status: unknown variant `bogus`,
expected one of `unreviewed`, `triage_good`, `triage_bad`, `eval_dataset`, `optimized`, `wontfix`
HTTP=422
```

### P3 · `total_duration_ms` 传字符串 → 整条 422（→ I11）

```
Failed to deserialize the JSON body into the target type: total_duration_ms:
invalid type: string "120272", expected i64
HTTP=422
```

### P4 · `turns:null` / `turns:[]` 均 201（→ I3）

```
HTTP=201   payload.turns = null
HTTP=201   payload.turns = []        # 见 P11 第二条
```

### P5 · 同 session 二次上报：身份列被覆盖 + total 被改写（→ W3 / INV-2 / INV-3）

```bash
post '{"session_id":"'$P'-p5","run_id":"run_first","turn_id":"turn_p1",
       "turns":[{"turn_id":"turn_p1","status":"success"}],"total_duration_ms":1}' >/dev/null
post '{"session_id":"'$P'-p5","run_id":"run_second",
       "turns":[{"turn_id":"turn_p2","status":"succeeded","duration_ms":500}],"total_duration_ms":999}' >/dev/null
$PSQL -c "select run_id, coalesce(turn_id,'<NULL>'), total_duration_ms,
                 jsonb_array_length(payload->'turns') turns, payload->>'total_duration_ms' payload_total
            from traces where session_id='$P-p5';"
```

```
run_second|<NULL>|500|2|500
```

**读法**：`turn_id` 列被写成 NULL（第二次上报未带 turn_id），`run_id` 列只剩 `run_second`；
`total_duration_ms` 由首个上报的 `1` 被改写为 `500`；payload 内两轮 turn 均在 → **列与 payload 失配**。

### P6 · 迟到补全被旧轮吞掉（→ I4 / INV-9）

```bash
S6="$P-p6"
post '{"session_id":"'$S6'","turns":[{"turn_id":"t1","status":"success"}]}' >/dev/null
post '{"session_id":"'$S6'","turns":[{"turn_id":"t1","status":"failed",
      "tool_calls":[{"tool_name":"format_jd_to_json","status":"error",
                     "error":"ValueError: expected value at line 1 column 1"}]}]}' >/dev/null
curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?session_id=$S6" | jq -c '.[0].payload.turns'
```

```
[{"status":"success","turn_id":"t1"}]
```

**读法**：第二次上报的 `status=failed` 与 `error=ValueError…` **完全丢弃**（`T:69-83` 旧轮胜出）。
这正是"错误详情事后补报"通道失效的直接证据。

### P7 / P8 / P9 · 类型与必填的边界（→ I3 / I11 / W4）

```
P7 tags.app=1      → tags.app: invalid type: integer `1`, expected a string      HTTP=422
P8 turns={"a":1}   → turns: invalid type: map, expected a sequence              HTTP=422
P9 session_id="   "→ session_id cannot be empty                                  HTTP=400
```

### P10 · `thread_id` 兜底造成列与 payload 不一致（→ W4 / INV-4）

```bash
post '{"thread_id":"'$P'-p10","turns":[]}'
$PSQL -c "select session_id, coalesce(payload->>'session_id','<NULL>') p_sid,
                 coalesce(payload->>'thread_id','<NULL>') p_tid
            from traces where session_id='$P-p10';"
```

```
HTTP=201
audit-probe-contract-20261023195407-p10|<NULL>|audit-probe-contract-20261023195407-p10
```

### P11 · 空 tool_call 对象 / 空 turns 数组（→ I3）

```
POST {"turns":[{"turn_id":"t","tool_calls":[{}]}]}  → HTTP=201
     payload.turns = [{"tool_calls":[{}],"turn_id":"t"}]
POST {"turns":[]}                                  → HTTP=201
     payload.turns = []
```

### P12 · 检索参数 `run_id` 被静默忽略（→ I1）

```bash
ALL=$($PSQL -c "select count(*) from traces;")
N=$(curl -s -H "x-client-token: $CLIENT" "$BASE/api/v1/traces?run_id=run_83c1879e338d442f953a5dd4b8d1646a&limit=100" | jq length)
echo "all=$ALL returned_with_run_id_filter=$N"
```

```
all=44 returned_with_run_id_filter=44
```

**读法**：判定依据是 `returned == all`（**不是**绝对数）——`all` 含当时存在的他成员探针行，行数会随并发波动。

### P13 · `/metrics` 无 trace 指标（→ I7 / M8）

```bash
curl -s "$BASE/metrics" | grep -c pony_sentry_trace
```

```
0
```

### P14 · 目标行只读复核（→ W5 / INV-5 / I9）

```bash
$PSQL -c "select run_id, coalesce(turn_id,'<NULL>') turn_id, eval_status,
                 coalesce(payload->>'eval_status','<NULL>') p_eval,
                 total_duration_ms, total_input_tokens, total_output_tokens,
                 pg_column_size(payload), octet_length(payload::text)
            from traces where id='$TID';"
$PSQL -c "select count(*) filter (where coalesce(payload->>'eval_status','<missing>') <> eval_status) mismatch,
                 count(*) from traces;"
```

```
run_83c1879e338d442f953a5dd4b8d1646a|turn_5dd7639f928549dfb86d96cbe6cbf33a|unreviewed|<NULL>|120272|36612|1368|2641|5821
44|44
```

**读法**：
- payload 的 `eval_status` 与列 **44/44 全不等**（payload 恒 null）→ W5/INV-5；
- 体积口径：`pg_column_size(payload)=2641` 与 `octet_length(payload::text)=5821` 差 2.2×，基线
  `payload_bytes=3405`（`00-baseline/01-db-truth.txt:5`）与二者皆不等 → **引用体积必须注明表达式**（→ I9）；
- `reported_at`（`2026-10-23T11:29:20.002Z`）与 `updated_at` 未被探针改动。

### P15 · 清理收据（只删本前缀）

```
DELETE 7
after_mine=0
post_count_all=37
target_row_intact=1
```

---

## 2. 生产者侧纯函数收据（无 I/O、不写库）

R2 · `tool_calls[].status` 值域坍缩（→ W7）

```bash
cd /home/dm/job_copilot/api_v2 && .venv/bin/python -c "
from src.modules.agent.pony_sentry_trace.payload import build_trace_payload
from tests.acceptance.pony_sentry_trace.test_pony_sentry_trace_contract import TURN_KEYS
base=dict(thread_id='thread-abc12345',run_id='run-abc12345',trace_id='trace-abc12345',
          status='completed',environment='development',release='1.2.3',turn_id='turn-abc12345',
          assistant_id='agent',current_step='profiling',duration_ms=1234,degraded=False,
          attempt_index=1,reported_at_ms=1700000000000)
minimal=build_trace_payload(**base)['turns'][0]
rich=build_trace_payload(**base,model='deepseek-flash',input_tokens=100,output_tokens=250,
        cache_hit_tokens=1024,input_text='hi',output_text='yo',
        tool_calls=[{'tool_name':'format_jd_to_json','status':'timeout','error':'ValueError: x'}])['turns'][0]
print('TURN_KEYS    =', sorted(TURN_KEYS))
print('minimal==frozen', set(minimal)==TURN_KEYS)
print('rich==frozen   ', set(rich)==TURN_KEYS, '| rich-frozen:', sorted(set(rich)-TURN_KEYS))
print('timeout ->', rich['tool_calls'][0]['status'], '| error ->', rich['tool_calls'][0]['error'])
" 2>/dev/null | grep -v '^{"timestamp"'
```

```
TURN_KEYS    = ['attempt_index', 'degraded', 'duration_ms', 'run_id', 'status', 'step', 'turn_id']
minimal==frozen True
rich==frozen    False | rich-frozen: ['cache_hit_tokens', 'input_text', 'input_tokens', 'model', 'output_text', 'output_tokens', 'tool_calls']
timeout -> error | error -> ValueError: x
```

**读法**：
1. `status="timeout"` → `"error"`：失败类型在 trace 内不可区分（W7）；
2. `CT:50` 的 `TURN_KEYS`（"精确，禁止夹带"）只对**默认入参**成立；一旦带富字段，键集合即溢出 7 个键
   → 生产者对 `turns[]` 键集合**没有单一真源**（01 §2 的依据）。这也解释了为何 `CT` 内 R19/R21/R24 与规则 3
   可以同时"全绿"（`pytest -k "rule3_turns_element_fields_are_frozen or turn_details_rich_fields or cache_hit_tokens"`
   → `3 passed`），但两份断言描述的其实是两个不同的键集合。

R3 · 生产者的错误详情来源链（→ M1）

```bash
sed -n '272,300p' /home/dm/job_copilot/api_v2/src/modules/agent/pony_sentry_trace/payload.py
sed -n '106,116p' /home/dm/job_copilot/api_v2/src/job_copilot/middleware/tool_observability_middleware.py
```

```
PY:296            "error": str(tc.get("error") or "")[:500] if tc.get("error") else None,
middleware:115    error=payload.get("error_message"),
```

**读法**：`error` 只有在工具观测 payload 带 `error_message` 时才非空；本 run 三条皆为 None
→ 生产者**从未获得**错误详情（不是 pony-sentry 丢的），这是 M1 的归因边界。

---

## 3. 结论复算索引（判据 ID → 收据）

| 判据 | 收据 | 结论 |
| :-- | :-- | :-- |
| W1 | 01 §3 `output`/`status` 行 + `jq`（02 W1 命令） | FAIL：2 条 success + 失败文案 |
| W2 | P5 + P14 | FAIL：total 口径漂移、历史值被改写 |
| W3 | P5 | FAIL（机制）：身份列被覆盖 |
| W4 | P10 | FAIL（机制）：列 vs payload.session_id |
| W5 | P14（44/44 mismatch） | FAIL：双写不一致 |
| W6 | 01 §4 INV-10（`jq` 绝对时间键计数 0） | FAIL |
| W7 | R2 | FAIL：`timeout → error` |
| W8 | P1/P11 + 01 §3 `output` 行 | FAIL：缺键/空值/占位不可区分 |
| M1 | R3 | FAIL：error 恒 null，来源链上游即空 |
| M2/M3/M5 | 01 §2/§3（span/retry/层级键计数 0） | FAIL |
| M4 | 01 §2 `step` 行（`other`）+ `duration_ms=120272` | FAIL |
| M6 | 01 §2 `input_text/output_text` 行 | FAIL |
| M7 | 01 §4 INV-1 + `\d traces` | FAIL |
| M8 | P13 + `EX` 静态 | FAIL |
| I1 | P12 | FAIL：`?run_id=` 无效 |
| I2 | P1 | FAIL：201 且键丢失 |
| I3 | P4/P8/P11 | FAIL：任意形状入库 |
| I4 | P6 | FAIL：迟到补全被吞 |
| I5 | `grep turn.status UI` → NO_HITS | FAIL |
| I6 | `grep stats_incomplete` → 仅 UI:702 | FAIL |
| I7 | P13 | FAIL |
| I8 | `\d traces` 无关联列 | FAIL |
| I9 | P14 体积三口径打架 | FAIL |
| I10 | `pg_indexes` 无 GIN | FAIL |
| I11 | P2/P3/P7 | FAIL：整条 422 无痕迹 |

---

## 4. 并发与污染声明（必须随卡传递）

1. **他成员探针行共存**：执行期间 `select count(*) from traces` 在 37–44 之间波动（`pre=37`、`P12 时 all=44`、
   `post=37`）。因此**任何"全表行数"类断言都必须同时给出取数时刻**，且不得作为缺陷证据。
2. **疑似过宽清理**：本卡首轮探针（19:48:43 创建 3 行）在 19:49:44 前消失，本卡当时**未执行删除**；
   推测有成员用 `like 'audit-probe-%'` 做了跨成员前缀删除。`[UNVERIFIED]`（无法从库内取证），
   但结论不受影响——本文所有输出均为**当次 stdout 实抓**。
3. **本卡自证清白**：仅删 `audit-probe-contract-<ts>-%`，`DELETE 7`、余 0；目标行 `target_row_intact=1`。
4. **重放注意**：`P12` 的 `returned==all` 判定与行数无关（只比较两者相等），可安全重放；
   `P5/P6` 依赖 `session_id` 唯一性，重放时务必换新 `$TS`。

---

## 5. 未随本卡固化的事项

- `[NON-MACHINE]` 目标行**未被探针触碰**这一点，只能由"未执行任何 UPDATE/DELETE 指向该 id"的审查保证；
  库侧无审计日志可证（`traces` 无 trigger）。
- `[UNVERIFIED]` 基线 `payload_bytes=3405` 的取数表达式（Lead 侧脚本未随基线落盘）；
  本卡实测 2641 / 5821 两个口径，均与 3405 不等。
