# 任务简报：pony-sentry trace 采集质量审核（一次真实事故样本）

> Lead 冻结版 v1（2026-10-23）。所有成员以本文件为唯一目标锚点；与本人 prompt 冲突时以本文件为准。

## 1. 背景（只读事实，不重开调查）

job_copilot 在 Tilt dev 环境的前端会话 `efd84b9f-1956-4660-be61-91cf1ab243d1`（cards 页面）
于 2026-10-23 19:27–19:29 CST 的一次 run 失败，并把该 run 的 trace 上报到 pony-sentry。

- pony-sentry 侧真值（Postgres，表 `traces`）：`id=ca0b5494-ae1b-47db-8e00-c9e7abbb6779`
  - `session_id=thread_ref_v1_e9b4ec395c96d8913c106ef8`，`run_id=run_83c1879e338d442f953a5dd4b8d1646a`，
    `turn_id=turn_5dd7639f928549dfb86d96cbe6cbf33a`，`project=job_copilot`，`environment=development`，`release=dev`
  - payload 3405 bytes，1 个 turn，status=failed，3 个 tool_call（2 success / 1 error），tool error 详情为空
- 事故真因（job_copilot 侧取证，仅作对照，不是本次要解决的问题）：模型结构化输出返回空/非法 JSON →
  `ValueError: expected value at line 1 column 1`；工具 `format_jd_to_json` 3 轮重试耗尽；
  `embedded_runtime` 120s 节点超时 → `TimeoutError`；前端 `sse-event-error`。
  证据：`/home/dm/job_copilot/.dev-team/evidence-cards-run/05-verdict.md`、
  `03-error-signature.txt`、`04-db-truth.txt`；服务日志 `/var/log/job-copilot-dev/api-v2-dev.log:187617,187692`。

## 2. 本次任务（唯一目标）

**以这份真实 trace 为样本，审核 pony-sentry 的 trace 采集「有没有错、有没有漏、哪里可优化」**，
产出可执行的评估结论与改进建议。

## 3. 明确非目标（Non-Goals）

- 不修复 job_copilot 的空 JSON/超时 bug，不改 job_copilot 业务代码；
- 不修改 pony-sentry 业务代码、不做实现（只出审核结论与建议）；
- 不重做事故根因取证（已有证据，引用即可）；
- 不引入新依赖、不改数据库 schema、不碰部署。

## 4. 审核维度（对每条结论都要给判定 + 证据）

1. **错（Wrong）**：trace 里记录的内容与事实不符，或语义自相矛盾（如 status=success 但 output 是失败文案；
   total 与 turns 口径不一致；时间字段口径混乱）。
2. **漏（Missing）**：为定位该类失败所必需、但 trace 中完全缺失的信息（错误类型/错误消息/堆栈、LLM 调用级
   span、重试与超时语义、父子关系、线程上下文、用户/请求关联、版本与环境、原始输入摘要等）。
3. **优化（Improve）**：数据可存储但不可诊断/不可检索/不可比较/不可告警的点（schema 无契约校验、
   未知字段被静默丢弃、合并语义让终态丢失、列表接口不能按 run_id/时间窗检索、控制台展示不足、保留期与体积等）。

每条结论必须带：
- `判定`（错/漏/优化）+ `严重度`（P0 阻断诊断 / P1 显著削弱 / P2 体验与效率）；
- `证据`：pony-sentry 代码 `path:line` 或机器收据（SQL / curl / 控制台截图）；
- `影响`：用本次事故具体说明「因此无法回答哪个问题」；
- `建议`：最小改动方向 + 预估成本 + 是否影响已有契约（不实施）。

## 5. 证据标准（零假测试底线）

- 所有事实必须可机器复现：给出可直接重放的命令（psql / curl / jq）与其真实输出；
- 严禁「看起来/应该是」式断言；无法机器验证的标 `[NON-MACHINE]` 并写明靠 review；
- 只读：不得修改 `/home/dm/pony-sentry` 与 `/home/dm/job_copilot` 的任何受版本控制文件；
  过程产物只写各自 write_scope 目录。

## 6. 交付物（各角色只写自己的目录）

| 目录 | 负责人 | 内容 |
| :--- | :--- | :--- |
| `.dev-team/audit-trace-cards/00-baseline/` | Lead（已冻结） | 基线收据 + 本简报 |
| `.dev-team/audit-trace-cards/10-contract/` | protocol-scribe | 字段契约矩阵（生产者承诺 / 消费者接受 / 实物实际）+ 审核判据 |
| `.dev-team/audit-trace-cards/20-core/` | auditor-core | pony-sentry 侧（ingest→校验→脱敏→合并→存储→API→控制台）错漏优化清单 |
| `.dev-team/audit-trace-cards/30-diagnostic/` | auditor-diagnostic | 诊断充分性 + 生产者语义 + 跨项目契约衔接清单 |
| `.dev-team/audit-trace-cards/40-verify/` | verifier | 独立复现收据 + 逐条 PASS/FAIL 裁决 |
| `.dev-team/audit-trace-cards/50-report/` | Lead | 汇总报告与 ADR |

## 7. 报告骨架（Lead 汇总时采用，各角色按此组织证据）

1. 审核对象与基线收据
2. 结论摘要（P0/P1/P2 分级计数 + 一页结论）
3. 逐字段对照矩阵（应有 / 实有 / 判定 / 证据）
4. 错（Wrong）清单
5. 漏（Missing）清单
6. 优化（Improve）清单
7. 改进建议路线图（P0 立即 / P1 近期 / P2 择机，标注成本与契约影响）
8. 与 job_copilot 的跨项目契约衔接建议
9. 不做什么（Non-Goals）+ 残余风险
10. 附录：可重放收据集

## 8. 已知的既有约束（避免误判为缺陷）

- trace 脱敏是设计行为（`SanitizationPipeline`），字段被脱敏不等于丢失；
- `session_id` 用 `thread_ref_v1_*` 哈希是 job_copilot 的隐私设计选择（可评估可追溯性代价，但不要直接判「错误」）；
- `release=dev`、`environment=development` 是 Tilt dev 的正常取值。

## 9. 只读访问收据（本机实测可用，2026-10-23）

```bash
# 1) Postgres 真值（只读查询；禁止 UPDATE/DELETE 既有业务行，实验行用完必须删除并留收据）
export PGPASSWORD=8siud3siDW9s02k33edsFAGDDFSDFllidsk9987sddj
psql -h 10.43.206.170 -U job_copilot -d ponysentry -c "select id,session_id,run_id from traces where id='ca0b5494-ae1b-47db-8e00-c9e7abbb6779';"

# 2) 本地服务 API（本机 3000 端口，连同一个库）
ADMIN=a723309e828ea8366cc6d22380ab31b3b74f0b171f041c5a4c7e07ff2b03c16c
CLIENT=6aa12e9e4294ddef559fd8f0d74626be9a313fad23a53868d5b07a88363c5d24
curl -s -H "x-admin-token: $ADMIN" http://127.0.0.1:3000/api/v1/traces/ca0b5494-ae1b-47db-8e00-c9e7abbb6779

# 3) 代码仓库
#    /home/dm/pony-sentry（Rust workspace + web/），/home/dm/job_copilot（生产者，Python + Vue）
```

约束：实验写入只能新增 `session_id` 前缀为 `audit-probe-` 的行，并在实验结束删除，附 `select count(*)` 收据；
禁止触碰 `thread_ref_v1_*` 既有行与任何 job_copilot 表。
