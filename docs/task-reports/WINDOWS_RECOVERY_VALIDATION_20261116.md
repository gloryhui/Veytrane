# Windows 恢复路径验证与升级阻碍记录

核验日期：2026-11-16，Asia/Shanghai，最终只读核验约 17:10。本文记录实际完成项和未完成项，不包含凭据、原始打洞 token 或完整私有配置。

## 当前结论

Windows 定向验证、启动适配、短时诊断功能和发布构建已完成；**已安装 Agent 尚未升级，也未发生 Agent 停机**。管理员 Windows PowerShell 在加载升级脚本时被现有执行策略拒绝；后续仅用于记录启动错误的 UAC 请求被用户取消。没有更改执行策略、改用其他执行方式绕过拒绝或继续发起提权请求。

因此不能把本次构建、测试或提交当作 Windows 已运行 `b981f07` 修复的证明。Mac↔Windows 仍未认证成功，尚未取得同轮 token/generation 对齐的双向 UDP 现场证据。

## 代码与验证

工作区和暂存区初始均为空。`origin/main` fetch 后从 `c67b847` 安全快进到 `03e5e76`，包含 `b981f07`、`80675ab` 和 Windows 导航合并；没有重做已有 NAT 修复。

| 阶段 | 提交 | 内容与依赖 |
| --- | --- | --- |
| Windows 启动适配 | `14ba770` | 识别旧可执行文件和 data_dir 对应的唯一任务；在旧 owner 停机前实际更新并核验任务。保留 principal、凭据方式、triggers、settings、参数及其他 actions。提前保存回滚元数据，不创建第二个启动任务。 |
| 私有短时诊断 | `fc13ff9` | 默认关闭的 UDP/PUNCH JSONL，默认 90 秒，上限 4096 条/1 MiB；记录 token SHA256 前 12 位、完整 NodeId、generation、watch、socket、候选及 QUIC 阶段/获胜路由。部署时依赖 `14ba770` 保留原启动任务。 |

所有提交保留现有 Git Author/Committer，并含唯一 Codex trailer；本会话没有推送授权，未推送。

实际通过的定向测试：

| 验证项 | Windows 结果 |
| --- | --- |
| `port_restricted_recovery_races_primary_for_both_quic_roles` | 通过，两种 QUIC 发起角色 |
| `port_restricted_recovery_only_cannot_pass_for_either_quic_role` | 通过，禁用 primary 主动路径后两种角色均按预期失败 |
| `simultaneous_private_socket_recovery_uses_primary_probe_bridges` | 通过，IPv4、IPv6 均实际执行 |
| `clearing_reconnect_probe_releases_old_token_for_same_source` | 通过 |
| `expired_pending_lookup_releases_its_private_socket` | 通过 |
| `ipv6_quic_completes_when_peer_never_answers_punch_probes` | 通过，没有 IPv6 skip |
| `failed_peer_automatically_recovers_without_breaking_another_peers_tcp_stream` | 通过，两种角色，测试本身 187.72 秒；健康连接标识保持、未关闭，TCP 心跳超过 20 次 |
| 安装边界测试 | 10/10，通过；包括更新失败不停止旧 owner |
| 私有诊断边界测试 | 3/3，通过；deadline/记录数/字节上限、摘要和 JSONL/新文件边界 |
| 插桩后受限 NAT 正负对照 | 再次通过 2/2 |

执行的是报告中的 `cargo test --locked --lib --features node <filter> -- --nocapture`，另执行诊断模块定向测试、`cargo check --locked --bin p2p-agent`、Python 安装边界测试及语法/diff 检查。没有运行全量测试，没有删除断言、跳过关键场景或修改恢复超时。

## 构建、运行与保留情况

本机架构 AMD64；Rust/Cargo 1.98.1，`x86_64-pc-windows-msvc`；检测到 Visual Studio 2022 BuildTools。构建命令：

```text
cargo build --locked --release --bin p2p-agent --bin p2p-node
```

构建成功，没有构建 GUI：

| 对象 | 实际结果 |
| --- | --- |
| 候选版本 | `0.1.0`，`fc13ff9ec92db112bf71d6ad8e428c9f459b52db`，source_state=clean |
| 候选 Agent SHA256 | `04d3262bc779d37b303948d5445720c62a5bc508e1b4fbab6a48b04939458e81` |
| 候选 CLI SHA256 | `d394760b3d30eb290df3dda381bfa134aafd559938bc7a78274eb2793186a2d6` |
| IPC 查询到的实际运行 SHA | `c67b847dca54ac25730b0f59b2fc0a0b7b34e92d`，source_state=clean |
| Windows NodeId | `5eef2d53912e77479fa2e61ef3aa27c3`，与指定身份一致 |
| 配置 revision | `13`，不变 |
| 已发布服务/出站映射/可信设备 | 6 / 17 / 2；node.json 字节摘要不变，服务授权和映射未改 |
| 启动入口 | `\Veytrane-Agent-Boot`；AtBoot、30 秒 delay、账户 glory、Password、Limited、IgnoreNew、10 次/1 分钟重试 |
| 任务定义 | Actions、Principal、Triggers、Settings 前后 XML 一致 |
| 进程 | 原 PID `16072` 仍持有既有 UDP/TCP 端点；非提权 CIM 对 ExecutablePath/CommandLine/GetOwner 返回访问拒绝，不能声称已直接核实操作系统进程所有者或加载路径 |

安装元数据和任务 Action 均指向：

```text
C:\Users\glory\AppData\Local\p2p-file\bin\c67b847dca54ac25730b0f59b2fc0a0b7b34e92d\p2p-agent.exe
--data-dir "C:\Users\glory\AppData\Local\p2p-file\node"
```

新版本仅准备到独立的 `bin\fc13ff9ec92db112bf71d6ad8e428c9f459b52db`，没有切换 installed.json 或原任务。身份、admin.token、node.json、tasks.json、导航 config.json/私有 HTML 和 installed.json 摘要前后一致；路由表和防火墙 profile 快照一致。没有写入代理/TUN/防火墙规则、服务授权或映射，没有初始化新身份、启动第二个 Agent owner、启用 Relay。

## Mac↔Windows 现场证据及缺口

最终 Windows IPC 样本的 Mac generation 为 `442`、phase=`failed`；观察期间反复进入 `authenticating` 后失败。该样本的诊断为 IPv6 候选 QUIC `closed`，以及 IPv4 打洞超时：“未收到带正确令牌的探测回应（4 个候选）”。这些诊断不能区分网卡之前丢包、进入 Windows 后过滤、mux token/generation 拒绝或旧 watch 问题。

重新读取到的 Windows 候选包括：

```text
127.0.0.1:59895
172.30.253.231:59895
192.168.100.169:59895
222.137.31.236:12274
[::1]:59894
```

PID `16072` 的 UDP 端点中存在 IPv4 `59895` 和 IPv6 `59894`，与 primary 候选端口相符。其他动态 UDP 端点仅记录在私有快照中；没有把未标注端点直接认定为 Mac recovery socket。当前日志仍引用 Mac IPv6 `:61972`，但缺少配对 token 摘要，不能证明属于同轮新候选交换，不能和旧报告中的 token 强行对齐。

本轮没有生成有效的 Agent 短时日志或 packet capture：

- 普通会话无法取得底层包监视状态/权限。
- Windows PowerShell 事件 `4100`、`UnauthorizedAccess` 明确拒绝加载 `run-upgrade.ps1`，故未进入包捕获、任务更新或停机步骤。
- 没有获知新的同轮 pairing token、primary/recovery 实际出站源端口、Mac PUNCH 的 Windows 网卡入站/Agent 入站或回应 socket。
- 没有可用的已配置 Mac 独立只读管理通道；未把 Mac 的 localhost 入口用于 Windows，也未借其他节点目录为空推断 Windows 服务不存在。
- 无法证明 Mac 收到了哪一轮 Windows 候选；没有用 Ubuntu 可通推断 Mac 路径可通。

已准备的诊断可在获准升级后对齐 watch 注册/释放、peer generation、token 摘要、两类 socket 的发送和入站匹配结果。取证前仍须重新采样候选和端口，不得固定本报告地址。

Ubuntu 只读复核通过 Windows 既有 SSH/P2P 入口 `glory@127.0.0.1:2222`，启用 StrictHostKeyChecking、BatchMode，未修改 known_hosts。Ubuntu 身份 `cbd82de8be77d5f813fc3ff92663b2ac`、revision `26`、运行 SHA `b981f07`；其 Windows peer generation `2`、Mac peer generation `5` 均 connected 且双向 trusted。Windows 的 Ubuntu peer generation `217`、connected。没有部署或重启 Mac、远端 Ubuntu、信令服务器或其他业务服务。

## 六项 Windows 服务

以下六项实际配置均 `enabled=true`，且 `allowed_peers` 均精确包含完整 Mac NodeId `ae4c49f3d120f1e013155039eb3263ce`。没有扩大授权或修改任何条目。Mac 六个入口全部未做端到端验收。

| Mac 规则/端口 | Windows service_id / 实际目标 | Windows 监听与协议验证 | Mac 入口 |
| --- | --- | --- | --- |
| `windows-rustdesk` / 22118 | `rustdesk-win` / `127.0.0.1:21118` | RustDesk PID 8012，`::` 监听；收到紧凑帧和 `Message.hash` protobuf。协议响应成立，未进行远程桌面会话/登录验收 | 未验收 |
| `windows-wsl-llama` / 18090 | `wsl-llama` / `127.0.0.1:18090` | Windows/所查 WSL 运行态均无监听；`/health`、`/v1/models` 在 HTTP 前被拒绝 | 未验收 |
| `windows-streamdesk-wsl-media` / 18098 | `streamdesk-wsl-media` / `127.0.0.1:18097` | 无后端监听；`/worker/v1/capabilities` 连接被拒绝 | 未验收 |
| `windows-streamdesk-wsl-srs-api` / 21986 | `streamdesk-wsl-srs-api` / `127.0.0.1:21985` | 无后端监听；`/api/v1/versions`、`/api/v1/streams` 连接被拒绝 | 未验收 |
| `windows-streamdesk-wsl-srs-hls` / 28099 | `streamdesk-wsl-srs-hls` / `127.0.0.1:28098` | 无后端监听；根路径连接被拒绝，未发现可验证的实际 m3u8，不猜测流名称 | 未验收 |
| `windows-rdp` / 13389 | `windows-rdp` / `127.0.0.1:3389` | TermService PID 1764，IPv4/IPv6 监听；X.224 Connection Confirm 和协商响应有效，selectedProtocol=`0x00000002`（MS-RDPBCGR 的 PROTOCOL_HYBRID/CredSSP）。未执行 TLS/NLA/登录 | 未验收 |

WSL 核验有生命周期限制：只读查询中曾调用本机 Ubuntu 发行版，观察到 uptime 很短的 WSL 运行态；辅助命令也曾误选 bash 进入 WSL。未运行相关业务服务、未执行 shutdown/terminate/restart，最终运行发行版列表为零。因此这里只能确认当前四个目标不可达，不能宣称其历史常驻服务已完整核实或已修复。

即使 Mac↔Windows 后续恢复认证，这四个无监听后端仍会阻碍业务访问。没有为本次验证启动、部署或改写它们。

## 私有证据、回滚和继续条件

主备份和部署准备目录（保护继承，仅当前用户 FullControl）：

```text
C:\Users\glory\AppData\Local\p2p-file\backups\windows-recovery-20261116-veytrane
```

其中包含身份/配置/授权/任务/导航副本、文件摘要、原启动任务 XML、安装及 previous-install 元数据副本、构建日志和 SHA、部署候选、升级脚本、回滚辅助脚本、最终状态和执行策略拒绝事件。没有将这些私有内容提交。

定向测试日志：

```text
C:\Users\glory\AppData\Local\p2p_file\codex-logs\peer-recovery-tests-20261116-fc3c1aaf7fba4a618deb59d0a78d3b54
```

服务协议证据：

```text
C:\Users\glory\AppData\Local\p2p-file\verification\service-verification-20261116-164621
```

当前未升级，故无需回滚运行版本。以后如完成升级而需回退，应先核对活动业务及身份，通过符合执行策略的管理员入口恢复原任务 Action，优雅停止当前 owner 并确认目录/身份/任务锁释放，再依据备份 installed.json 恢复原版本启动；不覆盖节点数据，不因回退代码恢复旧权限配置。旧版会重新具有已知恢复路径缺陷。

继续条件是用户确认符合本机既有执行策略的管理员执行入口。获准后先重新核对本地 SHA、配置摘要与任务定义，再执行已准备的升级、短时包捕获和日志对齐；不得以脚本受阻为由修改执行策略或绕过既有拒绝。完成 Windows 升级和同轮 UDP 取证之前，本任务不能标记为验收完成。
