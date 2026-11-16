# Mac↔Windows 单设备重连实机验收

日期：2026-11-16，Asia/Shanghai。接续 `WINDOWS_RECOVERY_UPGRADE_20261116.md`，本次用户明确重新授权 UAC。未启用 Relay、未重启 Agent 或其他服务。

## 操作与结果

本轮仅调用一次现有导航 `/api/navigation/reconnect`，完整目标为既有 Mac NodeId；该动作顺序执行指定 peer 的 disconnect/connect，不重建 Agent 核心。没有追加 connect、重连或其他人工恢复操作。

- 唯一操作 ID：`955286819f3e46d2abe8d78e18811aa2`。
- POST 开始：19:01:03.915；完成：19:01:04.043，HTTP 200，耗时 128 ms。该响应只证明管理动作接受，不代替认证验收。
- Windows 侧 Mac generation 从 99 进入 100，并经过完整状态序列到达双向 trusted/connected。
- 首次含 connected 事件的查询于 19:01:04.493 返回，即 POST 开始后 **578 ms 的观察上界**；首次 connected 状态样本为 19:01:04.839，即 924 ms 的保守上界。事件没有原始 wallclock，不能把查询返回时刻写成精确握手完成时刻。
- Ubuntu 始终为 Windows 侧 generation 2；同一条 SSH/P2P TCP 流持续返回 210 个心跳，正常退出。

验证范围为当前网络条件下，一次管理端单设备重连后的协议自动运行与恢复；不将它外推为任意 NAT 环境或长期稳定性保证。

## 基线及保留

| 项目 | 本轮前后结果 |
| --- | --- |
| 实际运行版本 | 0.1.0，`fc13ff9ec92db112bf71d6ad8e428c9f459b52db`，clean，`x86_64-pc-windows-msvc` |
| Windows NodeId | `5eef2d53912e77479fa2e61ef3aa27c3`，不变 |
| Mac NodeId | `ae4c49f3d120f1e013155039eb3263ce` |
| Ubuntu NodeId | `cbd82de8be77d5f813fc3ff92663b2ac` |
| Agent owner | 唯一 PID 4460；管理员采集前核对实际加载的既有版本路径，没有启动第二个 owner |
| epoch / revision | `814df6c9f93513f71ed2736e8b46e9df` / 13，前后不变 |
| 信令 | 原 `47.101.183.194:8900`，TCP |
| Relay | `relay_server=null`，前后均未配置 |
| 配置摘要 | identity.key、admin.token、node.json、tasks.json、导航 config.json/私有 HTML、installed.json、previous-install.json 共八项全部相同 |
| 转发 | 收尾原 17 项均 listening、peer_ready；授权和映射配置未修改 |
| 启动及网络规则 | 未修改原计划任务、执行策略、代理/TUN/防火墙规则；未重启任何服务 |
| 执行环境 | 管理员 PowerShell 7.6.10，保持 RemoteSigned，无 ExecutionPolicy override |

原始 token、导航管理 token、身份私钥、密码和完整私有配置不进入报告或提交。

## 同轮信令与 PUNCH

采集从 19:01:00.388 开始，早于唯一重连请求。信令帧按 TCP 序列重组，通过项目生产 `SignalMessage::decode` 解码；没有复用先前窗口的 token。

| 时间 | 观察 |
| --- | --- |
| 19:01:04.014154 | Windows 向原信令服务器发送 Lookup |
| 19:01:04.040504 | Windows 收到 Mac PeerCandidates；本轮 token SHA256 前 12 位为 `3b235cd32068` |
| 19:01:04.047970 | `61.52.86.166:60670` → `192.168.100.169:56297`，完整同 token PUNCH |
| 19:01:04.246703、.247196 | Windows primary `56297` → Mac 公网候选 `61.52.86.166:60670`，两个完整同 token PUNCH |
| 19:01:04.250325、.251006 | Mac 公网地址 → Windows primary，再收到两个完整同 token PUNCH |

共保留 **8 个完整 PUNCH**：Mac→Windows 3 个，Windows→Mac 公网候选 2 个，Windows→另外三个 IPv4 Host 候选各 1 个。八个包的 token 摘要与本轮 PeerCandidates 完全一致。PUNCH 的剩余 16 字节是随机填充，不能误当作 NodeId 或 generation。

本轮收到的 Mac IPv4 候选为：

```text
61.52.86.166:60670
172.30.215.132:60085
192.168.1.50:60085
198.18.0.1:60085
```

另有多项 IPv6 候选，包括 `2408:8221:321:b8f0:...:57842` 和 `[fdfe:dcba:9876::1]:57842`。本轮包观察限于 IPv4；没有为测试修改候选、固定公网端口或调整 IPv6 路由。

Windows 原 primary 绑定为 `0.0.0.0:56297`，此时公布的公网候选仍为 `222.137.31.236:12370`。实际 IP 层出站源为 `192.168.100.169:56297`，不能用它推断到 Mac 的 NAT 后源端口。181 次端点采样均保留 primary；此次没有观察到新 recovery socket，捕获学习到的本地 PUNCH 端口也只有 56297。

## QUIC 与认证事件

同轮公网 PUNCH 使用的双向地址对 `192.168.100.169:56297` ↔ `61.52.86.166:60670` 上，原始包保留了 **3 个 UDP datagram、5 个完整 QUIC v1 长报头包**：

| IP 层包时间 | 方向 | UDP 内的 QUIC 包 |
| --- | --- | --- |
| 19:01:04.251827 | Windows → Mac | Initial，1200 字节 |
| 19:01:04.261213 | Mac → Windows | Initial 149 字节 + Handshake 265 字节，位于同一 datagram |
| 19:01:04.262449 | Windows → Mac | Initial 95 字节 + Handshake 103 字节，位于同一 datagram |

Windows 的 source CID 摘要为 `bcc18a3c83be`，Mac 返回包的 destination CID 与之吻合；Mac 的 source CID 摘要为 `466fd17c7c55`，Windows 后续包的 destination CID 与之吻合。CID 摘要同样使用 SHA256 前 12 位，不保存原始 CID 到报告。

解析按 version 1、CID 长度、varint Length 和合并包边界检查，不把 UDP fixed-bit 形状当成握手证据。这里有两个长报头的 QUIC bit 为 0；离线工具按 RFC 9287 §3.1 允许的结构保留该值。此次没有解密来验证 grease transport parameter，身份认证结论来自下方 Agent Events。工具另按 RFC 9000 §17.2.5 修正了 Retry 的 opaque token + 16 字节完整性尾部解析，本轮握手没有记录 Retry。

Initial 内的 46 字节 QUIC token 与 16 字节应用层 PUNCH token 是不同字段，未混合做摘要或代际关联。前述信令/PUNCH token 对齐本轮 Mac peer；同地址对的 QUIC 包和下方唯一 gen100 生命周期共同补齐 Windows 侧证据链。此次实际观测到的握手经过 Windows primary IPv4 56297，验证依据为原始包和事件 journal；短时应用 UDP sink 保持到期关闭。

Agent Events journal 为本轮提供了连续的有序认证证据：

| Sequence | Windows 侧 Mac peer 状态 |
| --- | --- |
| 980 | generation 99 disconnected |
| 981 | generation 100 pending |
| 985 | generation 100 punching |
| 987 | generation 100 authenticating |
| 989 | generation 100 negotiating |
| 990 | generation 100 authorization_pending |
| 993 | generation 100 connected，双向 trusted，既有 legacy_import 信任来源 |

本轮事件总 sequence 为 980–998，19 个事件连续，无缺口、无重放、无 resync_required；其余为 changed。所有 7 个 PeerState 都属于 Mac，没有 Ubuntu PeerState。

observer 总计 377 个事件查询、191 个状态样本，error_count=0。Mac 从首个 gen100 connected 状态样本起至 19:04:56.584，181/181 个后续状态样本保持双向认证；Ubuntu 191/191 个样本保持 gen2 connected、双向 trusted。不能用“最近的状态样本”把新 token 错配给 POST 前的 gen99，关联依据是唯一操作、PeerCandidates 的完整 Mac NodeId，以及上述有序 gen100 事件。

19:16:44.135 交付前再次只读复核，Mac 仍为 gen100、Ubuntu 仍为 gen2，均双向 trusted/connected，17 个原出站规则全部 listening/peer_ready；唯一 PID 4460、epoch、revision 13 和运行 SHA 保持。该样本另存为 `status-delivery.json` / `delivery-summary.json`，不把离散复核扩写成全程连续采样。

## Ubuntu 同一条健康 TCP 流

本轮只启动一次既有 Windows `127.0.0.1:2222` → Ubuntu SSH/P2P 通道，使用已有密钥及严格主机密钥校验。没有 SSH 重试或重新连接。

- 同一 run UUID `d6a94f9b-3380-4579-b6a0-f85b684225c3`、同一远端 Python PID 3874278、主机 `ubuntu`。
- 首心跳：19:01:01.521281；末心跳：19:04:30.545658。
- 序号 0–209，共 210/210，完整连续，SSH returncode=0、stderr=0 字节。
- 全程最大间隔 1.0559852 秒，发生在序号 38 附近，不与重连操作重叠。
- 重连前心跳 2：19:01:03.522809；重连后心跳 3：19:01:04.522436；跨操作间隔 **0.9996276 秒**。

该结果支持“本轮 Mac 单设备重连未中断 Ubuntu 的这条既有 TCP 流”。不把它扩展为 Ubuntu 所有业务服务的验收，也没有清除 runtime 中遗留的无时间戳 last_error 10054。

## 捕获边界与清理

采集按 180 秒期限于 19:04:00.391 正常结束。三个 IPv4 接口共读取 273,745 个 IP 包，持久化 21,703 包（16,777,176 字节包记录）。16 MiB 上限被触发，130,094 个额外匹配包因上限未持久化；实际保留包时间为 19:01:00.389158–19:01:23.959336，约 23.57 秒。完整信令、双向 PUNCH 和上述 QUIC 握手均位于保留前段。不能把尾部缺失解释成链路没有流量，持续状态和 TCP 健康另由全程事件/心跳核对。

观察点为本机 `RCVALL_IPLEVEL`，不是网卡 L2、NAT 后或 Mac 侧独立抓包；包时间为用户态接收时间，Winsock 没有提供内核丢包计数器。完整本地抓包不能证明 Mac 独立管理日志里的每个内部阶段，也没有将此结果推广为所有 NAT 都可直连。

三个 receive-all socket 全部恢复 RCVALL_OFF 并关闭，采集进程正常退出 0。observer 按 stop marker 正常结束；协调器、observer、管理员采集 helper 均已退出，pcap 只读独占打开成功。没有保留临时抓包进程或网络规则。

私有证据目录：

```text
C:\Users\glory\AppData\Local\p2p-file\backups\windows-recovery-20261116-veytrane\controlled-reconnect-20261116-190053
```

核心文件为 `reconnect-request.json`、`reconnect-result.json`、`ip-window.pcapng`、`raw-capture-result.json`、`signal-pcap-analysis.json`、`event-alignment.json`、`quic-v1-correlation-v5.json`、`independent-quic-headers.json`、`events-samples.jsonl`、`runtime-status-samples.jsonl`、`endpoints-samples.jsonl`、`healthy-stream-run-*/result.json`、`preservation-after.json` 和 `final-preservation-cleanup.json`。QUIC 最终计数以 v5 及独立原始报头复核为准，保留的旧分析草稿不作为验收依据。

pcap SHA256：`98c86ec7e5088ffaeff7ccc3ef4a3876f5f09fdb9e7cbd13703bccdfb822e500`。

本阶段仅更新验收文档，没有修改或重新构建 Agent。依赖此前 `fc13ff9` 的实际运行程序及 `4562c53` 记录的准备工具与边界；没有推送。
