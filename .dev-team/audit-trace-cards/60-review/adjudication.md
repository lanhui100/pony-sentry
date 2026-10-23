# 终审关闭状态裁决：PASS
- prior finding 实测 28 条（L3-A 14 + L3-B 14，v2 头部记「24 条」不实）：CLOSED 26 / PARTIAL 2 / OPEN 0；无 OPEN，PARTIAL=2 ≤3 且均非 BLOCKER → **PASS**。
- 口径：v2 须有可复核的正文改动（新增 finding / 改建议 / 改数字）；仅「已裁决」一句声明不计 CLOSED。

## PARTIAL（2）
1. **L3A-F6 严重度与判据漂移** — §9.2 已声明重排依据（P0=直接阻断本次诊断）并给出逐行「裁决依据」，但该定义无阈值，且与自身行冲突：§4.1 判 D2、D5「不能」，§4.2 承认补 M-02 可把 D5、D7 拉到满分，M2 仍定 P1（理由仅「显著削弱」）。建议：§9.2 增可复核阈值（如「使任一必答题判『不能』且无替代证据 = P0」），据此复核 M-02/W-06；否则「P0 仅 1 条」不可复核。
2. **L3A-F8 27 判据映射** — 映射已补齐（W1-W8/M1-M8/I1-I11 无缺席，W7/M4 不再整条丢失），但 brief:69 要求的 39 行逐字段矩阵仍「不在正文复述」，仅指向 `10-contract/01-field-matrix.md`。建议：作附录 A 收 39 行摘要，或在 §9.2 明写「偏离 brief:69、已获 Lead 裁决」，把遗漏变为已声明偏离。

## 重点 13 条核对（均已达实质改动）
- F7 路线图 → 新增 **Wave 0（立即）** 承载唯一 P0（M-01）+W-07/W-08/I-10，先于 Wave A；「零改动」在 §4.2 限定为「入库并原样返回」，展示改动移入 Wave C：CLOSED
- F9 → W-02 明写「`FOR UPDATE` 对首次上报无效（0 行无锁）」，改 advisory lock / 单语句 upsert+serializable：CLOSED
- F10 → §6 前置约束双写：schema 三项标「越出 brief 非目标·待裁决」，未授权走 advisory lock + payload #>/GIN；改控制台须重走 console-timeout 冻结并复核 `API_TIMEOUT_MS` 不变式：CLOSED
- L3B-F1 → 撤回「user_id 仅 dev/preprod 外发」，§6/§7/§8.2 一律禁列，M-06 补 input_text 前须隐私裁决；读侧矩阵 §9.1 记 Wave D：CLOSED
- L3B-F2 → M-01 只要求受控 `error_type`、明令「不允许 error_message」，与 R1 冲突消解：CLOSED
- L3B-F3 → §5 容量补充：2MB 只是单次 POST body，实测 2.4MB 行 / 2.4MB GET / 4.4MB List，并入 I-04/I-06：CLOSED
- L3B-F4 → 新增 W-09，并在 §6/§7/M-05/I-07 全线不用 `extra`：CLOSED
- L3B-F6 → M-03 降级为 metrics+待评审队列+日报先行，webhook 延后至 W-01/W-02 修复后：CLOSED
- L3B-F7 → D8 降 0.5/回链「不可达」，禁明文 thread_id，W-10 记 session_id 兜底：CLOSED
- L3B-F13 → W-02 补 lock_timeout/池上限/写放大/trace_turns 行数估算：CLOSED
- L3B-F14 → §7 给 turns ≤64 / 单 turn ≤256KB / 合并 ≤2MB + `truncated:true`：CLOSED
- 其余：F1/F2 verifier 回填与字节口径、F3/F4→W-07/W-08、F5/F10→M-04、F11→I-09、F12/F13 根因表述、F14 锚点：均 CLOSED

## 发布前文案 erratum（不影响判定）
- §3 标题「错——9 条」与 W-01..W-10 共 10 条不符；头部「24 条 finding（L3-A 10 / L3-B 14）」与两份 review 各 14 条（共 28）不符（L3-A 实为 MAJOR 10/MINOR 4）。
- §9 行「F12 吸收 G-01..G-08」偏宽：G-06 无落点、G-08 仅 L2 一行覆盖。
- §9.2 未列 W9/W10（v2 新增 finding）的映射行，宜标「新增，无对应判据」。
