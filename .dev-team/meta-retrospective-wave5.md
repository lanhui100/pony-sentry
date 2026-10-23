# 元框架复盘：wave-5 / tailnet 可信对端免 admin token

> 复盘对象是**协作元框架与组织协议本身**，不是业务代码变量命名。上一轮（wave-4）复盘已归档为
> `.dev-team/meta-retrospective-wave4.md`。

## 1. 通信拓扑与信噪比（Topology & Noise）

- 本轮 6 个角色：Lead（编排+裁决+同提交文档）→ Explorer×1（E1/E2 侦察）→ 三路方案对抗（安全 / 部署现实 /
  实现陷阱）→ Protocol Agent（契约）→ Test Agent（红相）→ Executor（绿相）→ L2-AT（对抗测试，Subagent 池）
  → 三路 Join Barrier 审查（L3A 安全 / L3B 并发 / L3-P 五问）。名册常驻 4（Lead+Protocol+Test+Executor），
  其余全部走 Subagent 池，未触碰 Roster ≤8 上限。
- 信噪比最高的三个决策点全是**外部视角推翻 Lead 初稿**：① 安全评审用「DNS rebinding + Host 头」证明仅靠
  IP 段与 csrf 无法防住跨站写；② 部署评审用 `ss`/`iptables` 实测把「猜 IP 段」变成「可验证的对端事实」；
  ③ 实现评审用 axum-test 源码证明「非 Option 提取器会让 11 个既有用例 500」。三者都是 Lead 单点思考的盲区，
  验证了「方案逆向对抗 ≥2 路」不是形式主义。
- 噪声：Protocol Agent 首版契约漏掉 Host 白名单（因 Lead 的修正消息在其完成之后到达），产生一次 ver1→ver3
  的返工；根因是**并行派发与裁决串行化的时序没有上板**（契约任务应在裁决收敛后才派发，或显式 blocked_by）。
- 令牌产出比：一次真实缺陷（跨主体会话驱逐）由 L3B 用可复现命令抓出并附带修法，优于十条风格建议。

## 2. 门禁穿透与误杀率（Gate Penetration / False Negatives & Positives）

- **真实拦截**：L3-B 抓到「未认证可信流量可驱逐手动会话」，L3-P 抓到「部署脚本外部调用无超时」
  「审计日志缺少 request_id 关联」，L3-A 抓到「`Forwarded` 头未纳入代理门」。四类都是**功能测试全绿但
  生产语义有洞**，机器门禁（fmt/build/clippy/19 个测试 target/17 条契约）在此完全沉默 —— 证明对抗审查层不可被
  机器门禁替代。
- **误杀/过度声明 1 例**：L3-P 把部署脚本 `curl -m 5` 判为突破 `external_call_timeout_ms=3000`，该 helper 属
  既有实现；Lead 裁决为「新增调用改用 `-m 3` 专用 helper，既有冒烟不动」，按最小闭包而非全量改超时。
- **豁免 2 例（带证据、非逃生舱）**：全局 `tracing_subscriber::fmt` 非 JSON、`SessionStore` 锁毒化史遗留 ——
  均非本次引入，登记为 backlog 事项而非 NFR-DEGRADED 欠账（`nfr_degraded=0`）。
- **红相真实性**：冻结测试文件在本轮**零改动**（`git diff --numstat` 可验），实现期间 17 条黑盒从 5 红转 17 绿；
  执行者被明确禁止触碰 `tests/**`，且 L2-AT 独立新增 951 行对抗用例，形成三方分权。
- **物理凭证纪律**：所有放行都有机器原生收据（Exit Code、`ss` 输出、curl 状态码、浏览器 0 console error +
  截图），没有一条结论只靠推理。

## 3. 分工契约与隔离有效性（Contract Isolation）

- 写域隔离有效：Executor（`crates/server/src` + deploy 配置 + README）、Test Agent（`tests/**`、
  `crates/server/tests/**`）、Protocol（`trust.rs` 空桩 + `.dev-team/contract`+NFR）、Lead（docs/ADR/
  deploy 脚本/state）。全程零写域冲突；Protocol 越域一行 `lib.rs pub mod trust;`（编译衍生）被显式追认并记录。
- 信息过度下发的风险点：Lead 把「契约 ver2 的 4 条硬红线」同时下发给了 Executor 与 L2-AT，导致 L2-AT 部分用例
  与 Test Agent 重叠（XFF/Host 门各写一遍）。重叠 15 条断言里没有一条产生不同结论 —— 属**冗余验证带来的成本**，
  在安全敏感特性上可接受，但不该在低风险特性上照抄。
- 波次并行度：本轮实际串行（WIP=1）—— 鉴权链、会话存储、前端行为高度耦合，并行收益小于整合成本，
  ROI 判定自动落回串行，符合「先问是否值得并行」的原则。
- 契约演进：ver1→ver2（Host 白名单 + 只读兑换）→ver3（Forwarded + 空 CIDR 降级 + 五字段审计）三次升版，
  每次都有冻结断言与之对应，Executor 按契约实现、Test Agent 不追改断言，未出现「改测试迁就实现」。

## 4. 元协议迭代建议（Self-Evolving Protocol）

1. **契约派发必须串行化**：契约任务（Protocol Agent）应 `blocked_by` 到「Lead 方案裁决完成」，或明确要求
   Protocol 在动笔前向 Lead 回读裁决摘要；本轮因并行派发产生 ver1 作废成本。
2. **安全类特性的「三与门」模板**：IP 段类信任必须强制包含 `Host 白名单`（反 rebinding）与
   `代理头门`（只拒不放）两项，否则 L3-A 直接 FAIL —— 建议写入 dev-team skill 的安全专项检查清单。
3. **部署脚本纳入 NFR 扫描面**：`external_call_timeout_ms` 目前只被心照不宣地用于服务端；应显式声明
   「运维脚本的外部命令（tailscale/curl/systemctl）同样受 ≤3s 硬超时约束」。
4. **审查产物早落盘**：L3A 首轮 subagent 运行 20 分钟未产出文件（后由重派实例完成）。建议审查员
   **先落盘骨架 JSON（verdict=FAIL/待定 + 空 findings）再深化**，避免长任务丢失可审计痕迹。
5. **红相口径写入 skill**：本轮 Test Agent 拒绝使用 `#[should_panic]` 吸收空桩 panic（会变成假绿），
   改为「用例存在且失败原因是能力缺失」——该口径值得固化为 L2-T 的标准做法。
6. **仪表化并行**：本轮 6 个子角色并行提交结论，Lead 侧对账成本集中在「谁在改哪个文件」。建议任务板强制
   记录 `started_at/last_artifact`，用 mtime 变化替代 `wait_agent` 轮询（本轮已实践，无空转等待）。
