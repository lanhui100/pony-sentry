# Agent Note: Console Deploy Rollback and Embedded-Token File Hardening

Status: implemented

## Problem

`deploy/scripts/deploy-console.sh` 已能构建期内嵌 `ADMIN_TOKEN` 并做基本冒烟，但加固不足：

- `web/dist` 只在脚本最后一步还原；构建成功后任一中途失败都会把带 token 的产物留在 git 工作区。
- 安装新二进制前没有备份，服务或自检失败后无法自动恢复上一版本。
- 自检只覆盖 `/api/v1/issues`，没有覆盖 `/api/v1/projects` 与 `/api/v1/traces`。
- 内嵌 token 的 `/usr/local/bin/pony-sentry-server` 安装为 `0755`，同机其他用户可直接读取并提取 token。
- 上述部署流程没有 CI 机械门禁，后续改动容易静默退化。

## Decision

`deploy/scripts/deploy-console.sh` 现在是带失败回滚与 secret 文件权限约束的部署入口：

1. 用 `trap cleanup EXIT` 在所有退出路径无条件执行 `git checkout -- web/dist`，且 `set -Eeuo pipefail`。
2. 安装前把现有 `BIN_DEST` 备份到临时目录；新二进制先写 `$BIN_DEST.new` 再原子替换。
3. 服务未存活、`/healthz` 非 healthy、`/api/v1/issues` / `/api/v1/projects` / `/api/v1/traces` 任一鉴权请求非 200、产物未内嵌 token、二进制权限异常，任一条失败都自动装回备份并重启服务。
4. 安装权限改为 `0750 root:<service-group>`，并机械拒绝 other-readable 的部署产物。
5. 新增 `tests/deploy/test_deploy_console_hardening.py` 与 CI 门禁，锁住 trap、回滚、三接口自检和 0750 约束。
6. `README.md` 的部署段同步记录上述运行语义。
7. 同批修复 Web 构建门禁的 `pnpm --filter` no-match 假绿，见
   `implemented/testing/2026-10-22-web-build-gate-false-positive.md`。

## Alternatives considered

- **只补 `chmod 0750`**：解决本地读取面，但不能覆盖中途失败残留 token 或无回滚的问题，否决。
- **每次在临时 git worktree 构建，彻底避免主工作区出现 token 产物**：隔离性更强，但会复制依赖、增加构建时间和路径参数；当前以 EXIT trap 覆盖所有可捕获失败，成本更低。若将来需要抗 SIGKILL，应升级为隔离 worktree。
- **不回滚，只报错并人工处理**：部署故障时生产会停在未知二进制上，恢复依赖人工，不够幂等，否决。
- **把 ADMIN_TOKEN 改为服务端会话/短期令牌**：能彻底移除静态产物中的长期 token，但涉及后端登录态、CSRF 与前端改版，超出本次内网控制台部署加固范围；继续由 tailnet 暴露面约束，后续如公网化再立项。

## Consequences

- 部署失败会自动恢复上一版本；工作区 `web/dist` 在正常/异常退出时都会还原为 committed 无 token 版本。
- 部署机上的二进制必须由 service group 可执行；当前 systemd 为 `User=dm`，脚本自动读取 unit 的 User/Group 或回退到 `id -gn dm`。
- 若进程被 `SIGKILL` 打断，EXIT trap 无法执行；这是当前方案的剩余风险，彻底消除需要隔离构建目录。
- 生产已用新脚本重装，并验证 `0750` 权限、健康检查、三个读接口与浏览器渲染。
