# L3-B 独立对抗审查（安全 / 隐私 / 运维容量）

- 结论：**FAIL**（BLOCKER 3 / MAJOR 8 / MINOR 3，共 14 条；F10–F14 为安全/容量补充）
- 对象：[report-draft.md](.dev-team/audit-trace-cards/50-report/report-draft.md)（草案 v1）
- 证据：只读代码 + 3 个 `audit-probe-l3b-*` 临时行实验（已精确删除，见 `l3b-receipts.txt`）
- 范围：隐私/脱敏、容量与保留、告警、合规边界；不评估诊断覆盖率算法本身

## 阻断项（必须先改报告）

1. **F1 隐私边界缺失**（line 255）：`user_id` 只靠“dev/preprod”控制不成立——`environment` 客户端自报、消费者不剥离（`sanitizer.rs` 无 `user_id`）、开启 tailnet 信任后读侧是“整个 tailnet 免 token”（ADR 004:65-66）、90 天 eval 语料；`input_text` 被当诊断缺口，但 10k 字符 JD/简历正文会进同表（`payload.py:82-83,244-252`）。
2. **F2 错误正文红线**（line 178）：M-01 要求 `error` 含 `{type,message}`，而生产者 `sanitize_error_message` 对所有环境恒返回 `[REDACTED]`（`trace_safety.py:344-349`），报告 line 222 自己也说不要外发 error_message。二选一，不能同时写。

## 主要问题

3. **F3 2MB 误读为行级上限**（line 255）：实测同 session 两次 1.2MB turn 合并后 2,400,513B，GET 2.4MB、List 4.4MB；2MB 只是 POST body limit（`routes.rs:422`），读路径全量 payload + 不 clamp。
4. **F4 `extra` 不持久**（line 217）：第二次不带 `extra` 的上报会把 `user_id/request_id` 置 null（`trace_merge.rs:13` 的 `new.clone()`；实测 extra=null）。D8“零改动”不成立。
5. **F5 脱敏建议反向降级**（line 207）：只按 key 名脱敏会取消 `SECRET_PATTERNS` 对自由文本的扫描；消费者 `routes.rs:1200` 二次改写路径，生产者“保留仓库相对路径”无效；顶层 `_redactions` 被 struct 丢弃；逐 trace 计数还应改聚合指标（避免把 secret 命中数暴露给 tailnet/eval 语料）。
6. **F6 告警风暴**（line 192）：trace 侧无指纹/issue/通知状态机；`turn.status` 在 W-01/W-02/W-06 修复前不可信，并发丢轮会让失败 trace 根本不入库。先 metrics + 待评审队列 + 日报，webhook 后置。
7. **F7 回链承诺不成立**（line 167）：`request_id` 被 HMAC alias 化（`trace_safety.py:507-508`），无法反 grep 日志；`thread_id` 若补齐会明文落库，破坏 thread_ref HMAC。D8 不应给“满分”。

## 次要

8. **F8 retention 口径**（line 206）：按 `created_at` 删除且 upsert 不更新，活跃线程第 90 天整行被删（`pg_repository.rs:665-677,815`）。
9. **F9 INV-2**（line 137）：W-04 把列语义写成“最近一次”等于把 INV-2 降级为注脚，未列入 §8 契约风险。

## 安全视角补充（G-01..G-08 缺口，F10–F14）

10. **F10 脱敏绕过未覆盖**（line 70）：§1.4 把脱敏判为“不计缺陷”，全文 0 次提 G-04；`tool_call.error` 只截断不 `redact_text`（`payload.py:296`），正则只容双引号且词表无裸 `token`，单引号 repr 与 `token=` 原样入库。
11. **F11 500 回显 DB 文案**（line 204）：`limit=-5` → 500 + `OFFSET must not be negative`（`routes.rs:1276-1278`），属信息泄露并污染 5xx 语义；I-04 只写“无 clamp”。
12. **F12 未吸收 G-01/G-02**（line 167）：幻影 id 让“按响应 id 回链”永久 404；响应 `total_*` 与库内不一致，生产者自检失真。
13. **F13/F14 容量预算缺项**（line 124/246）：`FOR UPDATE` 无 lock_timeout/连接池评估（20 并发同 session 串行重写 MB 级 jsonb）；`llm_calls≤50` 但 turns 无上限，体积预算只算 8 次调用。

## 实验与卫生

- E1（extra/redactions）：`audit-probe-l3b-extra`，POST1 201、POST2 201；首报 `user_id=510` 原样入库，二报后 `extra=null`；顶层 `_redactions` 被丢弃。
- E2（容量）：`audit-probe-l3b-cap`，两次 1.2MB POST 201；`octet_length=2,400,513`，GET 2,400,913B，List 4,414,470B。
- E3（thread_id 回退）：仅带 `thread_id` 时 201，库列 `session_id` 与 `payload.thread_id` 均为原始 thread_id，HMAC 被绕过；该行未命中前缀，已按精确值 `DELETE 1`。
- 清理：合计 `DELETE 3`；`audit-probe-l3b-%` 终态 0；`job_copilot` 37 行；事故行 `ca0b5494…` 完好。
- 未修改报告/业务代码；`thread_ref` HMAC 等式仍未复算（缺 key）。
