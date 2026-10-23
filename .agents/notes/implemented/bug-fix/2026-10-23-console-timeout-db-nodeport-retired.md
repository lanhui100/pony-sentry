# Agent Note: Console "请求超时" — dev DB NodePort 下线后 DATABASE_URL 指针漂移

Status: implemented

## Problem

2026-10-22 起控制台三个数据源（Issues / Traces / Projects）统一报
「数据加载失败 / 请求超时：服务响应过慢或网络不稳定，请稍后重试」，持续约 12 小时。

现场证据链（全部为机器产出，非推理）：

| 观测 | 结果 |
| :--- | :--- |
| `psql` / `pg_isready 127.0.0.1:30543` | `Connection refused`；`ss -ltnp` 无该端口监听 |
| `curl /healthz`（宿主机直连 3000） | `503` + `{"db":"down"}`，耗时 2.001s（探活 2s 超时） |
| `curl /api/v1/issues` | `500 Database error: pool timed out while waiting for an open connection`，耗时 3.003s |
| `journalctl -u pony-sentry` | 首条 `healthz db probe timed out` = 2026-10-22 22:26:28 CST，累计 2895 条；`ingest persistence failed` 11 条（3.01s 后） |
| `psql 10.43.206.170:5432`（dev Service ClusterIP） | `accepting connections`，`ponysentry` 库可读 |
| `kubectl get svc -n dev job-copilot-postgres` | `type: ClusterIP`，managedFields 变更时间 `2026-10-22T13:58:33Z`（= 21:58:33 CST） |
| 临时实例（同二进制，PORT=3999 + ClusterIP DSN） | `/healthz` 200 17ms；`issues/traces/projects` 200 5–14ms |

根因：同集群 `job_copilot` 的「dev 公网入口退役 / 红队 H5 整改」在 2026-10-22 21:58 CST
以 `kubectl patch` 把 `dev/job-copilot-postgres` 由 NodePort `30543` 改为固定 ClusterIP
`10.43.206.170`（仓库侧改动次日 09:40 才提交为 `f6128e01d`）。PonySentry 跑在宿主机
systemd、`/etc/pony-sentry/pony-sentry.env` 的 `DATABASE_URL` 仍指向 `127.0.0.1:30543`
——这是仓库外的消费方，未进入该整改的爆炸半径清单。旧连接池里经 conntrack 保活的 TCP
连接又撑了约 28 分钟，直到 22:26 全部失效才暴露。

为什么 UI 看到的是「超时」而不是「服务端错误」：前端 `API_TIMEOUT_MS = 3000`
与服务端 `PgPoolOptions::acquire_timeout(3000ms)` 相等，浏览器在 3.000s 先 abort，
服务端 3.003s 才返回 500 —— 前端永远赢不到这条 500，于是归因成 `TimeoutError`，
显示成「网络不稳定」。这是本轮排障被误导 12 小时的直接原因（记为已知诊断盲区）。

## Decision

只改运行态指针，不改代码、不动集群对象：

1. `/etc/pony-sentry/pony-sentry.env` 的 `DATABASE_URL` 主机改为
   `10.43.206.170:5432`（宿主机经 kube-proxy `OUTPUT` 链 DNAT 直达 pod，实测建连 ~0.1ms）。
   改前备份为 `pony-sentry.env.bak-<时间戳>`，`systemctl restart pony-sentry` 生效。
2. 本仓库 `deploy/systemd/pony-sentry.env.example` 同步改为 ClusterIP 并写清「不要再用
   `127.0.0.1:30543`」的原因，避免下次照模板部署复发。

验证收据：`/healthz` 200 `{"db":"up"}` 3.4ms；`issues?limit=100` 200 3.6ms / 57KB；
`traces?limit=100` 200 26ms / 1.5MB；`projects` 200 3.5ms；浏览器登录后状态灯「就绪」，
Issues 6/3/3/18 计数正常、Traces 列表正常，页面无 `load-error` 横幅、`console` 与
`errors` 均为空。

## Alternatives considered

- **恢复 NodePort 30543**（`kubectl patch svc … type=NodePort,nodePort=30543`，即 job_copilot
  回滚手册里的「即时止血」）：30 秒可恢复，但把刚被红队要求关掉的「全节点全网卡暴露数据库」
  重新打开，且下次 `tilt up` 按 chart 渲染又会漂移回 ClusterIP，否决。
- **`kubectl port-forward` / `ssh -L` 隧道 + systemd 托管**：进程生命周期与集群 API 耦合，
  断线即再次全站故障，且引入代理头会与 tailnet trust 的 fail-closed 语义冲突，否决。
- **把 `ponysentry` 库迁到独立 PostgreSQL 实例**：彻底解耦，但需要停机迁移与新的备份链，
  超出本次故障处置范围，留作长期选项（本文不承诺）。
- **只改 `/etc` 不改模板**：下次按模板重建环境必然复现同一故障，违反 same-commit 文档义务，否决。
- **顺手调大队列/调小 acquire_timeout 修掉误报**：会改变前端加载语义与既有测试冻结的超时契约，
  需独立 ADR 与红相测试，本次不动，作为后续建议记录。

## Consequences

- 控制台恢复；`ponysentry` 库仍与 `job_copilot` dev 共享同一 Postgres 实例，连接目标从
  「NodePort 字面量」变成「ClusterIP 字面量」，两者都是外部事实指针：
  **`10.43.206.170` 在 `job_copilot` 的 `charts/job-copilot/values-dev.yaml` 里被固定**，
  该值若变更，本仓库 env 必须同步（机械可查：`curl -fsS -m 3 http://127.0.0.1:3000/healthz | grep -q '"db":"up"'`，非零退出即失败）。
- 备份不受影响：`PG_DUMP_MODE=cluster` 走 `kubectl exec` 在 Pod 内 `pg_dump`，不经过该 DSN
  （10-23 03:33 备份仍成功，530786 bytes）。
- 故障窗口内 11 次 ingest 落库失败，客户端已收到 500，数据不可回补；控制台只展示已落库数据。
- 遗留改进（靠 review + 后续 ADR）：前端 3000ms 超时应大于服务端 3s acquire_timeout，
  否则任何 DB 侧 500 都会被伪装成「网络不稳定」；`job_copilot` 的迁移记录也应把本宿主机
  消费方写入爆炸半径清单。
- 相关记录：`implemented/architecture/2026-10-08-refactor-to-host-systemd-service.md`
  （宿主机 systemd 部署口径，其 DB 访问路径已链入本条）、
  `implemented/bug-fix/2026-10-22-console-load-error-diagnostics.md`（本次把 500 误报成超时
  所依据的错误分类逻辑）。
