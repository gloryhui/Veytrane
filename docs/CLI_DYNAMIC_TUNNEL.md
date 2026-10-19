# CLI 动态 TCP 转发与共享会话

对应 [Issue #85](https://github.com/gloryhui/p2p_file/issues/85)。本文描述已实现的
CLI 能力、配置格式、热加载语义和验收边界。

## 1. 目标与非目标

目标：

- Ubuntu 无界面常驻服务用持久化 JSON 管理“哪些节点可以访问哪些 TCP 目标”，
  修改后通过 `SIGHUP` 热加载，不重启进程。
- macOS/Linux CLI 只常驻一个客户端进程，多条本机转发规则共享同一对端的信令登记
  和已认证 QUIC 会话。
- 增删/停用/修改一条规则不影响其他监听和已经建立的转发连接。
- 保留 loopback 监听、目标 IP:端口白名单、节点身份与端到端认证边界。

非目标：

- 不把本机变成整个局域网或全网段 VPN；
- 不做 UDP 转发；
- 不做 Web 管理界面；
- 不改变 Direct/Relay、文件传输、测速的协议行为。

## 2. 服务端：`serve --config`

```bash
p2p_file serve \
  --signal 203.0.113.7:7000 \
  --config /etc/p2p_file/service.json \
  --recv-dir /srv/p2p/inbox
```

`service.json`：

```json
{
  "version": 1,
  "forward_targets": [
    {
      "id": "sub2api",
      "name": "Sub2API",
      "target": "127.0.0.1:8080",
      "enabled": true,
      "allowed_peers": ["a1b2c3d4e5f60718293a4b5c6d7e8f90"]
    },
    {
      "id": "nas-ssh",
      "name": "NAS SSH",
      "target": "192.168.1.20:22",
      "enabled": false,
      "allowed_peers": ["a1b2c3d4e5f60718293a4b5c6d7e8f90"]
    }
  ]
}
```

规则：

- `version` 必须为 `1`；未知字段直接报错，避免拼写错误被静默忽略。
- `id` 为 1..=64 个 ASCII 字母、数字、`-`、`_`、`.`，同一文件内唯一。
- `name` 为 1..=128 个字符，仅用于日志/诊断。
- `target` 必须是具体 IP 和 1..65535 端口；拒绝 `0.0.0.0`、`::` 和组播。
- `allowed_peers` 是允许访问该目标的 NodeId 列表；空列表表示**不允许任何节点**。
- `enabled=false` 的条目保留在文件里，但不授权新连接。

当前 `serve` 每个进程只支持一个对端（打洞是点对点的）。配置里所有启用目标的
`allowed_peers` 并集必须恰好是一个 NodeId；多个对端请各开一个进程，或使用桌面端。
`--config` 与旧的 `--allow` / `--forward` 互斥，避免两套授权来源混淆。

热加载：

```bash
systemctl kill -s HUP p2p-signal   # 或 kill -HUP <pid>
```

收到 `SIGHUP` 后重新读取并校验文件：

- 校验通过：原子替换运行中的策略；之后的新连接按新策略授权；
- 校验失败：打印错误，**保留旧策略**，不会出现半加载状态；
- 已经建立并正在传输的业务流不会被强制中断；撤销授权只影响新连接/新隧道流。
  这是刻意选择：中断正在进行的 SSH/HTTP 流比等待其自然结束风险更大。
- 当前 `serve` 的打洞循环绑定一个对端。热加载可以增删该对端的目标、
  或通过空目标列表撤销全部授权；把授权对端换成另一个 NodeId 需要重启进程，
  热加载会拒绝并提示。

## 3. 客户端：`forward --config`

```bash
p2p_file forward \
  --signal 203.0.113.7:7000 \
  --config ~/.config/p2p_file/client.json
```

`client.json`：

```json
{
  "version": 1,
  "rules": [
    {
      "id": "sub2api",
      "name": "Sub2API",
      "peer_node_id": "a1b2c3d4e5f60718293a4b5c6d7e8f90",
      "listen": "127.0.0.1:8080",
      "target": "127.0.0.1:8080",
      "enabled": true,
      "auto_start": true
    },
    {
      "id": "nas-ssh",
      "name": "NAS SSH",
      "peer_node_id": "a1b2c3d4e5f60718293a4b5c6d7e8f90",
      "listen": "127.0.0.1:2222",
      "target": "192.168.1.20:22",
      "enabled": true,
      "auto_start": true
    }
  ]
}
```

规则：

- 所有启用规则必须指向同一个 `peer_node_id`，这样才共享一个认证会话；出现多个
  对端会在加载阶段被拒绝。
- `listen` 必须是 `127.0.0.1` 或 `::1`，端口 1..65535；同一文件内启用规则的
  `listen` 不能重复。
- `target` 必须是具体 IP 和端口；域名被拒绝，避免 DNS 绕过白名单。
- `auto_start` 保留给桌面端语义；CLI 只使用 `enabled`。

进程启动时绑定所有启用规则。之后修改文件并发送 `SIGHUP`：

- 新增规则：绑定新端口并启动；绑定失败会报告，其他规则继续运行；
- 删除/停用规则：关闭对应监听；已经接受的连接继续传输到自然结束；
- 只改 `target`：不重新绑定端口，新连接使用新目标；
- 改 `listen`：先绑定新地址，成功后再关闭旧监听；新地址失败时保留旧规则；
- 配置文件整体解析/校验失败：保留全部旧规则。
- 空规则文件（`rules: []`）在运行中可把全部监听停掉；进程启动时仍要求至少一条
  启用规则，否则没有可共享的对端。

不启动网络也可以先校验并查看规则：

```bash
p2p_file rules docs/examples/service.json
p2p_file rules --service /etc/p2p_file/service.json
p2p_file rules --client ~/.config/p2p_file/client.json
```

未指定 `--service` / `--client` 时按 JSON 顶层字段自动识别。命令成功退出码为 0，
非法文件、重复 ID、非 loopback 监听、无效 NodeId 等会以非零退出并打印原因。

也可以不手改 JSON，启动仅监听 loopback 的 [Web 管理页](WEB_ADMIN.md)：

```bash
p2p_file serve ... --config /etc/p2p_file/service.json --admin-listen 127.0.0.1:7788
p2p_file forward ... --config ~/.config/p2p_file/client.json --admin-listen 127.0.0.1:7789
```

## 4. 空闲后重连

原来的 Direct-only 重连复用首次配对的 token、候选地址和探测观测。服务端在
30 秒业务空闲后关闭认证连接，并在 `re-punch-after` 后重新登记/打洞；客户端再用
旧值拨号就会得到：

```text
no authenticated path: ... authenticated path timeout
```

修复分两部分：

1. 客户端 `DirectLink::connect_authenticated` 的每次重连都重新登记信令、等待新的
   pairing/token、重新向新候选发探测；Relay fallback 仍然只作为可选路径。
2. Direct-only 的服务端在已认证会话期间也处理新的 `PeerCandidates`，注册新 token
   并回探测包，使客户端的新配对能在同一轮服务会话内完成打洞。

自动化回归：`tunnel::tests::direct_only_idle_reconnects_after_serve_repunch`。
它使用缩短的 300ms 业务空闲和 2s 重打洞周期，验证第一次隧道关闭、服务端重打洞
之后，第二次本机请求仍能成功。

## 5. 实现结构

| 位置 | 作用 |
| --- | --- |
| `src/tunnel/rules.rs` | 桌面/CLI 共用 `AllowedForwardTarget`、`TunnelRule`、配置校验、`ServePolicy` 原子快照 |
| `src/tunnel/supervisor.rs` | `SharedSession` 与多监听 `RuleSupervisor`，支持运行时增删/改目标 |
| `src/admin/` | loopback Web 管理页、token/Host/Origin 校验、API 与原子持久化 |
| `src/tunnel/mod.rs` | Direct-only 重连、服务端动态 `PeerCandidates`、按 peer 校验目标 |
| `src/desktop/config.rs` | 重新导出共用规则类型，settings.json 字段名保持不变 |
| `src/cli.rs` / `src/main.rs` | `serve --config`、`forward --config`、SIGHUP 热加载入口 |

`AllowedForwardTarget` / `TunnelRule` 的字段名就是桌面 `settings.json` 的持久化
格式；从 `desktop::config` 移到核心模块时没有改字段或 serde 属性，因此旧配置
无需迁移。

## 6. 验收对照

| Issue 验收项 | 自动化证据 | 真实环境 |
| --- | --- | --- |
| 运行中服务端加目标、客户端加规则，无需重启 | `service_policy_hot_reload_changes_authorized_targets`、`shared_session_serves_multiple_rules_and_applies_live_changes` | 需在真实双 NAT 上复测 |
| 同一身份/对端多条转发共享会话、不互踢 | `shared_session_serves_multiple_rules_and_applies_live_changes` | 多浏览器/API 并发 |
| 未授权节点/目标被拒绝，撤销有回归 | `service_policy_is_atomic_and_per_peer`、`service_policy_hot_reload_changes_authorized_targets` | 撤销后新连接验证 |
| 端口占用/非法配置可诊断、保留旧规则 | `client_rules_require_one_peer_and_loopback_listeners`、`service_config_rejects_duplicate_peers_and_targets`、`RuleSupervisor::apply` 错误保留逻辑 | 真实端口占用 |
| 空闲超过服务端超时和重打洞周期后恢复 | `direct_only_idle_reconnects_after_serve_repunch` | 关闭保活脚本后浏览器/API 请求 |
| 服务端重启/候选变化/短断网后重新配对认证 | 上述重连测试 + 既有 Relay/Direct 回归 | 真实双 NAT 断网 |
| 文件/测速/认证/Direct/Relay 不回归 | 全量 `cargo test --all-targets`、`cargo test --features gui --lib` | 三平台 CI |

真实双 NAT、浏览器资源加载和 API 流式响应仍需人工验收；自动化只证明 loopback
上的授权、共享会话、重连和规则热加载语义。

## 7. 任务拆分与接口

实际实施按三个可独立合并的增量推进：

### PR1：Direct-only 空闲重连

- 范围：`src/net/mod.rs`、`src/tunnel/mod.rs`、重连回归。
- 接口变化：`DirectLink` 内部用 `attempts` 区分首连/重连；重连固定走
  `SignalingClient::connect_with_tls` → `resolve_peer_waiting` → `spawn_punch_probes`
  → `race::prepared_tasks`。
- 服务端新增 `PeerCandidates` 直连探测分支，不改变 Relay 路径。
- 验收：`direct_only_idle_reconnects_after_serve_repunch`，以及原有 tunnel
  全量测试。

### PR2：共用规则模型与服务端动态授权

- 范围：新增 `src/tunnel/rules.rs`，`src/desktop/config.rs` 改为复用类型，
  `ServeConfig` 增加 `Option<Arc<ServePolicy>>`。
- 关键接口：

  ```rust
  pub struct ServePolicy { /* RwLock<CompiledPolicy> */ }
  impl ServePolicy {
      pub fn new(&ServiceConfig) -> Result<Self>;
      pub fn replace(&self, &ServiceConfig) -> Result<()>; // 校验后原子替换
      pub fn peer_allowed(&self, NodeId) -> bool;
      pub fn target_allowed(&self, NodeId, SocketAddr) -> bool;
  }
  ```

- `serve --config FILE`：加载失败即退出；运行中 `SIGHUP` 失败保留旧快照。
- 验收：`service_policy_is_atomic_and_per_peer`、
  `service_policy_hot_reload_changes_authorized_targets`。

### PR3：客户端共享会话与多规则监督

- 范围：新增 `src/tunnel/supervisor.rs`，CLI 新增 `forward --config FILE`。
- 关键接口：

  ```rust
  pub struct SharedSession { /* 单飞重连 + 复用 quinn::Connection */ }
  impl SharedSession { pub async fn connection(&self) -> Result<Connection>; }

  pub struct RuleSupervisor { /* 每条启用规则一个监听 task */ }
  impl RuleSupervisor {
      pub async fn start(Arc<SharedSession>, &[TunnelRule]) -> Result<Self>;
      pub async fn apply(&mut self, &[TunnelRule]) -> Vec<String>;
      pub async fn shutdown(&mut self);
      pub fn listeners(&self) -> Vec<(String, SocketAddr)>;
  }
  ```

- `forward --config FILE`：启动时绑定全部启用规则，`SIGHUP` 时 diff：
  新增/删除/停用/改目标/改监听；失败规则保留旧监听。
- 验收：`shared_session_serves_multiple_rules_and_applies_live_changes`。

三个增量共用 `cargo test --all-targets`、`cargo test --features gui --lib`、
`cargo clippy --all-targets -- -D warnings` 和三平台 CI；真实双 NAT 单独记录人工证据。

## 8. 后续可扩展

- 为 `serve` 支持多个对端（每个对端一个打洞循环/会话）；
- 为无界面端增加本地管理 CLI（`rules add/remove`）或 Unix socket 控制接口；
- Windows 使用命名管道/文件监听替代 `SIGHUP`；
- 按规则统计连接数、字节数和最后错误，输出到日志或状态文件。
