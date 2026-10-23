# ext 波终审关闭记录（Lead）

终审 `adjudication-ext.json` 判 FAIL，唯一 OPEN 为 **L3B-E7**（`payload_bytes` 指标口径与 HELP 文案相反）。
Lead 直接修复（1 处，属小改豁免）：

- `crates/server/src/routes.rs`：ingest 入口计算 `ingest_bytes = serde_json::to_vec(&raw_payload).len()`，
  `record_trace_payload_bytes(ingest_bytes)`——计数改为**本次上报体积**，与 HELP「上报体积（非存储体积）」一致；
- 证据：`cargo test -p pony-sentry-server --test api_test` → 33 passed / 0 failed（Exit 0），
  含 `test_metrics_payload_bytes_help_describes_upload_volume` 与指标累加用例；
- 其余 6 条 MAJOR（L3A-E1/E2/E3、L3B-E1/E3/E4）终审已判 CLOSED；MINOR 全部 CLOSED/登记。

结论：ext 波 16 条 findings 全部关闭，`L3B-E7` 已按上修复；复跑门禁见 `gates-lead-v16-final.txt`。
