# 契约：控制台 API 读超时跨层不变量（R1–R7）

**Status: FROZEN (ver 4)** · Owner protocol-agent（task-1）· 执行方 T3 Executor · 验收 T2 Test Agent（本表只读；改任何一行 = 破冻结，须 Lead 裁决并升 ver）。ver 4 = 依据 L3-A 三处假绿穿透加固对齐判据（覆盖 ver 1–3），裁决记录见 §4（ver 2）/ §5（ver 3）/ §6（ver 4）。

**Binds**：`.dev-team/nfr-baseline.json#console_api_timeout_ms` · `web/src/lib/apiTimeout.js` · `web/src/App.vue`（import 与 `/healthz` 探活）· `crates/server/src/main.rs:152` · `crates/core/src/db.rs:9`（R4 绑定的 acquire_timeout 全集）
**适用范围**：控制台**入站**读请求的浏览器侧超时（`apiFetchJson`、`/healthz` 探活）与服务端 PostgreSQL 连接池等待（`acquire_timeout`）。不含 SSE/流式、上传，以及本服务**对外发起**的 webhook/通知（后者归 `external_call_timeout_ms=3000`）。

## 0. 竞态成因：为何前端超时反而必须大于服务端超时

1. DB 不可达时服务端先耗满 3s 池等待，再由 axum 生成 500（事故实测 ~3.003s）。
2. 若前端同为 3000ms，浏览器在 500 到达前抢先 abort；`apiFetchJson` 把 `AbortError` 改写为 `TimeoutError`（`web/src/App.vue:1078-1082`），`classifyLoadFailure` 归为 `timeout`。
3. 于是「DB 不可达」被译成「请求超时：服务响应过慢或网络不稳定」，真实原因被掩盖 12 小时。前端超时 = 服务端池等待 + 网络/解析余量，必须更晚到期。

## 1. 规则表（冻结）

| 规则 | 判据（机器可查） | 反例 |
| --- | --- | --- |
| **R1 单一来源**：控制台所有 API 读取超时只能取自 `web/src/lib/apiTimeout.js` 的 `export const API_TIMEOUT_MS`；`App.vue` 内不得再出现裸数字 `3000` 充当 API 超时；`/healthz` 探活复用同一常量。 | `grep -c "export const API_TIMEOUT_MS" web/src/lib/apiTimeout.js` == 1；`grep -nE "\b3000\b" web/src/App.vue` 零命中（`\b` 不误伤 `30000`）；`grep -c "const API_TIMEOUT_MS" web/src/App.vue` 零命中（定义残留兜底，AMB-b）；healthz 定时器形如 `setTimeout(() => healthController.abort(), API_TIMEOUT_MS)` 恰 1 处；`timeoutMs = API_TIMEOUT_MS` 恰 1 处且位于 `apiFetchJson` 函数体内；`abort(), <数字>` 与 `healthController.abort(), <数字>` 各 0 处；`@/lib/apiTimeout` 的 import 恰 1 处；剥离这三处合法用途后若仍有 `API_TIMEOUT_MS` 引用 → 判「死引用凑数」违规。 | `App.vue:918 const API_TIMEOUT_MS = 3000` 残留；healthz 里写死字面量 `3000`。 |
| **R2 下界（ver 2）**：`API_TIMEOUT_MS` 必须严格大于 `acquire`（R4 的 max 值），且余量 **> 2000ms**（预算下限 2000，判定严格；余量须覆盖池等待之后的网络 RTT 与 JSON 解析）；基准取值 **8000ms**，即 `acquire=3000` 时最小合法 **5001**。 | 断言 `API_TIMEOUT_MS > A + min_margin_over_acquire_ms`（严格 `>`，等价余量 > 2000，不接受 =2000）；`nfr-baseline.json#console_api_timeout_ms.value == 8000`、`min_margin_over_acquire_ms == 2000`（仅声明预算下限）；运行常量与 baseline `value` **不要求强相等**，各自落区间即可（AMB-c）。 | `value=5000`（余量恰 2000，判负）；`value=3000`（== acquire，余量 0，事故原型）；`value=4999`。 |
| **R3 上界**：`API_TIMEOUT_MS` ≤ 15000ms，防止单请求挂死拖住 UI 与刷新队列。 | `nfr-baseline.json#console_api_timeout_ms.max_ms == 15000`；断言 `API_TIMEOUT_MS <= 15000`。 | `20000`；`Infinity`/`0`（0 会让 `setTimeout` 立即 abort）。 |
| **R4 服务端真值来源（ver 2）**：真值 = `crates/**/*.rs` 全集内所有 `acquire_timeout(Duration::from_millis(N))` 的 **max**（当前 `crates/server/src/main.rs:152` 与 `crates/core/src/db.rs:9` 均为 3000 → A=3000）；零匹配 / 解析失败 / 文件不可读时契约测试必须 **fail-closed**（非零退出），禁止静默通过或 skip。 | 递归 glob `crates/**/*.rs`，正则取全部 `acquire_timeout\(Duration::from_millis\((\d+)\)\)`，`A = max(...)`；`A` 为空、任一候选非十进制整数、目录/文件不可读 → `process.exit(1)`；`grep -nE "\.skip\(|test\.skip|describe\.skip" web/tests/` 零命中；变异收据（ver 3 改正）：`db.rs` 改 `6000` → A=6000 → `8000 > 6000+2000` 为假 → 下界判负可复现；清空全部来源 → 测试非零退出。（ver 2 所写「改 `4000` 可复现下界失败」错误，已删：`8000 > 4000+2000` 为真，仍通过。） | 只读 `main.rs`（漏掉 `db.rs`，或两处分叉时取错值）；`false`/`return true` 兜底；`t.skip()` 或 catch 后 `console.warn` 放行。 |
| **R5 归因语义**：DB 不可达（服务端 500 先到，R2/R3 保证前端尚未 abort）时 `loadFailureDetail` 必须给出「服务端错误：PonySentry 返回 5xx，请查看服务日志」，不得为「请求超时」；既有 auth > timeout > server > network 优先级与 `web/tests/load-error.test.js` 断言语义保持不变。 | `node --test web/tests/load-error.test.js` 退出 0（真实回归）；行为优先级矩阵 4 条全绿：auth>timeout、auth>server、timeout>server、server>network；`loadError.js` 源码分支顺序判据：`kinds.has('auth')` 必须先于 `timeout`/`server`/`network`（`web/tests/api-timeout-contract.test.js` 的「R5 分支顺序判据」）；`{issues:'server'}` → 精确匹配 `服务端错误：PonySentry 返回 5xx，请查看服务日志`；`{issues:'server',traces:'timeout'}` → `/请求超时/`；`{issues:'server',projects:'auth'}` → `/鉴权失败/`。 | 把 `status>=500` 分支提到 `timeout` 之前（改优先级）；把 500 文案改成「请求超时」。 |
| **R6 最小闭包**：本次实现只允许 ① 新增 `web/src/lib/apiTimeout.js` ② `App.vue` 改 import 与引用 ③ Rust 侧一行注释。禁止重试、退避、新 fetch 封装或任何额外行为变更。 | `git status --porcelain` 仅含上述三项 + 本契约 + `nfr-baseline.json`；`grep -nE "retry|backoff|sleep" web/src/lib/apiTimeout.js` 零命中；`git diff crates/` 的 `+` 行数 == 1（注释行）。语义最小性机器到不了 → **靠 review**。 | 顺手加重试/指数退避；抽 `request()` 封装；顺手改 `apiFetchJson` 默认值以外的逻辑。 |
| **R7 对抗判据**：三类夹具（边界 / 解析器缺失 / 畸形输入）必须被测试逐值判定真假——违规值判负且测试进程非零退出，合法边界值判正；测试必须确定性，禁止 sleep/概率等待。 | ① 边界（ver 2，四值全覆盖）：`5000`（= A+2000）判负、`5001`（= A+2001）判正、`15000` 判正、`15001` 判负；② 解析器缺失：`crates/**/*.rs` 零匹配、目录/文件不可读 → 判负；③ 畸形输入：`from_millis(abc)`、`from_millis()`、`from_millis(-1)` → 判负；`grep -nE "sleep\(|setTimeout|Math\.random" web/tests/` 零命中。 | 用 `await sleep(3000)` 模拟服务端 500；把边界夹具写成 `4000` 等非边界值；畸形输入「跳过即可」。 |
| **R8 漂移哨兵（ver 4）**：`A` 实测值必须恰为 `3000`（当前 `crates/server/src/main.rs:152` 与 `crates/core/src/db.rs:9` 均为 3000）；任一侧 `acquire_timeout` 变化都必须重走契约冻结并复核 `API_TIMEOUT_MS`——这是刻意的警报，不是误杀。 | 对 `crates/**/*.rs` 的**每个** acquire_timeout 站点逐一断言取值 `=== 3000`（不是取 max 后比对），失败时输出含文件路径的站点清单；失败文案必须含 `acquire_timeout 漂移：须重走契约冻结并复核 API_TIMEOUT_MS`；该断言不得 skip。 | 写成 `A >= 3000`；漂移只 `console.warn` 放行；改了 `db.rs` 却不升契约 ver、不复核 8000。 |

## 2. 校验收据（物理收据）

- 命令：`cd /home/dm/pony-sentry && python3 /home/dm/.agents/skills/dev-team/scripts/dev-nfr-lint.py .dev-team/nfr-baseline.json`
- 退出码：`0`（2026-10-23，执行方 protocol-agent）
- stdout 原文：

```text
[PASS] dev-nfr-lint 校验通过: .dev-team/nfr-baseline.json
```

- **ver 2 复跑（2026-10-23，裁决回写后）**：同一命令，退出码 `0`，stdout 原文 `[PASS] dev-nfr-lint 校验通过: .dev-team/nfr-baseline.json`。
- **ver 3 复跑（2026-10-23，AMB-a/b/c 回写后，未重跑契约测试）**：同一命令，退出码 `0`，stdout 原文 `[PASS] dev-nfr-lint 校验通过: .dev-team/nfr-baseline.json`。
- **ver 4 复跑（2026-10-23，L3-A 假绿穿透加固对齐后）**：同一命令，退出码 `0`，stdout 原文 `[PASS] dev-nfr-lint 校验通过: .dev-team/nfr-baseline.json`。
- 本收据只证明 NFR 基准表格式与取值合规（既有 6 个顶层键零改动，`git diff` 为 8 行纯插入）；R1–R7 的契约测试由 T2 实现，其实施后收据另附。

## 3. 历史歧义（ver 2 已全部裁决，正文规则已按裁决回写，条目见 §4）

- AMB-1 判定边界互斥 → 已裁决：严格不等式（见 §4.1）。
- AMB-2 `=15000` 判定归属 → 已裁决：15000 判正、15001 判负（见 §4.2）。
- AMB-3 第二处 `acquire_timeout` → 已裁决：纳入绑定、取 max（见 §4.3）。

## 4. Lead 裁决记录（ver 2）

- **4.1 AMB-1 — 取严格不等式（R7 优先）**：余量必须 `> 2000`，即 `API_TIMEOUT_MS > acquire + 2000`；acquire=3000 时最小合法 **5001**；`nfr-baseline.json#min_margin_over_acquire_ms=2000` 保留为「声明预算下限」，下界判定用严格不等式；夹具 `5000` 判负 / `5001` 判正。*理由*：落地事故的余量是安全垫，等号落在垫子边缘即失去容错，且边界必须可机械判定唯一真假。
- **4.2 AMB-2 — 批准原判**：`=15000` 判正、`15001` 判负，R3 闭区间成立，上界不改。*理由*：15000 是 R3 明文允许的闭合上界，误判为负会让合法基准无法通过自检。
- **4.3 AMB-3 — 纳入绑定**：真值 = `crates/**/*.rs` 中所有 `acquire_timeout(Duration::from_millis(N))` 的 **max**（实测 `crates/server/src/main.rs:152` 与 `crates/core/src/db.rs:9` 均为 3000 → A=3000）；零匹配 / 解析失败 / 文件不可读 → fail-closed 非零退出。*理由*：SQLite 是默认部署路径，只绑 PG 那一行会让最常见部署的超时不变式失去守卫。

## 5. ver 3 裁决记录

- **5.1 AMB-a（R4 变异收据不自洽）— 原文认错并改正**：ver 2 的「`db.rs` 改 `4000` 后下界判定随之失败」不成立（`8000 > 4000+2000` 为真，仍通过），该错误收据已删；正确变异为 `db.rs` 改 `6000` → A=6000 → `8000 > 6000+2000` 为假 → 下界判负。同时把「`A` 必须恰为 `3000`」升格为契约明文 R8「漂移哨兵」：任一侧 acquire_timeout 变化都必须重走契约冻结并复核 `API_TIMEOUT_MS`，这是刻意警报而非误杀。*理由*：变异收据必须真能复现失败，否则等同于零假测试的同谋；漂移静默通过会让不变式悄悄失效。
- **5.2 AMB-b — 批准**：R1 的「恰 3 处」计数不独立充分（同一常量被 import/默认值/healthz 复用时可被误数），正文补一条兜底断言：`const API_TIMEOUT_MS` 在 `App.vue` 必须零命中。*理由*：定义残留才是包内第二来源的真身，计数只是间接证据。
- **5.3 AMB-c — 不加强绑定**：运行常量与 `nfr-baseline.json#console_api_timeout_ms.value` **不要求相等**；基准是决策记录（8000），常量是运行值，两者各自落在 R2/R3 区间内即可。*理由*：强相等会让未来调值必须同时改基线与契约（三次写盘换一个数字），收益低于噪声。

## 6. L3-A 裁决记录（ver 4）

- **6.1 FINDING-1（R1 计数可凑）— ACCEPTED_FIXED**：ver 3 的「恰 3 处 + `const` 零命中」只数次数，可在 import、`timeoutMs = 3000`、healthz 字面量三处各凑一次而全部通过。判据改为按**语义形态**逐条锚定（healthz 定时器形如 `<timer>(() => healthController.abort(), API_TIMEOUT_MS)` 恰 1、`timeoutMs = API_TIMEOUT_MS` 恰 1 且在 `apiFetchJson` 内、`abort(), <数字>` 类 0 处），并补「剥离三处合法用途后仍有引用 = 死引用凑数」违规。加固落点：`web/tests/api-timeout-contract.test.js`（37 用例全绿）。
- **6.2 FINDING-2（R5 `git diff` 恒空）— ACCEPTED_FIXED**：已提交工作区里 `load-error.test.js` 的 `git diff` 永远为空，该判据零证明力。改为行为优先级矩阵 4 条（auth>timeout、auth>server、timeout>server、server>network）+ `loadError.js` 源码分支顺序判据（auth 分支必须先于 timeout）+ 真实运行 `node --test tests/load-error.test.js` 回归。
- **6.3 FINDING-3（R8 `max` 掩盖单点）— ACCEPTED_FIXED**：取 max 时单点**下漂**会被另一站点掩盖（如 `db.rs` 由 3000 降到 1000，max 仍为 3000），哨兵形同虚设；只有上漂（改 6000）才暴露。改为**每个站点**逐一断言 `=== 3000` 并输出含路径的站点清单，失败文案固定含漂移告警串。
- **6.4 为什么原判据会被绕过**：计数可凑（形态错、次数对）；聚合量（max）掩盖单点；版本库对照在干净工作区恒真空。三者共同病根是**判据锚在可被满足的统计量上**，而非锚在被约束的语义形态上。
- **6.5 附带修正**：R1 内 `:1041` / `:1234` 两个易漂移行号已删除，改用符号名（实现后实际为 `timeoutMs = API_TIMEOUT_MS` :1040 与 healthz 定时器 :1233）。
