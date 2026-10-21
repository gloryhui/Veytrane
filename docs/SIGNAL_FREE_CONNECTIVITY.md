# Agent 局域网发现与 IP 直连

通用 Agent 支持三种地址来源：局域网 mDNS、手动/已保存的 IP 地址，以及可选信令。三者进入同一个 P2PN v2 会话与资源授权系统，不创建第二套身份、任务库或文件/TCP 实现。独立 Sub2API 产品保持其固定信令与权限边界，不启用这项通用节点发现功能。

## 使用条件

- 同一局域网：允许 mDNS UDP 5353 组播和 Agent 的实际 UDP 监听端口。访客网络、VLAN 隔离或组播过滤会妨碍自动发现，可改用可达 IP。
- 跨网 IP 直连：知道对端完整 NodeId、当前可达 IPv4 / IPv6 和 UDP 端口。IPv6 必须有可用路由，不能使用无 scope 支持的链路本地地址。
- 公网地址在路由器上：将公开的 UDP 端口映射到 Agent 的实际 UDP 端口。这里的端口不是 Web 管理端口、信令 TCP 端口或应用服务端口。
- 两端都没有可达地址：仍需信令协调 NAT 穿透；本功能不提供无协调的跨 CGNAT 自动发现，也不能保证穿透任意防火墙。

完整 NodeId 用于固定预期身份。地址是可变的定位信息，不能替代身份。九位短 ID 仍需要信令查找。

## 首次启动与连接

以下示例用于新建测试/使用节点。已有节点沿用原数据目录，不要删除身份或重新初始化。首次构建/初始化见 [README](../README.md#快速开始)。Agent、CLI、桌面均从同一源码版本构建。

```sh
# 两端各运行一次，已有配置时无需重复
./target/release/p2p-node init

# 终端一：两端分别运行
./target/release/p2p-agent
```

两端先设置自己的远程访问密码。桌面设置可交互完成；无桌面时，从本机 `config` 响应取得当前 revision 后，通过标准输入设置。下面的 Python 辅助仅用于隐藏输入，不保存密码、不把密码放进进程参数：

```sh
# 新初始化且尚未提交其他变更时 revision 为 0；否则替换为实际 revision
python3 -c 'import getpass,sys;sys.stdout.write(getpass.getpass("节点远程密码："))' \
  | ./target/release/p2p-node password --revision 0

# 终端二：查询本机实际监听地址及附近设备
./target/release/p2p-node nearby
```

`nearby` 返回 `enabled`、`discovery`、`listen` 和 `nearby`。发现状态为 `active` 时表示浏览器工作，不表示任何设备已获授权或实时在线；`unavailable` 时 IP 直连仍可用。启动后可能需要数秒才能收到公告。候选中的回环地址只供同机测试，不能复制到另一台设备作为远端地址。

局域网连接：在桌面“节点管理”或 Web 中选择附近设备，核对完整 ID，输入远程密码并连接。也可以使用已发现的完整 NodeId 执行普通 `connect`：

```sh
./target/release/p2p-node connect <REMOTE_NODE_ID> --password-stdin
```

已知 IP 连接：

```sh
./target/release/p2p-node connect-address <REMOTE_NODE_ID> 192.168.1.10:9000 --password-stdin
./target/release/p2p-node connect-address <REMOTE_NODE_ID> '[2001:db8::10]:9000' --password-stdin
```

上述地址为示例，不是现有部署地址。`--password-stdin` 从标准输入读到 EOF；也可通过上面的隐藏输入辅助传入。对端已经信任本机时可省略该参数，继续使用原可信设备认证。单次调用可传多个地址，Agent 会尝试同地址族端点并选择已认证的连接。

连接后，仍须分别配置允许访问的设备、服务目录以及文件/测速授权。后续 `services`、`send`、任务恢复和端口映射与信令模式相同。发现某设备不代表它已被信任；选择设备也不会自动连接或保存授权。

## 持久配置与端口

`NodeSettings.direct` 的默认值为：

```json
{
  "enabled": true,
  "lan_discovery": true,
  "listen_port": 0,
  "peers": {}
}
```

这是配置片段，不能单独作为完整配置提交。

| 字段 | 含义 |
| --- | --- |
| `enabled` | 是否启用通用节点无信令直连入站和地址建连。 |
| `lan_discovery` | 是否公告并浏览 `_veytrane._udp.local.` 服务。关闭它仍可手动 IP 直连。 |
| `listen_port` | Agent UDP 端口；0 表示操作系统分配，IPv4 / IPv6 可能不同。固定值可用于路由器转发与公网入口。 |
| `peers` | 完整 NodeId 到 1～8 个 `IP:端口` 的映射，最多 128 个设备；只保存地址，不授予权限。 |

`enabled`、`lan_discovery`、`listen_port` 涉及监听与发现生命周期，需要停止 Agent 后离线应用，或走明确的保存并停止流程。变更前备份，沿用当前 revision，不直接编辑正在运行节点的配置。现有接口见[宿主说明](../packaging/node/README.md)。

`direct.peers` 可以通过原配置事务在线更新，不停止现有连接。Web 的“保存直连地址到草稿”仅修改草稿，还须点击保存配置才能持久化。普通 `connect-address` 只缓存当前 Agent 会话的地址；重启后需重新发现/输入，或提前保存到 `direct.peers`。地址优先级为已保存/本次手动地址，再到局域网发现；地址变化后需重新发现或更新保存值。现有服务监听按需建立连接，保存地址不会自动发起网络访问。

无信令模式将 `signal_server` 留空：此时不注册信令、不查询公共 STUN、不使用 Relay。`status.node.signal` 为 `disabled`，表示主动使用本地直连模式，不代表 Agent 停止。未连接时也可以使用附近设备列表。

## 身份、授权与资源边界

- mDNS 使用独立的节点 v2 命名空间，携带完整 NodeId、版本与各地址族 UDP 端口。公告可被同网段设备伪造，只当作地址提示。
- 外连必须验证预期 NodeId、Ed25519 签名和当前 TLS exporter 绑定；错误地址或伪造公告不能冒充预期设备。
- 未由信令建立路由的入站先通过 QUIC Retry 校验回程可达性，再在有界并发和超时内验证设备身份。
- 所有直连仍经过原 P2PN v2 能力协商、远程密码/可信设备证明、方向权限和持续授权门禁，之后才发布业务连接。
- 新路径不会将来访设备写入可信列表，不修改现有私钥。禁用设备、撤销授权和服务白名单继续生效。
- 发现缓存最多 64 条、单设备最多 8 个地址，过期及 goodbye 移除旧提示；握手并发为 8，身份阶段 10 秒、直连及后续认证总阶段有界。

## 验证与部署范围

核心回归覆盖真实 IPv4 / IPv6 QUIC、调用端两种应用角色、错误预期身份、错误密码和有效重试、并发连接及无信令时不探测外部服务。Agent/CLI 进程回归覆盖实际 mDNS 发现、显式连接、文件内容校验、TCP 转发、临时节点重启后保存地址恢复和设备禁用。

测试只使用独立身份、私有临时目录与测试进程。它们不代表任意运营商的公网 IPv6 / 单端公网 IPv4 都可达，也不代表当前已安装的 Mac / Ubuntu 服务已更新。现有部署快照仍见[部署记录](task-reports/VEYTRANE_DEPLOYMENT.md)。

本次检查的准确通过数、测试范围与现有部署边界见[验证记录](task-reports/SIGNAL_FREE_CONNECTIVITY.md)。
