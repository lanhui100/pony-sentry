# Agent Note: Upgrade Issue Fingerprint to v2 Tuple and Power Indexed Reindex

Status: implemented

## Problem

生产库 `ponysentry`（k3s dev）实测发现聚合乱象：issue `3a317b92`
（count=20）一只 issue 混装了 3 个模型（muse-spark[1m] / muse-spark /
antigravity/gemini-3.8-flash），而同一模型 `antigravity/gemini-3.8-flash`
又同时出现在另一只 issue `9d02ed8b`（17 条）。根因有三：

1. **旧指纹方案无 error_kind**（commit `8724f4c` 及更早）：`sha256(platform:error_type[:culprit])`
   没有 error_kind 维度；而 `GatewayExhaustedError` 上报的 `exception.stacktrace`
   恒为 null → `culprit = None` → 所有模型共享同一指纹 `sha256("rust:GatewayExhaustedError")`，
   11:32–12:04 的 20 条 3 模型事件全部误并（复算命中 `076983fab…`）。
2. **判别器被 culprit 有无否决**：`compute_fingerprint_with_discriminator` 仅在
   culprit 为 None 时把 `requested_model` 纳入指纹。同错误"带堆栈/不带堆栈"
   会因判别器被忽略而拆成两只 issue。
3. **模型别名过度拆分**：客户端上报 `requested_model` 时带/不带 provider 前缀
   （`antigravity/gemini-3.8-flash` vs `gemini-3.8-flash`）会拆成不同 issue，
   二者实为同一模型同一失败。

12:07:34Z 部署 `6f936d0`（model fingerprinting）后新事件按判别器正确拆分，
但历史 issue 仍被旧指纹污染；且 `error_kind`（QuotaExhausted / UpstreamUnavailable /
RateLimitExceeded）仍未参与聚合。

## Decision

1. **指纹升级为 v2 五元组**（`crates/fingerprint/src/hasher.rs` 的
   `compute_fingerprint_v2`）：

   ```
   sha256(platform : error_type : error_kind : model : culprit)
   ```

   - `error_kind`、`model` **无条件参与**（不再受 culprit 有无影响）；
   - `error_kind` 规范化：`RateLimitExceeded { retry_after: None }` 这类 Rust Debug
     串取 `{` 前变体名，收敛抖动字段；
   - `model` 规范化：剥掉 provider 前缀（`antigravity/gemini-3.8-flash` →
     `gemini-3.8-flash`），收敛别名；
   - culprit 保留 clean_culprit（去行号抖动），仅在存在 stacktrace 时附加。
2. **指纹真源收敛**（`crates/ingest/src/fingerprint.rs` 的 `compute_issue_fingerprint`）：
   实时上报（`routes.rs`）与全量重索引共用同一函数，杜绝"重索引后新事件落不进
   正确 issue"的二次漂移。
3. **幂等全量重索引**（`crates/core/src/reindex.rs` + trait 方法
   `IssueRepository::reindex_fingerprints` + sqlite/pg 双实现）：
   - 以新指纹为唯一分组键重建 issue 归属；
   - 旧 issue 与新组 1:1 映射时复用 id 并保留人工状态（status/assigned_to）；
   - 合并/拆分场景生成新 id，旧 issue 清空后删除；
   - 指纹解析失败的事件原地保留；重复执行结果不变（幂等）。
   - 入口：`PONY_REINDEX_FINGERPRINTS=1 pony-sentry-server`（`crates/server/src/main.rs`）。

## Alternatives considered

- **只清 `3a317b92` 一只脏 issue，不动指纹逻辑**：治标不治本——error_kind
  未入指纹、判别器被 culprit 否决、模型别名拆分三个隐患仍在，新数据还会继续
  产生误并/误拆；未选择。
- **v2 指纹去掉 project 维度**：project 以首报为准（已有决策），不参与聚合，
  维持原语义；未加入指纹。
- **error_kind 用完整 Debug 串**：`RateLimitExceeded { retry_after: Some(5) }`
  与 `None` 会互相拆开，聚合不稳；故规范化到变体名。
- **模型别名不归一**：`antigravity/gemini-3.8-flash` 与 `gemini-3.8-flash` 将永远
  拆成两只 issue，同一次故障告警翻倍；剥 provider 前缀后归一。
- **重索引一次性脚本不回写**：只 dry-run 不落库无法端到端清理；直接写幂等
  重索引工具并回写（可复跑、可验证）。

## Consequences

- 历史脏数据清理：`3a317b92` 等旧指纹 issue 在重索引后被拆分/合并/删除，
  issue 列表恢复"一模型一失败原因"粒度。
- 新上报聚合：同一错误带/不带堆栈不再拆分；同一模型不同失败原因（配额/
  上游/限流）正确分离；provider 前缀别名合并。
- 重索引幂等可安全复跑；指纹真源收敛避免两套路径漂移。
- 已通过单元测试与集成测试：cargo test --workspace 全绿
  （hasher 8 例、ingest fingerprint 4 例、reindex 规划 5 例 + SQLite 端到端 1 例）。