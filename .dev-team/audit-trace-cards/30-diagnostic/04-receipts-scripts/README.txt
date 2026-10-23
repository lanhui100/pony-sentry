# 可重放脚本（凭证从环境变量读取，禁止写入仓库）

前置（见 00-baseline/00-brief.md §9）：
  export PGPASSWORD=<job_copilot PG 口令>
  export ADMIN=<pony-sentry ADMIN token>
  export CLIENT=<pony-sentry CLIENT token>
  export PSQL_HOST=10.43.206.170

运行：
  bash rcp03-assertions.sh    # RCP-03 18 条机器断言（需先 curl 出 /tmp/t3.json，脚本内含）
  bash rcp06-merge-probe.sh   # RCP-06 合并语义/error 通道探针（写入 audit-probe-* 并自动清理）
  bash rcp07-filter-probe.sh  # RCP-07 run_id 过滤 + 顶层字段丢弃探针（自动清理）
  bash rcp02-inventory.sh     # RCP-02 字段清单
  bash rcp08-local-truth.sh   # RCP-08 job_copilot 本地 span/event 真值（kubectl 只读）

纪律：所有 probe 只写 session_id 前缀 audit-probe-*，运行末尾自带 delete 与 count(*) 收据。
