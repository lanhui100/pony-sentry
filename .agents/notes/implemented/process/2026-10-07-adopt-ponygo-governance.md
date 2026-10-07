# Agent Note: Adopt Ponygo Governance

Status: implemented

## Problem

PonySentry 是 Pony 家族生态中聚焦客户端缺陷追踪与智能体闭环治理的开源服务。项目需要建立可追溯、防熵增的工程化治理体系，确保所有非平凡变更与架构决策都有据可查，契约与文档同频演进。

## Decision

采纳 ponygo 工程治理规范（治理根 `.meta/` + `.agents/`）：
1. 明确宪法核心定义与常载命约（Standing Orders），成熟度目标设定为 L2。
2. 设定决策必须先于代码或与代码同次提交落盘于 `.agents/notes/`。
3. 遵循 ponygo 状态流转机制与机械门禁检查。

## Alternatives considered

- **纯自发无约束开发**：缺少统一决策沉淀与规范门禁，随项目演进容易产生文档滞后、架构漂移与熵增。
- **重型企业级管理规范**：引入过重的流程文档（如外部 Wiki/Jira），阻断 AI Agent 自动化协作与自省闭环。
- **采纳 ponygo（选中）**：轻量本地化、文档有家、代码与决策同构提交，契合 Agent 协作工程标准。

## Consequences

- 团队所有重大技术决策、数据契约及状态演进必须记录于 `.agents/notes/`。
- 本地构建与提交前需通过 `ponygo status` 机械门禁。
