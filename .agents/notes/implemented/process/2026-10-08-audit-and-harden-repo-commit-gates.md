# Agent Note: Audit and Harden Remote Repository and Commit Gate Security

Status: implemented

## Problem

公开 GitHub 仓库及本地提交若缺乏严密的敏感凭据拦截防线，极易在多人协作或自动化开发过程中无意泄露生产凭据（Token、私钥、连接密码等）。
审计现有仓库发现：
1. 本地 `.meta/gates/` 提交门禁脚本未被绑定为全局/本地 Git 钩子（`core.hooksPath` 未配置），本地 `git commit` 时未实际触发扫描；
2. `.gitignore` 仅忽略了 `*.key` 和 `*.crt`，遗漏了 `.env`、`*.env.local`、`*.pem`、`*.pfx` 等本地开发环境配置文件及私钥变种；
3. CI 扫描流水线（`.github/workflows/ci.yml`）中的安全检测命令带有 `|| true` 且仅匹配简单正则，检测到泄露时不会使 CI 非零退出阻断合并；
4. 提交门禁与 CI 的正则规则未覆盖常见云服务与平台 Token（如 GitHub PAT `ghp_`、GitLab PAT `glpat-`、Slack Token `xoxb-`、JWT 以及各类 RSA/EC/OPENSSH/DSA 私钥块）。

## Decision

1. **环境与文件忽略加固**：
   - 在 `.gitignore` 中补充忽略所有 `.env`、`*.env`、`*.env.local`、`*.env.*.local`、`*.pem`、`*.pfx`、`*.p12`，同时白名单豁免 `!*.env.example` 和 `!*.example.*`。
2. **本地提交门禁加固与生效**：
   - 强化 `.meta/gates/pre-commit` 正则表达式，覆盖多类私钥头（RSA/EC/OPENSSH/DSA）、JWT、GitHub/GitLab/Slack Token 及常见硬编码密码键值。
   - 配置仓库 Git `core.hooksPath .meta/gates`，使得本地提交自动触发门禁。
3. **CI 流程阻断加固**：
   - 改造 `.github/workflows/ci.yml` 中的 `Secret Leak Prevention Scan` 步骤，移除盲目 `|| true` 放行，命中敏感特征时输出 `::error::` 并以 Exit Code 1 阻断 CI 流程。
4. **全历史合规核验**：
   - 扫描现有全量 Git 历史（含各分支与历史 commit），确认未存在泄漏的有效真实密钥与凭据。

## Alternatives considered

- **引入第三方大型扫描镜像或 SaaS 服务（如 GitGuardian / TruffleHog Action）**：增加外部依赖与 CI 构建时长，对于当前纯 Rust+Web 轻量项目，采用原生 POSIX 正则机械门禁在毫秒级内即可完成零依赖硬阻断。

## Consequences

- 本地与远端 CI 均具备严格零容忍凭据防泄漏门禁，任何潜在明文秘钥均无法进入仓库。
- 本地开发人员生成的临时 `.env` 或证书私钥不会被 `git status` 误加入暂存区。
