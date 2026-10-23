# L3-A 对抗审查（amend v1.5，契约/数据完整性/并发视角）

结论：**FAIL**（3 MAJOR + 6 MINOR）。范围 `git diff c02642a..HEAD`；只读，未改代码。

| 项 | 判定 | 依据 |
| :-- | :-- | :-- |
| 1 M-04 正则 | FAIL | 单/双引号、裸 `token=` 在常规写法命中，`tokens`/`token_count` 白名单不误伤（PASS 子项）；但值内含另一种引号时提前闭合 → **部分明文泄漏**（L3A-E1）。`_redactions` 在脱敏后注入、客户端 `_` 键被丢弃 → 不可伪造/不被二次改写（PASS）。`_unmapped` 注入先于脱敏 → 敏感值会被扫到（PASS）。 |
| 2 I-02 归一 | FAIL | 去重/上限(100)实现；全非法 turns → 空数组 + `unknown`，不拒请求（PASS）。但二次上报的新 warning 被旧 `_warnings` 覆盖丢失（L3A-E3），且 `_warnings` 去重按整对象相等、只保留最早 100 条。 |
| 3 I-06 截断 | FAIL(轻) | 按**字符**截断（`chars()`），非字节（PASS）；`truncated` 可被客户端预设/不可复位、截断先于脱敏（L3A-E5）；计数按字段而非 tool_call，与 HELP 口径矛盾（L3A-E4）。 |
| 4 W-04 run_id | PASS(轻) | 写入取 `turn.run_id` → 回退顶层/`trace.run_id`，`COALESCE` 保留旧值；EXISTS 相关子查询无重复行；`idx_trace_turns_run_id` 已建。存量行不回填、SQLite 索引可能漏建（E6/E8）。 |
| 5 I-04/W-05 口径 | PASS | list/count 共用 `*_trace_filter_clause`，from/to 均含端点、run_id 走同一 EXISTS；`order` 缺省 `reported_at DESC, created_at DESC`，非法值 400。 |
| 6 迁移 0006 | FAIL(轻) | PG 逐条幂等（`add_column_target`/`index_name_of` 解析正确、CONCURRENTLY）；SQLite 列探测正确但索引随列跳过（E6）；无 down（E7）。 |
| 7 验收强度 | FAIL(轻) | 10 条均只测正路径：无白名单非回归、无 `_` 前缀键、无二次 warnings、截断只断言 `<=20000`、run_id 未测 turn≠顶层（E9）。 |

必须修（阻断）：L3A-E1 引号值泄漏、L3A-E2 `_` 前缀字段静默丢弃（违 v1.5-3）、L3A-E3 二次上报 warnings 丢失。
