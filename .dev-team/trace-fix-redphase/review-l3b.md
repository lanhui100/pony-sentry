# L3-B 对抗审查（安全/协议/运维）— trace 修复 v1.3

**Verdict: FAIL** · 11 findings（1 BLOCKER / 4 MAJOR / 6 MINOR），详见 `review-l3b.json`

| # | 审查项 | 结论 |
|:--|:--|:--|
| 1 | 4xx/5xx 回显 DB 原文 | **FAIL** F1：issue 路径 `e.to_string()` → 500 `Database error: ... database is locked`（实测） |
| 2 | 参数解析/注入面 | **PASS**（全 bind；limit/offset 解析后为 i64 再 format；`limit=abc`/`from=2026-10-23`→400，重复参数→400；`+5`/空格宽松但 clamp 内无绕过） |
| 3 | 指标基数/转义 | **FAIL** F2：project/turn_status 客户端无界（2×100KB → /metrics 201KB）；转义本身**正确**（`\`、`"`、`\n` 均转义，实测恶意 project 未破行） |
| 4 | 写入路径 | **PASS**：脱敏(routes.rs:1392) → 标注(repo:870/845)，diagnostics 只写固定 `missing`/`true`，无原始 error 文本。F7 为 turn_auto 前缀可伪造（MINOR，不增越权） |
| 5 | 控制台 XSS/徽标 | **PASS**（全仓 0 处 v-html/innerHTML；unknown→灰，未误判绿）。F8 超长 payload stringify、F9 timed_out 落灰 |
| 6 | 迁移运维 | **FAIL** F3（回填 turn_id 口径 ≠ 运行时 sha256）、F4（ACCESS EXCLUSIVE + 非 CONCURRENTLY + 单事务全表 UPDATE）、F5（无 down） |

## 阻断项（必须先修）
- **F3（BLOCKER）**：`migrations/0005_trace_turns.sql:42` 用 `hex(json)`、PG 用 `md5`，运行时用 sha256 → 实测 legacy 1 turn 重报后 `turns_count` 1→2，token 汇总翻倍。测试只覆盖显式 turn_id（`crates/core/tests/trace_v2_repository_test.rs:774`），C9.10 假绿。
- **F1（MAJOR）**：`crates/server/src/routes.rs:1237/1249/1256/1292/1325/1334/1348` 直出 DB 错误（C5 违例）。trace 端点同一时刻正确返回 `Internal error`，属漏改而非设计。
- **F2（MAJOR）**：`routes.rs:1456` + `:89` 无界 label、无淘汰；`/metrics` 亦无鉴权（F11）。
- **F4/F5（MAJOR）**：生产库升级读也被锁（ADD COLUMN ACCESS EXCLUSIVE 持到语句尾）；`migrate()` 每次启动重放回填扫描；无回滚脚本且未记录「不支持回滚」。

## 物理收据（临时库 /tmp，未改仓库代码）
1. `GET /api/v1/issues`（DB 被 EXCLUSIVE 锁）→ 500 `Database error: error returned from database: (code: 5) database is locked`。
2. 2×100KB project 上报 → `/metrics` 201881 B / 5 条 traces_total series；`turn_status="WEIRD-status-42"` 落 label。
3. legacy 库升级：`turn_auto_7b22737461747573` → 重报后新增 `turn_auto_16883215494afe36`，`turns_count` 1→2。
4. `PATCH :id` 坏 body → 422 text/plain 回显 serde 与全部枚举值（F6）。
5. 无凭证 `curl /metrics` → 200 含 project 标签（F11）。

Pass 明细：SQL 拼接面 0（`limit/offset` 仅 i64 格式化，其余全 bind）；RFC3339 严格（无 offset 即 400）；诊断不落原始 error 文本；控制台无 XSS 汇点。
