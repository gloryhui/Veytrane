# Windows Agent 升级与 Mac 路径取证结果

核验日期：2026-11-16，Asia/Shanghai。本文接续 `WINDOWS_RECOVERY_VALIDATION_20261116.md` 的 17:10 阶段记录；不包含身份私钥、密码、原始打洞 token 或完整私有配置。

## 结论与完成边界

用户明确授权以本机 PowerShell 7、管理员身份、现有 RemoteSigned 执行已准备的升级脚本后，Windows Agent 于 **17:25:23** 完成升级。实际运行 SHA 为 `fc13ff9ec92db112bf71d6ad8e428c9f459b52db`，包含 `b981f07` 的 primary/recovery 修复及本次短时诊断；身份、revision、配置、信任与授权、映射、导航和原启动方式均保留。

**18:04:48.583 的首次成功状态样本中，Mac 已在 generation 96 建立双向已认证会话**，Windows Agent 仍为 PID 4460、原 epoch，Ubuntu generation 2 也保持 connected。前一个 17:54:44 样本中 Mac generation 72 仍 failed，实际恢复时刻只能限定在两次样本之间，不能用首次观察时刻代替握手完成时刻。

用户随后明确重新授权 UAC，已在管理员 PowerShell 7、原 RemoteSigned 下完成 18:04:53–18:05:38 的有效 IP 层捕获，观察到 primary IPv4 socket 的双向 UDP。捕获开始前 Mac 已 connected；本窗口没有 PeerCandidates 或 PUNCH，不能补成成功 generation 96 的 token/握手证据。早期失败 generation 的应用日志仍证明 primary/recovery 均主动发出 PUNCH 调用，但不能与本窗口强行配对。

Ubuntu↔Windows 双向认证及既有 Windows `127.0.0.1:2222` SSH/P2P 入口保持可用。Windows→Mac 原 RustDesk 转发也已收到协议帧。六项 Windows 发布和 Mac 授权都正确保留；四项 WSL 后端仍无监听。未发现已配置的 Mac 独立管理通道，六条 Mac→Windows 入口均未端到端验收。

## 实际安装、启动和保留检查

| 项目 | 实测结果 |
| --- | --- |
| 版本 / target | 0.1.0 / `x86_64-pc-windows-msvc` / clean |
| 实际运行 SHA | `fc13ff9ec92db112bf71d6ad8e428c9f459b52db`，来自 IPC status；管理员进程查询核对实际加载路径 |
| Agent 路径 | `C:\Users\glory\AppData\Local\p2p-file\bin\fc13ff9ec92db112bf71d6ad8e428c9f459b52db\p2p-agent.exe` |
| CLI 路径 | 同目录 `p2p-node.exe`，与 Agent 构建元数据及候选摘要一致 |
| 实际进程 | 新 PID `4460`，父 PID `2532`，所有者 `glory`；仅一个 Agent owner |
| 旧 owner | PID `16072`，已核实旧加载路径/所有者；优雅停止，确认目录、身份及任务锁释放后才启动新 owner |
| Windows NodeId | `5eef2d53912e77479fa2e61ef3aa27c3`，保持原身份 |
| data_dir / revision | `C:\Users\glory\AppData\Local\p2p-file\node` / `13`，不变 |
| 启动入口 | `\Veytrane-Agent-Boot`，AtBoot、30 秒 delay、原账户、Password、Limited、IgnoreNew、原重试设置 |
| 任务变更 | 仅版本可执行路径及对应 WorkingDirectory 更新；Principal、Triggers、Settings、原 Arguments XML 保持 |
| 配置保留 | identity.key、admin.token、node.json、tasks.json、导航 config.json/私有 HTML 摘要逐项相同；6 项服务、17 条出站映射、2 项可信设备保持 |
| 网络及策略 | 路由表、防火墙 profile 和包监视器过滤器快照相同；未写代理/TUN/防火墙规则；PowerShell 7.6.10 的原 RemoteSigned 保持 |
| 导航 | 原 `http://127.0.0.1:3002/` 返回 200，保持现有导航配置和 3001 出站映射 |

发布程序 SHA256：

```text
p2p-agent.exe  04d3262bc779d37b303948d5445720c62a5bc508e1b4fbab6a48b04939458e81
p2p-node.exe   d394760b3d30eb290df3dda381bfa134aafd559938bc7a78274eb2793186a2d6
```

临时诊断参数只用于这次启动，任务 Arguments 随后恢复原值，不再重启 owner。日志写入有 90 秒期限及 4096 条/1 MiB 上限；17:45 以只读独占打开确认 writer 已关闭，文件大小和最后写入时间稳定，无后台日志任务。运行进程命令行仍保留本次已到期参数，不代表日志仍在写入。

## 按 generation、token 摘要和路由对齐的证据

Agent 观察窗口为 17:25:23.133–17:26:24.559，共 1116 条实际事件；最后一条事件早于默认 90 秒截止，不表示观察期限被延长或日志达到上限。日志中共有 17 次成功 watch 注册及其 17 次 drop，7 个不同的 token/generation 组。

Windows 此轮公布的 IPv4 primary 候选端口为 `56297`，公网候选 `222.137.31.236:12370`，Host 候选包括 `172.30.253.231:56297`、`172.31.192.1:56297`、`192.168.100.169:56297`。primary IPv6 socket 为 `[::]:56296`，公布的 IPv6 listen 仅 `[::1]:56296`。这些是采样值，未写入配置。

Windows 本轮收到并用于 Mac attempt 的 IPv4 候选：

```text
61.52.86.166:53542
172.30.215.132:53817
192.168.1.50:53817
198.18.0.1:53817
```

Mac IPv6 候选为多项 `2408:8221:321:b8f0:...:61972` 及 `[fdfe:dcba:9876::1]:61972`。这是 Windows attempt 实际使用的候选集合，不等于已核实 Mac 此轮收到了哪些 Windows 候选；没有 Mac 侧或信令服务端的同轮接收证据。

代表性同轮记录如下。表中摘要为完整 16 字节 token 的 SHA256 前 12 个 hex，只用于证据对齐：

Mac generation 6 的四路 attempt 始于 17:25:55.264，generation 7 始于 17:26:18.221。这里的源端口来自实际应用 socket 绑定，尚未得到网卡捕获中的源 IP/端口证明。

| peer / generation | token 摘要 | IPv4 primary / recovery 发送调用 | IPv6 发送调用 | 入站及 QUIC |
| --- | --- | --- | --- | --- |
| Mac / 6 | `9b4ec4e89952` | `56297` 与 `60784` 各 120 次成功，共 240 次；同一 token、primary route 1 / recovery route 2 | `[::]:56296` 与 `[::]:60785` 各 12 次失败，均 NetworkUnreachable / 10051 | Mac PUNCH 入站 0；12 次 IPv6 QUIC dial 开始后均失败；无 winner |
| Mac / 7 | `b7e60f536003` | 两个 IPv4 socket 各 120 次成功，共 240 次，仍为同一轮 token 的两条路径 | 两个 IPv6 socket 各 12 次失败，均 NetworkUnreachable / 10051 | Mac PUNCH 入站 0；12 次 IPv6 QUIC dial 均失败；无 winner |
| Ubuntu / 2 | `8f0e57f9372a` | primary `56297` 共 7 次成功调用 | 本次获胜为 IPv4 | 来自 `39.151.73.27:17948` 的两次 matched PUNCH（17:25:24.233 / .241），随后 QUIC ready、primary route 1 direct winner |

`send_to ok` 只证明 Windows socket 接受发送调用，不证明包离开网卡或到达 Mac。Mac IPv4 路径没有取得正确 token 的 PUNCH 回应，未进入 IPv4 QUIC dial；IPv6 路径的本机发送已明确返回 10051，不能把它归类为远端身份认证拒绝。

第一轮全部诊断中，PUNCH 发送为 867 次成功、96 次失败；仅两条入站记录，均属于 Ubuntu 且 token matched，没有 unknown-token 或 source-limit 记录。Mac 没有入站可用于判断来源地址、generation、旧 watch 或授权 claim 的具体拒绝分支；没有证据证明认证被放宽或应当放宽。旧 token/watch 和 socket 释放的必要边界另由已通过的定向测试覆盖。

## 包级观察限制与连接复核

补充 PktMon 窗口约 17:31:26–17:32:58，使用所有网络适配器、无过滤器、完整包记录；结束后停止本任务的监视器并分别转换普通与 drop-only pcapng。两份结果均只有 176 字节容器元数据、0 个 packet event，连正常 Ubuntu 流量也没有记录。该 collector 没有有效正对照，不能作为“未收包”“未发包”或“没有信令”的证据。

首轮脚本还发现了状态判断错误：PktMon “没有运行”也返回 0，最初被误判成已有监视会话，故首轮只取得 Agent 日志。补充捕获改为检查实际状态内容。两个时间窗口没有强行拼接 token/generation。

首次附加 Winsock IP 层观察的 UAC 请求返回“操作已被用户取消”，当时未创建 raw socket 或 ip-window.pcapng。随后用户明确重新授权 UAC，才进行了下述 45 秒捕获；没有修改执行策略。

17:42:46–17:43:32，通过既有 Windows P2P SSH 入口保持同一条 Ubuntu TCP 会话，连续收到 0–45 共 46 个心跳，正常退出码 0。17:47 只读复核：Ubuntu 仍运行 `b981f07`、revision 26，Windows peer generation 6、Mac peer generation 5 均 connected、双向 trusted；Windows 侧 Ubuntu generation 2 connected。没有重启或部署远端 Ubuntu、Mac、信令服务器或业务服务。

17:54 阶段 Windows 17 条出站规则均有实际监听，15 条 Ubuntu 规则 peer_ready=true，两条 Mac 规则 peer_ready=false；18:04 后 17 条规则全部 peer_ready=true。`ubuntu-ssh` 的状态仍保留一条无时间戳的 last_error 10054；本次 46 个心跳及后续只读 SSH 登录均成功，未把这一历史字段清除或据此宣称所有 Ubuntu 业务都已验收。

Windows 17:45 样本的 Mac generation 53 仍 failed；17:54:44 样本已到 generation 72，仍 failed。这一历史失败阶段能定位到 **Windows IPv4 等待正确 token 的 PUNCH 回应，以及 IPv6 本机无路由**。18:04 之后状态已恢复，历史失败不能描述为当前阻塞，也不能用恢复后的 IP 包反推此前丢包发生在哪个 NAT、出口或防火墙。

## 重新授权后的 IP 层捕获与恢复状态

18:04:52 经 UAC 启动已准备的 `capture-ip.ps1`，使用管理员 PowerShell 7.6.10 及原 RemoteSigned。collector 使用 IPv4 `SIO_RCVALL / RCVALL_IPLEVEL`，分别绑定实际以太网、vgate0、WSL 虚拟网卡 IP；没有进入 WSL、修改代理/TUN/防火墙规则或重启任何服务。

捕获标记时间为 18:04:53.104，结束为 18:05:38.108，包时间范围 18:04:53.183–18:05:38.011。三个接口共读取 32,096 个 IP 包，其中以太网 32,088，另外两接口各 4；筛选持久化 127 包（121 UDP、6 TCP），全部来自以太网，文件 20,904 字节，未触及 16 MiB 上限。三个 receive-all socket 均成功恢复 RCVALL_OFF 并关闭；stderr 为空，管理员 helper 已退出，捕获文件以只读独占打开成功。没有保留后台捕获进程。

| IP 层流向 | 包数 | 当前能够确认的范围 |
| --- | --- | --- |
| `192.168.100.169:56297` → `61.52.86.166:65243` | 6 | 与 Mac 恢复状态一致的远端流；对端端口已不同于历史候选 53542 |
| `61.52.86.166:65243` → `192.168.100.169:56297` | 7 | 反向 UDP 已到达 Windows IP 观察点；未获得本轮 token 或连接 ID 到 peer 的直接关联 |
| `192.168.100.169:56297` → `39.151.73.27:17948` | 52 | 既有 Ubuntu 路径的同窗双向流量 |
| `39.151.73.27:17948` → `192.168.100.169:56297` | 53 | Ubuntu 心跳期间保持活动 |
| `192.168.100.169:56297` → `111.206.174.2:3478` | 3 | STUN 类流量 |
| `192.168.100.169:54692` ↔ `47.101.183.194:8900` | 各 3 | 生产 SignalMessage 解码得到 18:05:23.610 Ping / 18:05:23.637 Pong |

捕获内 32 次 runtime 样本的 Mac 均为 generation 96、connected、inbound/outbound trusted，Ubuntu 均为 generation 2 双向 trusted/connected；epoch 始终 `814df6c9f93513f71ed2736e8b46e9df`。同窗 Agent 只有 primary IPv4 `0.0.0.0:56297`、primary IPv6 `[::]:56296` 及回环 UDP 53314，未见 recovery socket 残留。包证据支持当前 primary IPv4 路径活动，但没有记录 generation 96 的 QUIC winner 事件，不能据此重建当时的握手竞争顺序。

18:05:08.340–18:05:39.345，原 Windows 2222 P2P SSH 通道同一会话连续返回 Ubuntu 0–31 共 32 个心跳，退出码 0；其中前 30 个处于捕获窗口。该会话与包内 Ubuntu 双向 UDP、信令 Ping/Pong 一起证明本轮 collector 有效；它与此前全空的 PktMon 结果不同。

恢复后的只读 Mac 服务目录查询成功，返回 `rustdesk-mac`、`streamdesk-v2-hooks`；没有新增管理或业务映射。18:09:32 前完成 Windows 已有 `127.0.0.1:22118` → Mac `rustdesk-mac` 入口复核，收到 RustDesk Message.hash 帧，未登录或建立桌面会话。这是 Windows→Mac 数据面验证，不能替代下面六条 Mac→Windows 入口验收。

本窗口 **0 个完整 PUNCH、0 个 PeerCandidates、无 token 摘要**，没有新 pairing 可对齐；不能把早期 generation 6/7 的 token 或 Mac 候选填入 generation 96。仍缺成功轮次中两端候选接收、primary/recovery 的 PUNCH 往返及 QUIC 握手/认证阶段日志，Mac 侧同轮独立证据也未取得。IP 层观察不是 L2、NAT 后或 Mac 侧抓包；出站本地源端口不证明公网 NAT 端口，QUIC 形状启发式也不证明认证。当前会话已恢复并保持，历史失败根因尚不能进一步归到某一跳。

捕获后身份、admin token、node.json、tasks.json、导航 config.json/私有 HTML 与原备份的六项 SHA256 比较全部相同；revision 仍为 13，Agent 仍为唯一 PID 4460。原启动任务未再次修改。新增私有证据包括 `ip-window.pcapng`、`raw-capture-result.json`、`raw-endpoints.jsonl`、`raw-ip-detail-summary.json`、`signal-ip-analysis.json`、`raw-healthy-tcp-live.log`、`post-raw-protected-hashes.json` 和 `post-raw-cleanup-check.json`。

18:12:08.672 最后一次只读状态复核中，Mac generation 96、Ubuntu generation 2 仍双向 trusted/connected，17 条原出站规则全部 listening/peer_ready，revision 13、运行 SHA 和 epoch 均保持。该状态样本与运行历史一并保存在私有目录。

## 六条映射的升级后验收

17:33:39–17:35:20 及恢复后的 18:06:56–18:09:32 两次重新核对六项实际目标、启用状态及完整 Mac NodeId 授权，均保持原值，没有扩大授权或重建映射。下表协议结果在后一窗口再次确认。

| Mac 规则 / 监听 | Windows service_id / 实际目标 | 发布 / Mac 授权 | Windows 升级后监听及协议结果 | Mac 入口 |
| --- | --- | --- | --- | --- |
| windows-rustdesk / 22118 | rustdesk-win / `127.0.0.1:21118` | 启用 / 保留 | RustDesk PID 8012 有监听；收到紧凑帧和 Message.hash protobuf；未登录会话 | 未验收 |
| windows-wsl-llama / 18090 | wsl-llama / `127.0.0.1:18090` | 启用 / 保留 | 无监听；/health、/v1/models 在 HTTP 前连接被拒绝 | 未验收 |
| windows-streamdesk-wsl-media / 18098 | streamdesk-wsl-media / `127.0.0.1:18097` | 启用 / 保留 | 无监听；/worker/v1/capabilities 连接被拒绝 | 未验收 |
| windows-streamdesk-wsl-srs-api / 21986 | streamdesk-wsl-srs-api / `127.0.0.1:21985` | 启用 / 保留 | 无监听；/api/v1/versions、/api/v1/streams 连接被拒绝 | 未验收 |
| windows-streamdesk-wsl-srs-hls / 28099 | streamdesk-wsl-srs-hls / `127.0.0.1:28098` | 启用 / 保留 | 无监听；根路径连接被拒绝，实际 m3u8 未发现且未猜测 | 未验收 |
| windows-rdp / 13389 | windows-rdp / `127.0.0.1:3389` | 启用 / 保留 | TermService PID 1764 有 IPv4/IPv6 监听；X.224 协商响应成功，selectedProtocol 0x00000002；未执行 TLS/NLA/登录 | 未验收 |

四个 WSL 后端问题独立于 P2P 认证；没有启动或修改它们。此次升级后复核全程使用 PowerShell 7，没有进入 WSL。此前阶段 WSL 只读查询的生命周期限制保留在阶段报告中，不将那次短暂运行状态当作业务服务已修复。实际私有导航的 Mac peer 没有管理通道字段，SSH 配置没有明确 Mac 别名，当前可见 Mac 服务目录也只有两个业务服务，故不能从本机执行六条 Mac 回环入口验收。

## 受控单设备重连准备：本轮未触发

用户随后授权一次 Mac↔Windows 受控单设备重连，要求不启用 Relay、不重启其他服务，并验证 Ubuntu 健康 TCP 流保持。代码审核确认应调用现有 `/api/navigation/reconnect`，它只对指定完整 peer 执行一次 disconnect/connect；单独 Connect 已连接 peer 可能复用现有 transport，而单独 Disconnect 会留下手动断开标记，不能用于自动恢复验收。

本轮私有准备目录为上述备份目录下 `controlled-reconnect-20261116-183038`。已经准备有界 180 秒 IPv4 IP 捕获、只读 Agent event journal helper、0.5 秒事件和 1 秒状态采样、单次 210 秒 Ubuntu SSH 心跳，以及带抓包/心跳/身份/epoch/revision/无 Relay 前置检查的唯一重连请求。事件 helper 只调用现有 Agent 的 Events IPC，不启动第二个网络 owner、不初始化身份；工具均保存在用户私有目录，没有修改 Agent 程序。

管理员 PowerShell 7 的 `Start-Process -Verb RunAs` 返回“操作已被用户取消”。这是本轮 Windows UAC 的返回结果，不推断用户取消的原因。抓包没有启动，pcap/started marker 不存在；Ubuntu 专用心跳尚未启动，重连 request/result 文件也不存在，因此本轮没有向任何设备发出 disconnect/connect。

在确认 PID 和专用脚本命令行后，停止了本任务等待中的协调器；只读 observer 通过 stop marker 正常退出。observer 运行 165.091 秒、error_count=0，收尾确认两项辅助进程均不再运行。18:42:43.556 收尾状态为 Mac generation 99、Ubuntu generation 2 均 connected 且双向 trusted，唯一 Windows owner 仍为 PID 4460，epoch 和 revision 13 不变。18:34 的准备基线中 Mac 为 generation 96；这些状态变化不能归因于未发送的受控重连请求。

准备期间只读 Events preflight 查询（after=922、resync=false）另外保留了 Mac 的有序恢复记录：sequence 925 为 gen96 disconnected；927/931/933/935 为 gen97 pending/punching/authenticating/failed；938/940/942 为 gen98 punching/authenticating/failed；945/947/949/950/953 为 gen99 punching/authenticating/negotiating/authorization_pending/connected，最后双向 trusted。事件没有原始 wallclock，只能确认整批已在 18:37:10.659 前被观察；不能给每个 sequence 编造发生时间，也未取得初始断开原因或同轮 token。preflight 到 sequence 955，observer baseline 为 958，956–957 的内容未知。

18:39:58.464–18:42:43.011 的 156 个 runtime 样本中，Mac 已始终为 gen99、Ubuntu 始终为 gen2，均 connected/双向 trusted；两份事件记录没有观察到 Ubuntu 非 connected 的 PeerState。该记录证明非本次受控操作触发的 Mac 失败/恢复阶段确实存在，不等于完成了有 PUNCH 抓包和连续健康 TCP 流的本次验收。

八项文件摘要（身份、管理凭据、节点/任务配置、导航配置/页面、installed/previous-install 元数据）与本轮准备前一致，RemoteSigned 不变。没有更改代理/TUN/防火墙规则、Relay、授权或映射，也没有重启任何服务。本轮的单设备重连、同轮 token/PUNCH/QUIC 取证及连续 Ubuntu TCP 流验证均未执行，不能用收尾 connected 状态替代验收。

## 提交、私有证据与恢复

- `14ba770`：保留 Windows 原启动任务的升级适配。
- `fc13ff9`：有界私有 UDP/PUNCH 诊断及本次实际运行的干净构建。
- `6d0d3f0`：升级前策略阻碍的阶段记录，已被本报告后续结果补充。
- `448d8fd`：安装工具默认调用 PowerShell 7、不自动回退 5.1；依赖 14ba770 的任务保留流程，不需要再次重启 Agent。

七项 Windows 恢复定向测试通过，包含两种 QUIC 角色、负例、IPv6、旧 token/watch、端口释放及健康 TCP 流。插桩后受限 NAT 正负对照 2/2、私有诊断边界 3/3 通过；PowerShell 7 选择修正后的安装边界测试 12/12 通过。未运行全量测试，未推送。

私有证据和旧安装备份：

```text
C:\Users\glory\AppData\Local\p2p-file\backups\windows-recovery-20261116-veytrane
```

该目录保留 before manifest、原任务 XML、installed/previous-install 副本、候选与运行 SHA、锁释放、管理员进程路径/所有者、前后保留检查、Agent 日志及 UDP 脱敏汇总、空 PktMon 结果和停止状态、实机 SSH 心跳及本次有效 IP 捕获。六服务证据位于 `verification\service-verification-20261116-164621\post-upgrade-20261116-173339` 及 `post-connect-gen96-20261116-180656`。本机 `C:\Users\glory\.codex\INFRASTRUCTURE.md` 已按实际运行状态更新，没有写入凭据。

旧 `bin\c67b847dca54ac25730b0f59b2fc0a0b7b34e92d` 和 `bin\previous-install.json` 保留。若需回退，应先核对活动业务，在符合当前策略的管理员入口恢复原任务 Action，优雅停止现有 owner 并确认锁释放，再按旧 installed 元数据启动旧二进制；不覆盖身份、权限、配置或真实数据。旧版仍具有已知恢复缺陷。

Windows 升级与定向验证已完成，Mac↔Windows 当前双向认证已恢复，本机有效 IP 层双向 UDP 与 Windows→Mac 业务协议正控已取得。成功轮次的 token/PUNCH/QUIC 完整证据链、历史失败的精确丢包跳点及 Mac 六入口验收仍有缺口；四项 Windows 后端无监听是已确认的独立失败项。没有证据保证该修复适用于任意 NAT。
