# 独立 CLI 使用指南

本文保留当前仍由 `src/main.rs` 使用的命令与部署示例。Veytrane 的常驻节点使用 Agent 架构，首页入口见 [README](../README.md)。

## 首次连接与文件传输

本文命令属于 `src/main.rs` 的独立 CLI，使用 `p2p_file` 二进制。常驻 Agent、`p2p-node` 和原生桌面的初始化与管理流程见[节点宿主说明](../packaging/node/README.md)。两者的配置、管理接口和设备协议不可混用。

整个流程你只需要提供 **3 个值**：

| 值 | 是什么 | 怎么拿到 |
| --- | --- | --- |
| 云主机地址 | 你那台有公网 IP 的机器的 `IP:端口` | 自己知道，例如 `203.0.113.7:7000` |
| 家里的节点 ID | 家里那台机器的身份 | 在那台机器上跑 `p2p_file id` |
| 外面的节点 ID | 外面那台机器的身份 | 在那台机器上跑 `p2p_file id` |

下面每台机器的命令块，开头都把需要的值导出成变量，**填一次，后面的命令整段复制粘贴即可**。
文中出现的 `203.0.113.7` 和那两个节点 ID 都是示例值。

### 编译

使用 rustup stable，rustc/Cargo 最低 1.90.0（现代 macOS 通知后端要求）。

```bash
cargo build --release
export BIN=$PWD/target/release/p2p_file
```

把 `$BIN` 拷到要用的机器上（`scp` 就行）：

```bash
scp target/release/p2p_file root@203.0.113.7:/usr/local/bin/p2p_file
```

`203.0.113.7` 就是你云主机的公网 IP，全文的示例都用它。

### 第 1 步：云主机 —— 跑信令服务器

```bash
export SIGNAL=0.0.0.0:7000
$BIN signal-server --listen $SIGNAL
```

直连模式在**云厂商的安全组**里放通 TCP 7000；启用 Relay 时还需放通对应 UDP 端口。

以上命令仅启用 TCP 信令；Direct 成功时云主机不承载业务数据。若启用可选 Relay UDP
服务，则需按实际转发流量准备带宽；Relay 只转发端到端加密的 QUIC datagram，
看不到业务明文。开机自启见[部署](#部署为-systemd-服务)。

### 第 2 步：两台机器各拿一次身份

在**家里那台**和**外面那台**上分别跑：

```bash
$BIN id
```

输出形如：

```
节点 ID:  a1b2c3d4e5f60718293a4b5c6d7e8f90
公钥:     3b99296d64377f91605631f89feb53aec6e432dd8b0a0e15a59267de106b04f9
密钥文件: /home/you/.config/p2p_file/identity.key
```

要的就是 `节点 ID:` 后面那串 **32 位十六进制**。它是公钥摘要，可以随便公开。
私钥自动生成在 `~/.config/p2p_file/identity.key`（权限 `0600`），**别外传**。
两台机器的 ID 记下来，第 4、5 步要用。

### 第 3 步：先确认打洞打得通

```bash
$BIN stun
```

输出形如：

```
本地端口 59744，向 3 个 STUN 服务器查询 ...

  111.206.174.2:3478           看到的是 39.151.73.136:10281
  74.125.250.129:19302         看到的是 39.151.73.136:10282
  162.159.207.0:3478           看到的是 39.151.73.136:10282

NAT 映射行为: 未知（证据不足）
mapping 证据: 证据不足（服务器不支持 RFC 5780 行为发现或备用探测失败）
Filtering behavior: 未测量（当前只测 mapping）
证据不足时不能宣称 NAT 可以打洞。
```

程序会优先使用支持 RFC 5780 `OTHER-ADDRESS` 的 STUN 服务器，对同一个 socket
按 RFC 5780 §4.3 先探测 primary IP+port，再探测 alternate IP+primary port；只有
Test II 映射与 Test I 不同，才继续探测 alternate IP+alternate port。只有 RFC 5780
行为发现证据充分时，才会区分 EIM / ADM / APDM；跨 IP 且每个 IP 只有一个样本时会保持未知。
当前只测 mapping，不测 filtering，因此任何 mapping 结果都不能单独等同于最终 punchability。

如果输出是 `地址端口相关/对称型（mapping 对打洞不利）`，请直接跳到
[打不通的时候](#打不通的时候)第 3 条做端口映射。

### 第 4 步：家里那台 —— 常驻等着被连

```bash
export SIGNAL=203.0.113.7:7000        # 换成你云主机的公网地址
export LAPTOP_ID=0f1e2d3c4b5a69788796a5b4c3d2e1f0    # 换成"外面那台"的节点 ID

$BIN serve \
  --signal $SIGNAL \
  --allow $LAPTOP_ID \
  --forward 127.0.0.1:22 \
  --recv-dir ~/下载 \
  --port 9000
```

这条命令的含义：

- `--allow` —— 只让这一个节点连进来，别人不行。
- `--forward 127.0.0.1:22` —— 允许对端转发到本机的 ssh。可以重复指定多个。
- `--recv-dir` —— 对端推过来的文件落在这里。目录不存在会自动创建。
- `--port 9000` —— 打洞用的本地端口。

它会**一直等**着对端上线（不超时），空闲一段时间后自动重新打洞，
所以开着不用管。要做成开机自启见[部署](#部署为-systemd-服务)。

### 第 5 步：外面那台 —— 开隧道或推文件

```bash
export SIGNAL=203.0.113.7:7000        # 换成你云主机的公网地址
export HOME_ID=a1b2c3d4e5f60718293a4b5c6d7e8f90       # 换成"家里那台"的节点 ID
```

**开隧道**（把本机 2222 接到家里的 22）：

```bash
$BIN tunnel \
  --signal $SIGNAL \
  --peer $HOME_ID \
  --listen 127.0.0.1:2222 \
  --to 127.0.0.1:22

# 另开一个终端：
ssh -p 2222 127.0.0.1
```

（`ssh` 不带用户名时会用你当前的用户名。家里那台的登录名不一样的话，
写成 `ssh -p 2222 那边的用户名@127.0.0.1`。）

`--to` 指定的地址必须在家里那台的 `--forward` 白名单里，否则会被拒绝。

**推文件**（落到家里的 `--recv-dir`）：

```bash
$BIN push --signal $SIGNAL --peer $HOME_ID ./大文件.mkv
```

### P2P / QUIC 纯网络测速

独立测速 CLI 默认运行 10 秒，`--duration` 支持 1–600 秒。它测量设备通道吞吐；文件传输速度还受磁盘、文件大小和校验开销影响。

想把网络链路和文件传输本身区分开时，在外面那台机器上运行：

```bash
$BIN speedtest \
  --signal $SIGNAL \
  --peer $HOME_ID
```

也可以指定时长、方向和固定内存 block：

```bash
$BIN speedtest \
  --signal $SIGNAL \
  --peer $HOME_ID \
  --duration 15 \
  --direction upload \
  --block-size 1048576
```

`speedtest` 是经过现有信令、STUN/候选、UDP 打洞、QUIC 和 Ed25519 身份认证后的
内存到内存测速。它不读写磁盘，不计算文件 hash，不使用 manifest、bitmap、fsync
或文件分片协议；因此它不是 ISP 官方测速，而是用来和 `push` 速度做对照。
`--direction both` 会先 upload、再 download，不做同时双向测速。

- `speedtest` 快、`push` 慢：优先检查文件协议、磁盘和 fsync。
- `speedtest` 慢、`push` 慢：优先检查 QUIC、UDP、RTT、丢包、流控或 ISP。
- `speedtest` 快、`push` 快：当前链路和文件路径都正常。

## 动态转发规则（CLI）

需要长期访问多个目标时，不必把 `--forward` / `--listen` 写死在启动参数里：

```bash
# Ubuntu 常驻服务：授权与目标白名单放在 JSON 里
$BIN serve --signal 203.0.113.7:7000 --config /etc/p2p_file/service.json

# macOS/Linux 客户端：一个进程、多条本机转发、共享同一认证会话
$BIN forward --signal 203.0.113.7:7000 --config ~/.config/p2p_file/client.json
```

修改配置后发送 `SIGHUP` 即热加载；非法配置或端口占用会保留原规则并报告错误。
不启动网络可用 `$BIN rules <FILE>` 校验并查看规则。格式、撤销语义、重连修复和
验收边界见 [CLI 动态 TCP 转发与共享会话](CLI_DYNAMIC_TUNNEL.md)，
示例见 [service.json](examples/service.json) 与 [client.json](examples/client.json)。

### 本机 Web 管理页

不想手改 JSON 时，可以只监听 loopback 打开管理页：

```bash
$BIN serve --signal 203.0.113.7:7000 --config /etc/p2p_file/service.json \
  --admin-listen 127.0.0.1:7788
$BIN forward --signal 203.0.113.7:7000 --config ~/.config/p2p_file/client.json \
  --admin-listen 127.0.0.1:7789
```

终端会打印管理 token 文件；浏览器打开 `http://127.0.0.1:7788/` 后填入 token。
远程访问请用 SSH 隧道。安全边界、API 和限制见 [Web 管理页](WEB_ADMIN.md)。

## 部署为 systemd 服务

两个服务都用**用户级 unit**，这样路径里不用写死用户名。

### 云主机上的信令服务器

```ini
# /etc/systemd/system/p2p-signal.service
[Unit]
Description=p2p_file 信令服务（可选端到端加密 UDP Relay 兜底）
After=network-online.target

[Service]
ExecStart=/usr/local/bin/p2p_file signal-server --listen 0.0.0.0:7000
Restart=always
RestartSec=3
StandardOutput=journal

[Install]
WantedBy=multi-user.target
```

该示例默认只启用 TCP 信令。启用 Relay 时按下文“可选认证 UDP Relay 兜底”添加
`--relay-listen` 并放通对应 UDP 端口；Direct 优先，Relay 按需转发加密 QUIC datagram。

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now p2p-signal
journalctl -u p2p-signal -f
```

### 家里那台的常驻服务

```ini
# ~/.config/systemd/user/p2p-serve.service
[Unit]
Description=p2p_file 常驻服务（等着被连）

[Service]
ExecStart=%h/bin/p2p_file serve \
  --signal 203.0.113.7:7000 \
  --allow 0f1e2d3c4b5a69788796a5b4c3d2e1f0 \
  --forward 127.0.0.1:22 \
  --recv-dir %h/下载 \
  --port 9000
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

`%h` 会自动展开成当前用户的家目录，所以路径不用改。启用：

```bash
systemctl --user daemon-reload
systemctl --user enable --now p2p-serve
# 让它在你不登录的时候也活着（关键，否则注销就停了）
loginctl enable-linger $USER
journalctl --user -u p2p-serve -f
```

程序同时处理 `SIGINT`（Ctrl-C）和 `SIGTERM`（`systemctl stop` 发的就是它），
停止时会优雅关闭 QUIC 连接，对端立刻就能察觉，不用干等超时。

## 打不通的时候

按顺序排查：

1. **先看 NAT mapping**：`$BIN stun`。只有报告明确显示 RFC 5780 行为发现证据充分时，
   分类结果才有判别力；filtering 未测量，不能把 mapping 结果当成最终“可打洞”结论。

2. **确认两边都在跑**。打洞是「同时开启」——只有一边发包是打不通的。
   `serve` 那台常驻会自动重新打洞，不用管。

3. **有公网入站条件时手工映射端口**。在路由器上把 UDP 9000 映射到家里那台机器，
   然后给它加 `--advertise`：

   ```bash
   --advertise 203.0.113.7:9000      # 填路由器上映射出来的那个「公网 IP:端口」
   ```

   需要路由器具备可达的公网地址并放行 UDP；运营商 CGNAT、上游防火墙或另一端网络限制仍可能阻止连接。

4. **检查双方 UDP 与地址族可达性**。本地监听端口可以不同，不要求两端使用相同端口。
   无法建立直连时，可按下节配置可选 Relay；双方仍需能够访问中继的 UDP 端口。

日志是排查的主要依据，加 `--log info` 能看到打洞过程：

```bash
$BIN --log info tunnel --signal $SIGNAL --peer $HOME_ID --listen 127.0.0.1:2222 --to 127.0.0.1:22
```

## 可选认证 UDP Relay 兜底

双方使用同一 signaling 服务签发的 pairing，配置其 Relay UDP 地址：

```bash
# 自建服务器：TCP 信令 + 固定 UDP Relay listener
p2p_file signal-server --listen 0.0.0.0:7000 --relay-listen 0.0.0.0:7001
# 可再加一个原生 IPv6 listener；不用 IPv4-mapped socket
# --relay-listen '[::]:7001'

# 双方在现有 serve / tunnel / push / speedtest 命令中加：
# --relay relay.example.com:7001
p2p_file serve --signal signal.example.com:7000 --relay relay.example.com:7001 \
  --allow <PEER_NODE_ID> --forward 127.0.0.1:22
p2p_file tunnel --signal signal.example.com:7000 --relay relay.example.com:7001 \
  --peer <SERVER_NODE_ID> --listen 127.0.0.1:2222 --to 127.0.0.1:22
```

云安全组与主机防火墙放通 **TCP 7000 + UDP 7001**（端口可自选）。
Relay 需要实际转发带宽；直连成功时仍不走服务器。以上参数用于独立 CLI。
Agent / 桌面的 Relay 配置通过节点设置管理，见[节点宿主说明](../packaging/node/README.md)。

连接顺序：原生 IPv6 / IPv4 直连竞速 → 2.5 秒无 authenticated winner → 延迟解析并
尝试 Relay。未配置时没有 Relay DNS、UDP socket 或 admission 请求。每个 peer/地址族
attempt 使用独立 socket，challenge-response + Ed25519 admission 后，收到 RelayReady
才将**同一个 socket**交给 Quinn。RelayReady 只是 UDP 就绪，身份仍需真实 NodeId、
双边 Ed25519 签名、当前 TLS exporter binding 和 Ready；仅一个 winner 提交 FIN。

Relay ticket 只来自真实在线节点 pairing，有 TTL、并发数量与单 IP pending 上限。
普通 NodeId Lookup 另有固定 10 秒窗口：默认每 signaling connection 64 次、每 source IP
256 次；IP bucket 最多 1024 项并清理过期项。超限在 pairing 前返回统一错误，不签发
ticket、不向目标推送候选，也不新增 offline waiter；`max_pending_lookups` 仍只限制
真实挂起的离线查询。少量重复查询、重新连接和直连模式继续使用原有协议与 CLI。
Admission 的 outstanding ticket 默认每 unordered NodeId pair 最多 16 张、每 NodeId
最多 64 张；小容量服务还将两者分别限制在全局 ticket 池的一半。quota 直接依据真实
ticket map，TTL 到期自动释放；已 Ready 的转发 session 仍按原有 idle timeout 运行。
Session 另有独立配额：每 unordered NodeId pair 最多 16 个、每 NodeId 最多 64 个，
小容量服务将其限制为全局 session 池的一半（单槽服务至少允许一个）。每个 arm
无论 half-bound 或 Ready 均计入，按已签名 Bound 的 node/peer 从真实 session map
统计；创建前及签名 Register 后都检查。ticket 过期或普通 UDP 保活均不能释放
session 配额；idle 清理自动释放。已有 half-bound arm 可在配额满时完成另一端绑定。
第三方 source、未知 ticket、错 pair/公钥/签名与重放均拒绝。两边绑定前丢弃数据，
不排队；绑定后的 source 不能漂移或抢占，断线重新 pairing。Relay 不解码 QUIC 或
文件/Tunnel/Speedtest，不解密、不落盘；恶意服务器可以阻断，不能冒充设备。
Remote Password / Trusted Device / 单向授权 / Tunnel allowlist 语义均保持原样，
新 transport 的授权必须重新绑定，Relay 不自动建立信任，也不保存对端密码。

运行时区分 `IPv6 Direct`、`IPv4 Direct`、`Relay IPv6`、`Relay IPv4`，日志包含
`path_kind`、family、remote。GPUI 依次显示 fallback、UDP 就绪、最终认证路径。

`./scripts/e2e.sh` 依次测试 IPv4、IPv6 和确定性的本地 Relay fixture；后者公布实际
被持有且不回应的 blackhole candidates（`--advertise-only` 诊断选项），验证真实
pairing → admission → 端到端 QUIC → 1MB Tunnel / 多连接 / 文件 hash / Speedtest / 重连。
原生三平台 loopback 测试不依赖公网；跨 NAT 的真实部署可达性仍需实际网络验收。

## IPv6 原生直连与双栈竞速

Desktop 和 CLI `serve/tunnel/push/speedtest` 默认分别建立 IPv4、IPv6 UDP path。
IPv6 socket 在绑定前设置 `IPV6_V6ONLY`，不用 IPv4-mapped 地址。每族自己的 STUN、
PunchToken 探测和 QUIC 使用同一个 socket；固定端口按族各绑定一次，`--port 0`
时各自公告自己的实际随机端口。某一族绑定失败不会禁用另一族。

IPv6 优先启动，IPv4 最多延后 200ms；候选并发、有数量和时间上限。竞速覆盖 QUIC、
Ed25519 签名、TLS exporter binding 和 `Ready`，只在 winner 的专用身份流上发送 FIN。
接收方确认 FIN 后才协商 Desktop 能力、Remote Auth 或启动 CLI 业务 handler；loser
连接和探测注册会被回收。IPv6 QUIC 或 Ready 阶段黑洞都不会等完 IPv6 超时才尝试 IPv4。

IPv6 global / ULA 是 Host 候选；IPv6 STUN Binding 只是补充地址观测，失败时仍保留
Host。IPv4 的 RFC 5780 mapping 分类只使用 IPv4 样本。链路本地 scope-id、IPv4-mapped、
未指定地址、多播地址不参与 P2P 候选。信令 TCP 可以走 IPv4，同时业务走 IPv6。

```bash
# 公网 IPv6 主机的手动公告；serve 与其余公网命令使用相同参数
p2p_file serve --signal signal.example.com:7000 --allow <PEER_NODE_ID> \
  --port 9000 --advertise '[YOUR_GLOBAL_IPV6]:9000' --ip-family ipv6-only

# LAN send/recv 按指定地址族绑定
p2p_file recv --listen '[::]:9000' --out-dir ./received
p2p_file send ./file.bin '[PEER_IPV6]:9000'
```

`--ip-family dual-stack` 为默认；排障时可用 `ipv4-only` 或 `ipv6-only`。`stun` 命令
分别报告两族 socket、Host、STUN observation，IPv6 不显示为 NAT EIM/ADM/APDM。
GPUI 网络卡片显示本地 path 是否可用、peer 两族候选数量和最终已认证的 family/address。
IPv6 地址只决定 transport，不代替 NodeId 身份、远程密码、单向 Trusted Device 或
Tunnel 的 peer/target allowlist。

IPv6 仍需要操作系统、路由器和云安全组允许相应 UDP 端口。没有 global IPv6 的网络可
通过 IPv4 path 工作；跨网络公网 IPv6 是否可达需要两台真实主机验证。自动化覆盖原生
IPv6 loopback 和可控黑洞，不代表已实测公网 IPv6 穿透。可选认证 UDP Relay 兜底见上节；
未实现完整 TURN/ICE、TCP/TLS Relay、UPnP、NAT-PMP、PCP、link-local scope-id、Multipath QUIC。

## 局域网直连（不经过信令服务器）

两台机器在同一个局域网里时，不需要云主机：

```bash
# 接收方
$BIN recv --listen 0.0.0.0:9000 --out-dir ./downloads

# 发送方（先用 discover 找到对端地址）
$BIN discover --timeout 5
$BIN send ./大文件.mkv 192.168.1.7:9000
```
