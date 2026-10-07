<!-- BEGIN constitution -->
> ⚠️ 本节是 **ponygo 初始引导（bootstrap）**：`.meta/constitution/constitution.md` 槽位未填期间的临时投影。槽位填妥并重跑 `ponygo sync` 后，本节自动被常载命约整体替换——无需手删，也不要手编本标记区。

你是进入本仓库的 AI coding agent。本项目采用 ponygo 工程化治理（治理根 `.meta/` + `.agents/`）。**在开始任何业务任务之前，先一次性完成治理引导**：

0. **建载体**：若不在 git 仓库（`git rev-parse --is-inside-work-tree` 失败），先 `git init` 并提交骨架——版本派生、提交边界、"同一次变更"的锚定、L2 钩子挂载，全部依赖 git；无 git 则治理时序在物理上不可判定。
1. **填宪法槽位**：读 `.meta/constitution/constitution.md`，替换全部 `【TODO: …】`——项目名（取仓库名）、一句话定位（读 README/源码推断，不确定就给草案并原地注明"草案，待人确认"）、技术栈（从 manifest 推断：package.json / go.mod / pyproject.toml / Cargo.toml 等）、成熟度目标（默认填 `2`）。Mission 节的 level TODO 填同一值。
2. **重投影**：运行 `ponygo sync`——本引导自动被常载命约替换。
3. **第一篇决策**：照 `.agents/skills/write-adr/SKILL.md` 落第一条 ADR（建议主题 `adopt-ponygo-governance`，class: `process`），并把 `.meta/meta.yaml` 的 `level` 改为 `1`。
4. **文档有家（v2.0 起）**：确认骨架已生成 `docs/AGENTS.md`（文档标准的家，agent 自动加载）与根 `README.md`（人读契约）；有包/模块目录（packages/crates/src）则给每个包补 `README.md`（config/semantics/limitations）或列入 `.meta/docs-tier/exemptions` 豁免——这是 L1 判据 1.9，init 只播种 AGENTS.md 自动加载层，包文档按实例语言自行补齐。
5. **自证**：运行 `ponygo status`——**无 FAIL 且无 WARN 才算引导完成**（WARN 指向治理卫生缺口：非 git 仓库 / 根目录游离计划文档 / 缺 .gitignore，逐项按提示修）。
6. **日常**：每次非平凡变更**先 ADR 后代码**（先于或同一提交，按 write-adr 技能判触发），改行为/契约的那次提交**同步更新对应文档**（same-commit，dsh 实证）；随时 `ponygo audit` 体检，或**用户要求治理审查**时用 `governance-review` 技能跑全面体检。升级路径见各目录 README（L1 文档有家 → `docs/AGENTS.md`，L2 门禁 → `.meta/gates/README.md`，L2 文档分层激活 → `.meta/docs-tier/README.md`，L3 技能 → `.agents/skills/README.md`）。建议节奏（非硬门禁）：每 N 个功能提交或用户点名时跑一次文档面复检（新鲜度/对应性抽查）。

若用户明确表示本项目不需要治理（一次性原型 / 纯实验），运行 `ponygo retire --level off` 并录一条 decision 说明。
<!-- END constitution -->
