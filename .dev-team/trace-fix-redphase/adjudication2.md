# 补修轮终审复核（round 2，只读）

- 裁决：**PASS** — 复核 7 项全部 CLOSED，open=[]，无 MAJOR PARTIAL。
- L3A-F4 ✅ ingest/retention 共用 `hashtextextended($1,0)`（pg_repository.rs:819 / :1003）；锁序统一 advisory→行锁，无反向边；PG 交错实测「聚合行=6 轮次行=73 不一致=0」、孤儿=0（gates-fix-core2 原始日志）。
- L3B-F4 ✅ 0005 逐句 autocommit（:79-140）；列存在查 information_schema 跳过（:143）；5 索引 CREATE INDEX CONCURRENTLY + pg_indexes 预检（:157）；二次 ensure 报告 columns 0/3、indexes 0/5，invalid=0（0005 无 UNIQUE 索引，改写全覆盖）。
- L3A-F8 ✅ 两处锁键均为 `hashtextextended($1,0)`；`hashtext(` 计数 0。
- L3A-F9 ✅ `checked_add` + 溢出置 null 并写 `stats_incomplete:true`、每轮重算（trace_v2.rs:202-210,483-497）；单测与仓储级用例实跑绿。
- L3A-F6 ✅ 幂等用例删行 + turns_count 归零强制命中回填扫描，真实重放 INSERT（trace_v2_repository_test.rs:975-991），本轮实跑 1 passed。
- L3B-F11 ✅ 默认无 token 放行（C9.9 仍 200）、配置后先鉴权再触 DB 返回 401 `Unauthorized`（routes.rs:1003-1044，main.rs:179）；本轮实跑 metrics 5 用例全绿。
- L3B-F8 ✅ 事件面板 `getPayloadPreview`（20KB）+ 面包屑 `breadcrumbSummary`（512 截断）；`JSON.stringify(ev.payload)` 已消失（App.vue:508,1520-1529）；web 40 passed。
- 收据：gates-fix-core2（core 81/clippy 0）、gates-fix-http2（178/acceptance 11）、gates-fix-web2（40）、gates-lead-final3（183、SKIP=0）。
- 本轮只读定向实跑：core lib 1、trace_v2_repository 1、server api_test metrics 5、web 40 —— 全绿。
- 范围外遗留（未复核、不计入本轮 open）：L3A-F7 默认无 `PONY_TEST_PG_URL` 仍 SKIP 即通过。
- 残差提示：`CREATE TABLE IF NOT EXISTS` 仍每次执行；未来 UNIQUE 索引不会自动 CONCURRENTLY；`breadcrumb.message`、span/turn 文本化无长度上限。
