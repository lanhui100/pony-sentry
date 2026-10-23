# L3-B 对抗审查（安全/隐私/运维）— amend v1.5 扩展面

**Verdict: FAIL** · 6 项重点：PASS 3 / FAIL 3，共 7 findings（3 MAJOR / 4 MINOR，无 BLOCKER）。详见 `review-ext-l3b.json`。

| # | 重点 | 结论 |
|:--|:--|:--|
| 1 | 隐私（input_summary / llm_calls / sampled） | **FAIL** E1：head 只过 6 类正则；实测简历姓名/院校/雇主/薪资/JD 逐字保留。llm_calls 无 prompt/response（PASS）。E3：sample_rate 口径自相矛盾 |
| 2 | webhook（M-03） | **FAIL** E4：去重只限条数不限字节；满额 `clear()` 可被 1024 轮转击穿；spawn 无闸、每任务新建 Client；`warn!(error)` 打印含密钥 URL（不打印 payload，该点 PASS） |
| 3 | `_redactions` / `_unmapped` | **PASS**：注入→归一→脱敏顺序正确，深度>32 fail-closed，顶层 `_` 键剥离。E5：计数口径窄、嵌套 `_redactions` 可伪造 |
| 4 | 查询面（summary / X-Total-Count / order_by） | **PASS**：order_by 枚举白名单→静态 SQL。E6：summary 仍带 note；count 可做 session oracle（均在 admin 鉴权后） |
| 5 | 指标三计数器 | **PASS**：label 归一 + 512 series 上限（F2 已修）。E7：payload_bytes 由客户端内容决定可灌水 |
| 6 | 生产者契约（50 条 / estimated / R1） | **PASS**：50 双处执行、R1 成立。E8：estimated 永无 false、唯一接线恒回推；服务端对 llm_calls 零校验 |

## 阻断/优先修（MAJOR）
1. **E3（先修，1 行级）**：`exporter.py:1013` 把 rate≤0 归一 1.0，而 `_is_status_sampled`（:1027）把 rate≤0 当全丢 → 实测 rate=0 时上报 `sample_rate=1.0`/`sampled=true`；R49 单测只测 payload 纯函数，未覆盖该接线（假绿）。
2. **E1**：`payload.py:201` head 就是前 200 字原文（仅掩手机/邮箱），R48 的「绝不外发原文」名不副实；建议改为字符直方/分词脱敏或去掉 head。
3. **E4**：`trace_webhook.rs:63-67` 满额清空 + key 无长度上限（project/tool_name 客户端可控，单 key 可达 2MB）+ `handle.spawn` 无并发闸 → 去重失效与内存/连接放大；建议 key 先 `normalize_metric_label` 化并限长、用 LRU 逐条淘汰替代 `clear()`、加信号量、日志脱 URL query。

## 物理收据（只读，未改任何代码）
1. `build_input_summary`（仓库 venv 直跑）→ head 保留「候选人张三/清华大学/字节跳动/期望薪资 45k/岗位JD…」，仅 `138****11`、`zha****om` 被打码。
2. `PONY_SENTRY_TRACE_SAMPLE_RATE=0`：`_effective_sample_rate()==1.0`，`_is_status_sampled('completed')==False`。
3. `trace_webhook.rs:63-67` 逻辑推演：1024 个不同 key → retain 无过期项 → `clear()` → 原 key 再次 `claim()` 通过（既有 `dedupe_table_is_bounded` 只断言 len≤1024）。
4. 全仓 grep：`crates/`、`web/src/` 对 `llm_calls|input_summary|sample_rate` **0 命中**（服务端无校验、控制台无渲染）。
5. App.vue 预设（:1301-1332）仅前端过滤 4 个终态（缺 aborted/interrupted/max_tokens），与 `CONTROLLED_TURN_STATUSES` 13 项不一致；无新增 XSS 汇点（无 v-html）。
