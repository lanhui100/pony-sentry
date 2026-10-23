# Lead 定点复核收据（2026-10-23 19:52 CST）

实验行 session_id=`audit-probe-fields`，实验后已删除（剩余 audit-probe 行 = 0）。

## P1. 未知顶层字段被静默丢弃
请求含 `"totally_unknown_field":"SHOULD_BE_DROPPED"` → 响应 payload 顶层键集合中不存在该键。
判定：CONFIRMED（`crates/server/src/routes.rs:1196` `serde_json::to_value(&payload)` 重新序列化 typed struct；无 `#[serde(flatten)]` catch-all、无 `deny_unknown_fields`）。

## P2. turns/tool_calls 无 schema 校验
`turns[0].custom_turn_field=123` 原样入库（`turns: Option<Vec<Value>>`，见 routes.rs:362）。
判定：CONFIRMED —— 任意形状的轮次都会被接受，无字段名/类型/必填校验。

## P3. 同一 turn 二次上报终态被旧值吞掉
第 1 次 `{turn_id:t1,status:running}`；第 2 次 `{turn_id:t1,status:failed,error:{...}}` →
入库仍为 `status:"running"`，第 2 次的 `error` 丢失（`crates/core/src/trace_merge.rs:69-83` 旧轮优先 + turn_id 去重）。
判定：CONFIRMED —— 终态保真缺陷。

## P4. total_* 与 turns 口径不一致
第 2 次上报顶层 `total_input_tokens=99` 被写入（routes.rs:1243 直接取客户端值），
但 turns 内无 input_tokens，`total_input_tokens` 与 turns 累计值不等（99 vs 0）。
判定：CONFIRMED —— 同一行内两套口径。

## P5. 脱敏绕过（Lead 复核 verifier G-04）
请求 tool_calls[0].output = `{'password': 'hunter2'}, auth_token=ghp_abcd1234, token=abc123def`
→ 入库 = `{'password': 'hunter2'}, [REDACTED_SECRET], token=abc123def`
判定：CONFIRMED —— 单引号 Python repr 的 password 与裸 `token=` 均未遮蔽（`crates/ingest/src/sanitizer.rs` 的 SECRET_PATTERNS 只容双引号、仅认 `auth_token/api_key` 等具名键）。
实验行已删（probe 行 = 0）。

## P6. 非法分页参数 → HTTP 500 + 回显 DB 错误（Lead 复核 verifier G-03）
`GET /api/v1/traces?limit=-5&offset=-3` → `HTTP 500`，body=`Database error: error returned from database: OFFSET must not be negative`
判定：CONFIRMED。
