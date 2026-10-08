# Agent Note: Refactor PonySentry Deployment from k3s to Native Host Systemd Service

Status: implemented

## Problem
PonySentry 后端本身是纯 Rust 编写、且在编译期将前端 SingleFile Vue 单页资产全量内联的单一无依赖可执行文件（ELF）。
但在先前部署中，为了套用集群规范强行将其封装进 Docker 镜像并运行在 k3s Pod 中。这引发了两个显著矛盾：
1. **构建部署过重**：前端修改 1 行样式需要跨越 Vite build → Cargo release build → Docker build → docker save → containerd ctr import → kubectl rollout，全流程需 1~2 分钟。
2. **上下文割裂**：AI 诊断调度器 `dsh_dispatcher.py` 必须跑在宿主机以调度 `dsh headless` CLI 及访问本地工作区；容器化服务与宿主机 Python 脚本跨网络边界双向调用，极易因网络或环境差池引发回调与认领故障。

## Decision
实施架构扁平化改造（方案 A）：
1. **原生宿主机守护进程**：直接在宿主机配置 `/etc/systemd/system/pony-sentry.service`，托管于 `systemd`，监听 `127.0.0.1:3000`。
2. **数据库直接连通**：通过本地 NodePort `127.0.0.1:30543` 访问现有的 PostgreSQL `ponysentry` 数据库，数据零丢失无感知平滑延续。
3. **Traefik/Ingress 切流**：将 k3s 内的 `pony-sentry` Service 改造为 ExternalName（指向 `host.k3s.internal` 或网关节点 IP）或通过 Endpoints 映射至宿主机的 3000 端口，保留外部域名 `sentry.ponyjob.top` 的 TLS 与 Ingress 路由。
4. **dsh_dispatcher 同构**：Dispatcher 运行在同一宿主机操作系统空间内，直接以 `http://127.0.0.1:3000` 与服务端互通。
5. **安全下线**：下线 k3s 内原有的冗余 Deployment 与 Pod，释放容器计算资源。

## Alternatives considered
- **保留 k3s + 本地 Registry**：仍然存在容器环境与宿主机 DSH 执行空间的网络隔离问题，未解决根本矛盾。
- **前后端彻底分离至 CDN/对象存储**：PonySentry 作为轻量监控控制台，单二进制内联交付是最优雅且免跨域、零静态运维的最佳模式。

## Consequences
- 发布与重启耗时由 90 秒缩短至 3 秒（`cargo build --release && sudo systemctl restart pony-sentry`）。
- 宿主机诊断与服务端完全同构，消除容器网络穿透障碍。
