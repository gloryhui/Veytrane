# 单设备恢复打洞修复与部署记录

核验日期：2026-11-16，Asia/Shanghai。运行版本为 `b981f073155c233213e2d843b2fe9dda3756efdc`；本文记录依赖该修复提交，不包含身份、凭据或完整私有配置。

## 问题与修复

存在健康连接时，失败设备的自动恢复会准备独立 UDP socket，同时保留主网络已经注册的候选地址。此前新 socket 主动打洞，已公布的主 socket 只被动回应；两端都处于恢复状态且 UDP 入站受目标端口限制时，探针无法触发被动回应。

`peer_attempt_paths` 现在同时纳入恢复网络和已公布主网络的可达路径。每条路径都执行有界 PUNCH 和 QUIC 竞争，并携带自己的 `NetworkRoute`。发起方只有选中的身份握手发送最终提交，未选中的连接和任务被关闭；接收方沿用实际 incoming route。主路径获胜后，不再把它误记为恢复网络，未使用的恢复端口可以被回收。

开始新尝试前同步撤销该设备的旧 reconnect watch，避免同一 UDP 来源同时受新旧令牌授权而出现来源歧义。仍使用原信令协议、NodeId/TLS/签名验证与代际隔离，不增加配置开关或中继兜底。

## 定向验证

Mac 共通过 7 个定向测试，Ubuntu 另通过 2 个受限 NAT 测试；未运行全量测试。

| 验证 | 结果 |
| --- | --- |
| 受限 UDP 过滤，双方分别担任 QUIC 发起方 | 通过；双方从已公布的 primary route 建立真实身份握手 |
| 仅使用 recovery socket 的负对照 | 通过；两种发起角色均无法穿过过滤 |
| IPv4/IPv6 同时恢复与过期代际隔离 | 通过；IPv6 按本机可用性执行 |
| 同来源新旧令牌歧义与旧 watch 撤销 | 通过 |
| pending、连接和 attempt 对恢复端口的持有与释放 | 通过；primary 获胜后恢复端口可重新 bind |
| IPv6 无 PUNCH 回应时继续 QUIC | 通过 |
| 失败设备自动恢复时另一设备的 TCP 流保持可用 | 通过；两种发起角色，183.93 秒 |

受限 NAT 夹具为每个私有端点维护独立的公开 UDP 代理及允许来源集合。只有目标端点先向源公开地址发包，反向数据才能进入；递交时保留源公开地址。测试直接使用生产路径组装函数，确认 recovery 来源确实被过滤，primary 的允许集未包含对方 recovery 来源。

主要命令：

```sh
cargo test --locked --features node port_restricted -- --nocapture
cargo test --locked --features node simultaneous_private_socket_recovery_uses_primary_probe_bridges -- --nocapture
cargo test --locked --features node clearing_reconnect_probe_releases_old_token_for_same_source -- --nocapture
cargo test --locked --features node expired_pending_lookup_releases_its_private_socket -- --nocapture
cargo test --locked --features node ipv6_quic_completes_when_peer_never_answers_punch_probes -- --nocapture
cargo test --locked --features node failed_peer_automatically_recovers_without_breaking_another_peers_tcp_stream -- --nocapture
```

Ubuntu 执行 `cargo test --locked --lib --features node -j 2 port_restricted -- --nocapture`。两个平台均完成 `cargo build --locked --release --bin p2p-agent --bin p2p-node`。

## 部署与实机结果

两端都从 clean 的同一提交构建，经 `scripts/node-host.py upgrade` 优雅停机、确认所有权锁释放后升级。旧版本目录和 `previous-install.json` 保留；没有数据迁移。

| 平台 | target | revision | 映射 | 最终状态 |
| --- | --- | --- | --- | --- |
| Mac | aarch64-apple-darwin | 31 | 26 条 | 20 条 Ubuntu 映射就绪，6 条 Windows 映射待连接 |
| Ubuntu | x86_64-unknown-linux-gnu | 26 | 1 条 | Mac 映射就绪；Mac、Windows 均双向信任并连接 |

Mac 先升级时，已自动连接旧版 Ubuntu；两端均升级后再次自动连接。Mac 上随后执行一次仅针对 Ubuntu 的设备断开、连接，约 1.024 秒恢复 20 条映射，管理 epoch 与 revision 不变，26 个监听恢复完整。Ubuntu 的 Windows 会话在后续验证中一直保持 generation 2。

最终通过既有 `ubuntu-local` P2P SSH 入口核对主机与用户；本机导航 `3001`、P2P Codex Web `8214` 和 Ubuntu 管理入口 `7788` 均返回 HTTP 200。Ubuntu 既有 Web 和独立 Sub2API gateway 的 PID 与升级前相同。

两端身份、管理凭据、node 配置、服务授权、映射、任务、导航私有配置/页面均通过前后对比。Mac FlClash 配置及规则表摘要一致。安装元数据和 Agent 启动路径按新 SHA 更新；其他启动字段一致。Windows 短时诊断结束后已重新加载正常 LaunchAgent，确认没有 DYLD 或 trace 环境残留。

发布二进制 SHA256：

| 平台 / 程序 | SHA256 |
| --- | --- |
| Mac p2p-agent | `b0f82865a76869c083767504c117d56d706b7635b8d03496e5651d49d220819e` |
| Mac p2p-node | `ffd55ca8b20fd153d800318c5f867a713e9ba0cf196c21140927b3a8fc6cc877` |
| Ubuntu p2p-agent | `d47ca28eaf08a5b767c94b6eee90cfac5e1347bc9076b73d2c5148092c424626` |
| Ubuntu p2p-node | `6c85a22b07b8ebfc534ca3eceacb8eee2515831e7830fe6f042e1041cbb93767` |

## Windows 六条映射的后续取证

六条本机监听都存在，peer 仍为 `authenticating`，尚未取得远端目录能力。它们分别为 RustDesk `22118`、WSL llama `18090`、StreamDesk media `18098`、SRS API `21986`、SRS HLS `28099`、RDP `13389`；本次未修改任何条目。

1. 约 64.19 秒的 Mac UDP 观察中，原主端口向 Windows 最新公网候选及 Host 候选共成功发送 180 次 PUNCH，三个配对令牌摘要，未收到 Windows PUNCH。同窗收到了 Ubuntu 探针，279 条记录未达到 2000 条上限。诊断只记录探针元数据和令牌摘要。
2. 随后的独立信令窗口在 15:42:49 确认，服务器向 Mac、Windows 下发同一配对令牌及对端候选；Windows 当前公网端口为 `12274`，Host 端口为 `59895`。这与早先 `11910/53382` 的注册候选已不同。
3. 15:43:08–18，Ubuntu 抓到与 Windows 公网端口 `12274` 的双向 QUIC 包。因此该候选对 Ubuntu 有效，不能推断 Windows 整体 UDP 不可达。
4. Ubuntu 查询 Windows 服务目录成功但返回 `[]`，仅代表该 Ubuntu NodeId 没有可见的已启用服务。Mac 尚未建立有效会话，不能据此判断六项 Windows 服务是否存在或是否授权给 Mac。

UDP 观察与信令捕获属于两个时间窗口，没有将前者的三个令牌与后者的单次配对强行对应。当前已将 Mac↔Windows 卡点收窄到 PUNCH 回程/处理阶段；缺少 Windows 侧实际收发和运行版本证据，仍无法区分 Windows 未发包、恢复 socket/目标选择或途中过滤。Windows 二进制、身份、代理和配置均未修改，也未启用 Relay。

本修复覆盖已确认的恢复路径缺陷及上述受限 UDP 过滤场景，不保证任意端点相关 NAT 映射都能直连。继续定位 Windows 需要其实际运行版本、候选 socket 的收发及 QUIC 日志。

## 证据与恢复边界

私有证据和停机前备份分别保存在：

- Mac：`/Users/glory/p2p-file-builds/recovery-fix-20261116-0kbsju_f`
- Ubuntu：`/home/glory/app/p2p-file-builds/recovery-fix-20261116-0kbsju_f`

其中保存前后 manifest、配置/任务备份、二进制构建信息与摘要、测试日志、运行核验、UDP 汇总及脱敏信令候选记录。原始令牌、身份与凭据不进入仓库；信令捕获未保存原始 pcap，没有遗留后台抓包。

若需回退，先检查活动业务，优雅停止新 Agent 并证明锁释放，再按 `previous-install.json` 恢复旧启动路径。无需恢复身份或覆盖节点数据；旧版本会重新具有本文所述恢复缺陷。本文是部署证据，不能单独改变运行版本。
