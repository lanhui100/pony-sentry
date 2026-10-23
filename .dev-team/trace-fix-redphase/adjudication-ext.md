# ext 波补修终审（v1.6）· 只读

**Verdict: FAIL** — 6 条 MAJOR **全部实质关闭**；1 条 MINOR 未实质关闭，按「无 OPEN」不通过。独立复跑：ext2 8/8 绿（新构建 sha256 `3126fcc0…`）、server lib 24 绿、core ext 9 绿。

| 项 | 判定 | 关键证据 |
|:--|:--|:--|
| L3A-E1 引号闭合 | **CLOSED** | sanitizer.rs:41-44 同类引号闭合 + 单测 :331 + 黑盒 v16_1 六写法 |
| L3A-E2 `_` 前缀键 | **CLOSED** | routes.rs:52-73/:1698-1716；v16_2 绿（`_sdk` 保留、伪造键覆盖、深层剥离） |
| L3A-E3 warnings 并集 | **CLOSED** | Lead 改动已核 pg:1420-1455 / sqlite:1242 纳入 payload 自带 `_warnings`；v16_3 绿（旧 1+新 0 均在、去重） |
| L3B-E1 去 head | **CLOSED** | payload.py:173-209 仅 chars/sha256_16；e2e 四项原文断言 False |
| L3B-E3 采样 | **CLOSED** | exporter.py:1007/1028 单一真源、0.0 不归一；矩阵 rate=0→(False,0.0)/(True,0.0) |
| L3B-E4 webhook | **CLOSED** | webhook.rs:63-95 双上限+淘汰最旧无 clear、Semaphore(8)、Client 复用、:117-129 只打 host；8 单测绿 |
| L3A-E4/E5/E6/E7/E8 | CLOSED | HELP 截断字段数；truncated 服务端判定；SQLite 列/索引分探；PG down；run_id 回填（v16_4 绿） |
| L3A-E9 / L3B-E5/E6/E8 | CLOSED(含注) | ext2 8 条补缺口；嵌套保留键已剥；summary 无 note；llm_calls 三处归一（v16_6/7 绿） |
| **L3B-E7 payload_bytes** | **PARTIAL（OPEN）** | 只改 HELP/docs，计数仍为合并后存储 payload |
## 唯一 OPEN（MINOR）：L3B-E7
`routes.rs:1819` 仍取 `to_vec(&saved.payload).len()`（合并后持久化行），HELP(:323)/docs:99 却称「上报体积（客户端内容序列化字节数，非存储体积）」——文案与实现相反（收据 §6.4 自认不改口径）。实测：二报仅上传 1228B，计数 +2585 且 == 存储行 payload 长度，容量看板随历史轮次线性高估。修法 1 行：改用合并前 `sanitized_payload`/请求体长度，或把 HELP+docs 改回「持久化 payload 体积」并在 v1.6-12 登记。

## 已登记残余（不阻断）
webhook 去重/并发进程内、permit 在 spawn 内（§6 登记，满足 ≤8 与禁 clear）；`warn!(error=%err)` 仍打完整错误文本（reqwest 请求类不含 URL，builder/redirect 类含）；未闭合引号 `password="ab` 不命中（非本轮回归）；PG 无法复跑（`PONY_TEST_PG_URL` 28P01，用例自 SKIP），Lead 的 pg 改动靠与 sqlite 同构+仓储单测+黑盒 HTTP 佐证。
