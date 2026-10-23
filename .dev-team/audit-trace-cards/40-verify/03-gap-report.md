# T4 复核报告：T2/T3 漏审点与口径不一致

> verifier（独立验证者）· 2026-10-23 · 锚点 `.dev-team/audit-trace-cards/00-baseline/00-brief.md`（Lead 冻结版 v1）
> 全部收据见同目录 [`02-independent-receipts.txt`](02-independent-receipts.txt)（A/B/B2/B3/B4/B5/C/C2/C3/D 章，均为「命令原文 + 真实输出 + exit code」）。
> 裁决文件：[`01-verdicts.json`](01-verdicts.json)，共 59 条（T2 18 / T3 41），`jq -e` 校验 Exit 0。

## 0. 裁决概览

| 项 | 数值 | 收据 |
| :-- | :-- | :-- |
| CONFIRMED | **59** | `02-independent-receipts.txt` §D：`jq -r 'group_by(.verdict)[]…'` → `CONFIRMED 59` |
| REFUTED | **0** | 同上（无 `REFUTED` 行） |
| UNVERIFIABLE | **0** | 同上（无 `UNVERIFIABLE` 行） |
| 重复 ID | 0 | `jq -r '[.[].id]|group_by(.)|map(select(length>1))|length'` → `0` |
| jq Schema 校验 | Exit **0** | `jq -e 'type=="array" and all(.[]; .verdict|test("^(CONFIRMED|REFUTED|UNVERIFIABLE)$"))'` → `true` |

**为什么 0 条 REFUTED**：T2/T3 的事实性结论逐条经我自跑 HTTP+DB（含并发竞态 W-02、old-wins W-01）或源码/生产者库直查复核，**没有一条被反例推翻**。
但复核中发现 **6 处引用/口径不精确** 与 **4 条双方都漏审的新缺陷**（下述 §A/§B）——这些不改变结论方向，属于「结论对、表述或覆盖面需修正」。

> 裁决口径声明：P 类（P1–P12/P10a/P10b，共 13 条）是**建议**而非可证伪命题；我对它们的 verdict **只覆盖技术前提**（字段能否加在 turns/extra、消费者是否需改 schema、词表/代码位置是否真实存在、是否触碰 Non-Goal），**成本（人日）与优先级属裁量，未验证**。

---

## A. T2/T3 都漏审的点（本次新增证据，建议并入 50-report）

### G-01 [新缺陷 · P1] `POST /api/v1/traces` 返回的 `id` 是**幻影 id**：同日二次上报后该 id 在库内不存在，`GET` 直接 404
- **证据**：`02-independent-receipts.txt` §B3-E3：
  ```
  POST2 响应 id  = c8dcee71-c3bd-4833-87b0-a7e12144b2bd
  库内真实 id    = 93139a38-8e4b-472a-b53c-f91692ca3fb5
  GET <响应 id>  → http=404 "Trace not found"；GET <库内 id> → 200
  ```
  根因：`pg_repository.rs:659-678` 的 `ON CONFLICT (session_id) DO UPDATE` **不更新 id**，而 `:698-701` 返回的是 `saved = trace`（携带本次新生成的 `Uuid::new_v4()`）。
- **为什么重要**：生产者若记录 POST 返回 id 作为深链/回执（job_copilot `client.py` 注释把 201 当创建成功），二次上报后该链接永久 404；这正是「跨项目回链」场景最容易踩的坑，而 **T2 只审了列级 run_id/turn_id 脱钩（W-04），未审 id 契约**。
- **建议归属**：消费者改（S）。修法二选一：`DO UPDATE ... RETURNING id` 回读真实 id；或冲突时把响应 `id` 替换为库内 id。

### G-02 [新缺陷 · P1] POST 响应体的 `total_*` 与库内落盘值**不一致**（同一请求两个真值）
- **证据**：§B3-E3（同 turn 二报）：响应 `resp_total_in=999/resp_total_dur=500`，库内 `100/120000`；§B3-E6（追加轮）：响应 `row_total_in=777777`，库内 `8`。
  根因：入库用 `effective_*`（`pg_repository.rs:636-657` 用 merge 后 payload 重算），返回却用未重算的 `trace.total_*`（`:698`）。
- **为什么重要**：T2 的 W-03「三套口径」成立，但只描述了「库内三套」；**响应体是第四套**。生产者按响应做自检会误判「服务端认可了我的 total」，而库内已改。属「错」而非「优化」，建议在报告中从 W-03 拆出独立条目。
- **建议归属**：消费者改（S，返回前用 effective_* 回填）。

### G-03 [新缺陷 · P2] 非法 `limit/offset` 返回 **HTTP 500 并回显原始数据库错误**
- **证据**：§B4-I-04：
  ```
  $ curl '…/api/v1/traces?limit=-5&offset=-3'   → http=500 bytes=73
  body = Database error: error returned from database: OFFSET must not be negative
  ```
  根因：`pg_repository.rs:753-755` 直接 `format!` 拼 `LIMIT {limit} OFFSET {offset}`，无 clamp 也无参数化。
- **为什么重要**：T2 I-04 只写「无 clamp」；实际是 **500 + 内部 DB 文案外泄**（信息泄露面 + 调用方无法区分「参数错」与「服务挂」）。另外此处 `format!` 拼接是 SQL 注入面（当前 SQLx 绑定只用于 WHERE 值，LIMIT 走字符串拼接；因类型是 i64 暂不可注入，但模式危险）。
- **建议归属**：消费者改（S：`clamp(1,100)` + `offset.max(0)`，并在 `TraceQuery` 用自定义 deserializer 拒负值→400）。

### G-04 [新缺陷 · P2] 脱敏管道在 **turn/tool_call 文本层存在绕过面**（T2 I-07 只谈「改写过度」，未谈「该挡没挡住」）
- **证据**：§A8b 五写法对照（同一请求内）：
  ```
  a_json_key      (JSON 对象键 password)        → [REDACTED_SECRET]  ✅
  b_double_quote  (字符串内 "password": "…")     → 值被替换          ✅
  c_single_quote  ('password': 'hunter2')       → hunter2 原样       ❌ 绕过
  d_bare_token    (token=abc123def456)          → 原样               ❌ 绕过
  e_auth_token    (auth_token=abc…)             → [REDACTED_SECRET]  ✅
  ```
  根因：`crates/ingest/src/sanitizer.rs:30-41` 的 `SECRET_PATTERNS` 只列 `api_key/access_token/refresh_token/auth_token/secret/password/…`，**不含裸 `token=`**；且正则的 `"?` 只容双引号，**Python repr 的单引号** `'password':` 直接穿透。
- **为什么重要**：本次事故的 `tool_calls[].arguments_summary`/`output` **正是 Python repr**（T3 S14 已证），即「最容易泄露的那类文本恰好是绕过的那一类」；而 `error` 字段（`payload.py:296`）连 `redact_text` 都不做。这与 T2 I-07「脱敏过度」是相反方向的风险，双方都未覆盖。
- **建议归属**：消费者改（S：正则支持单引号/JSON5 引号；裸 `token` 加入词表但保留 `*tokens` 白名单）＋生产者改（error 加 `redact_text`）。

### G-05 [口径修正] M-01 描述的第 3 条 tool_call 是「`output` 键缺失」，不是「`output:null`」
- **证据**：§A3 键集合 + `jq -c '.payload.turns[0].tool_calls[2]|{status,error,has_output:has("output")}'` → `{"status":"error","error":null,"has_output":false}`。
- **影响**：对渲染结果无差别（都为空），但契约表述必须准确——「键缺失」与「显式 null」在契约校验（必填 vs 可空）里是两件事。建议 T2 文中改写。

### G-06 [引用修正] T3 S5 引 `payload.py:188 保留 kwarg`，实际 `error_message` kwarg 在 `payload.py:177`
- **证据**：`grep -n "error_message" payload.py` → `177:    error_message: str | None = None`；`188` 是 `input_tokens`。
- **影响**：结论（R1 只禁 error_message、turn 无 error 通道）不受影响，属引用偏移。

### G-07 [引用修正] T2 W-01 把「冻结 old-wins 的测试」指向 `trace_repository_test.rs:248`
- **证据**：`249: async fn test_trace_upsert_on_same_session_id()`（这是幂等 upsert 测试）；真正冻结「重复轮次以旧为准」的是 `323: async fn test_trace_incremental_merge_upsert_dedup()`，其断言 `"turns 顺序必须为 [t1, t2, t3]"`、注释「t2 保持原样不重复」。
- **影响**：结论不受影响；但「哪条测试锁死了缺陷行为」关系到修 W-01 时要改哪些测试，建议改指向 323 行。

### G-08 [覆盖缺失] 双方都未检查 **`GET /api/v1/traces/:id` 与 `List` 返回体的一致性/可用性**
- **证据**：§B4-I-04 `limit=100000` 响应 2,016,030 bytes（41 行、含整 payload；我实测单行 **avg 49,359 bytes / max 324,739 bytes**）；`List` 与 `Get` 返回同一 `TraceRecord` 形状，但列表页在前端只渲染 `session_id/project/environment/release/Turns/Tokens/耗时`（App.vue:653-667），**列表 2MB 换来的是 6 个字段**。T2 I-04 已提「无投影」，但未量化「列表体积 vs 实际渲染字段」的浪费倍数（≈49KB/行 → 有效信息 ~100 字节）。

---

## B. 口径不一致清单（T2 / T3 / 基线 / 我的复算）

| # | 主题 | T2 | T3 | 基线 | 我的独立复算 | 判定 |
| :-- | :-- | :-- | :-- | :-- | :-- | :-- |
| B-01 | payload 体积 | 未引 | `01:4` 写 “payload 3405 bytes” | `01-db-truth.txt:5` 写 `payload_bytes=3405` | `octet_length(payload::text)=5821`、`char_length=3405`（§A1 之前的 A 段命令） | **3405 是字符数，不是字节数**；引用应写 5821 bytes / 3405 chars |
| B-02 | `reported_at-created_at` 下界 | W-05 未给范围 | S11：`-19min ~ +16h`（RCP-05） | — | 全库 37 行：`min=-0.164987s`、`max=+16:30:05.769373`；按 `total_input_tokens is not null` 过滤 24 行仍为 `-0.0956s` | **-19min 不可复现**（该值来自 T3 自己的探针行：reported_at_ms 固定为 1792754960002、created_at 为实测时刻；探针已删）。+16.5h 与「不可当服务端时间」成立 |
| B-03 | 并发丢轮比例 | 19/20 | — | — | 18/20（§B5，40 请求全 201） | 现象一致，**比例随时序波动**，报告应写「≥90% 会话丢轮」而非精确 19/20 |
| B-04 | List 未知参数实测行数 | 37 行（R6） | 37 行（RCP-07） | — | 41 行（§B4-I-03，测试时刻不同；终态 37 行） | 非冲突，**引用需带时间戳**，否则读者会对不上 |
| B-05 | `limit=100000` 响应体积 | 2,012,669 bytes | — | — | 2,016,030 bytes（41 行）；终态行数 37 时会更小 | 同上，属时点差异 |
| B-06 | `tool_call.error` 全库占比 | — | 274 条中 15 条非空（5.5%） | — | `274|15`（我复算完全一致） | 一致 ✅ |
| B-07 | attempt_index 语义 | M-02：「运行级，而生产方实际重试 3 次」 | S6/D5：「运行级尝试序号」 | — | `v2_agent_run_identity.attempt_index=1`；同 run 的 llm span 内 attempt_index 取 1/2 | 双方一致 ✅ |
| B-08 | 第 3 条 tool_call | M-01 写 `"output":null` | D4 只写 `error=null` | 03-raw.json 中该对象**无 output 键** | 键缺失（G-05） | T2 表述需修正 |
| B-09 | 「控制台展示能力」 | I-05「不展示 turn.status；call.error/arguments/output 都展示」 | P9/F5 同义 | — | 截图与 dist 统计一致（§C2/C3） | 一致 ✅（但摘要句易被误读成「控制台什么都看不到」，建议 T2 §0 第 5 行加限定） |
| B-10 | 报告行数基线 | T2 于 19:54 观测 37 行 | T3 于 19:57 观测 37 行 | Lead 基线 37 行 | 我 19:55 观测 41 行（T2/T3 探针行尚未清理时）；终态 37 行、`audit-probe%`=0 | **并发探针期间行数不可作为基线**；报告应统一用终态 37 行 |

---

## C. 我未验证 / 残余风险（显式声明，不当结论用）

1. **成本估算（人日）全部未验证**：T2/T3 的 `S/M/L` 与「0.5/1–2 人日」属裁量，我只核对了技术前提。
2. **T3 S9 的「失败 5 次至少另有 ~60k input 未计量」是 [EST]**：我确认了 4 条 error + 1 条 cancelled 的 `prompt_tokens` 为 NULL，但未对失败调用的 input 做重建估算。
3. **D1/D6 的 1.0/0.5 评分属口径裁量**：我只验证支撑事实，未复核评分合理性。
4. **未验证 SQLite 后端**：本机运行的是 Postgres 后端（`/usr/local/bin/pony-sentry-server` + `traces` 表）。T2 W-02 称 `sqlite_repository.rs:656-732` 同构，我做了代码对照但**未跑 sqlite 实验**。
5. **`/metrics` 未实抓**：`pony_sentry_traces` 指标「0 命中」来自源码 `grep`，未 curl `/metrics` 复核输出（因该结论有源码即够，但可补）。
6. **实验行清理的三方护栏**：终态 `traces=37`、`audit-probe%`=**0**、事故行 `md5=8abb872238435ca09fada802cda322db` 与实验前逐字一致、`thread_ref_v1_*`=24 行前后一致（§B3-clean / §B5-clean）。
7. **服务与源码同源**：运行中二进制 mtime `2026-10-23 11:20:41`，git HEAD `d6904b84`（10:53:11），`git status` 仅有 `.dev-team/` 与一条 notes 未跟踪 → 本次引用行号可对应源码。

---

## D. 给 Lead 汇总报告的建议（3 条）

1. **把 G-01/G-02 升级为独立「错」条目**（P1）：它们让「生产者按响应自检」这条最普通的工程实践失效，且与 W-03/W-04 不同源（一个是 id 契约、一个是响应 vs 落库）。
2. **把 G-04 作为「漏」条目补入安全维度**：本次样本的 tool 参数就是单引号 repr，属现实可达路径，不是理论风险。
3. **统一口径后再写摘要数字**：payload 体积用 `5821 bytes / 3405 chars` 双写；并发丢轮写「18–19/20」；行数一律标「终态 37 行」。

---

## E. 复核入口（可整体重放）

```bash
cd /home/dm/pony-sentry/.dev-team/audit-trace-cards/40-verify
bash run-independent-receipts.sh   > /tmp/a.txt   # A/B/B2/C：库行、payload 键全集、首轮上报实验、静态产物
bash run-independent-receipts-2.sh >> /tmp/a.txt  # B2：响应 vs 库内、total 口径
bash run-independent-receipts-3.sh >> /tmp/a.txt  # B3：Lead 纪律后的唯一前缀重跑（ACTIVE 证据）
bash run-verify-t2-findings.sh     >> /tmp/a.txt  # B4：T2 逐条对抗复现
jq -e 'type=="array" and all(.[]; .verdict|test("^(CONFIRMED|REFUTED|UNVERIFIABLE)$"))' 01-verdicts.json
# 实验清理：delete ... where session_id like 'audit-probe-verify-9691a9%'  → 仅自己前缀；终态 count=0
```
