# 当前仓库分层与维护边界

本说明对应 2026-10-21 的源码整理。它描述现有实现，不将历史规划或本地编译视为发布验收。网络协议设计见 [ARCHITECTURE.md](ARCHITECTURE.md)，协议兼容边界见 [NODE_COMPATIBILITY.md](specs/NODE_COMPATIBILITY.md)。

## 入口与依赖方向

```text
src/bin/p2p-desktop.rs → desktop/agent_shell → node/api + node/local
src/bin/p2p-node.rs   → node/adapters（CLI、HTTP）
src/bin/p2p-agent.rs  → node/host → node/core + node/api + node/local
src/bin/sub2api-*.rs / p2p-sub2api-gateway.rs → node/products
src/main.rs          → 既有独立 CLI（admin、tunnel、transfer 等）

node/core → 网络、协议、身份、通用存储基础设施
node/api  → core 的数据类型 + local 的 IPC 连接
node/local → OS 路径、实例锁、通用私有目录权限
node/host → core、api、local
node/adapters / node/products → 以上服务与契约
```

当前只维护现有调用方：`node/mod.rs` 只声明六个分层模块，旧 public 路径别名已删除，源码和集成测试均使用所属层路径。二进制名称、默认 feature、node.json、IPC JSON 和线上字节协议未变。

## 源码职责

| 位置 | 职责与边界 |
| --- | --- |
| `src/node/core/` | 唯一无 GUI 业务实现：会话、授权、配置、文件任务、服务目录、测速、调度与恢复。不得依赖 Agent 宿主、管理适配器、IPC 客户端或桌面。 |
| `src/node/core/settings.rs` | `NodeSettings`、持久配置记录及核心提交参数。`CommitOptions` 不再由宿主定义，核心可独立接收配置事务。 |
| `src/node/host/` | Agent 进程所有权、管理命令执行、配置协调、转发监听和运行恢复。 |
| `src/node/local/` | 当前 OS 用户的节点路径、IPC socket/pipe 连接与监听；权限实现由通用存储层提供。 |
| `src/node/api/` | 管理请求/响应、版本信息与客户端。客户端不持有 Agent 实例、身份或网络会话。 |
| `src/node/adapters/` | 管理 CLI、HTTP 网关、内嵌 Web HTML。Web 是 Agent 的客户端。 |
| `src/node/products/` | Sub2API 限定用途组合及其恢复策略；复用核心与监听机制。 |
| `src/desktop/agent_shell/` | 唯一 IPC 桌面及其工作台、窗口、托盘宿主。 |
| `src/desktop/ui/` | 工作台与二级窗口共用控件、文本输入、视觉主题、品牌。无业务运行时所有权。 |
| `src/desktop/platform/` | 桌面共用托盘、窗口隐藏/显示、展示状态；不反向引用任一窗口所有者。 |
| `src/storage/` | 原子写入、私有目录权限、相对路径校验、句柄相对文件访问与下载落盘；不依赖 node/desktop。 |
| `src/discovery/`、`nat/`、`net/`、`transport/`、`relay/` | 共用的发现、打洞、连接竞速、QUIC、Relay。 |
| `src/protocol/`、`transfer/`、`tunnel/`、`admin/` | 原独立 CLI 仍在使用的协议与执行路径；不能因与 node 同名就删除。 |

`core/settings.rs` 定义当前 `node.json`。`core/config.rs` 仍有共享规则校验、测速类型和独立核心配置读写调用；已移除旧 schema 1–10 升级、历史目录发现、通知/后台偏好及无生产调用的偏好写入接口。旧桌面导入适配器与对应 CLI / UI 已删除。

`core/process_tests.rs` 保留原先位于旧桌面目录的真实进程业务回归，直接依赖核心，随 node 特性测试执行，全部使用 P2PN v2 协商及统一分发器。

`core/session.rs` 与 `core/transfer.rs` 的大型测试模块分别位于同名子目录的 `tests.rs`，仍然是原模块的子测试，可访问必要私有状态。进程测试使用 `module_path!()` 构造子进程入口，避免目录重组后悄悄执行零个测试。

## 本次审阅后的处置

| 发现 | 处理 |
| --- | --- |
| desktop 下 29 个三行文件只转发 node 模块；多处“未来任务将接入”的注释掩盖实际依赖 | 删除转发文件，旧桌面直接导入唯一 core 实现；移除已经失去意义的模块级死代码豁免。 |
| 默认桌面从旧 6,493 行窗口文件通配导入控件、品牌和原生后台能力 | 默认入口与旧运行时分开，共用能力归入 UI/platform，内嵌旧入口随后按单用户当前版本策略移除。 |
| 会话依赖宿主配置协调器的 `CommitOptions` | 参数下沉到核心配置模型，协调器依赖核心契约。 |
| 通用身份锁和存储反向调用 node 的权限/文件安全函数 | 通用权限与安全文件操作移入 storage，协议相对路径校验保持同一实现和原有拒绝规则。 |
| `nat/portmap.rs` 没有生产调用，恒定返回不支持 | 删除占位模块及伪实现测试；UPnP/NAT-PMP/PCP 仍是未实现能力。该未实现模块的 Rust 导出不再保留。 |
| 恢复查询 DTO/访问器只有旧回归使用 | 保留为 `cfg(test)` 的回归辅助；实际启动恢复仍正常编译运行。 |
| 根模块兼容别名、导入与偏好遗留 | 删除别名、migration.rs、preferences.rs、旧导入规格及专属 UI/CLI；不再支持 schema 1–10 升级。 |
| 未实现的 LegacyCliV3 模式 | 移除占位枚举和三处不可达分支；配置边界直接拒绝，不进行连接。 |
| 内嵌旧桌面与 Agent 工作台并存 | 移除 `--legacy`、旧窗口/通知/登录启动及专属依赖；进程业务回归迁入 `core/process_tests.rs`。独立 CLI 的仍在使用路径另行维护。 |

## 其他目录如何维护

- `scripts/`：打包、宿主管理、回归工具及手动导航同步。`node_smoke.py` 是脚本公共夹具；不是废弃启动入口。核心进程矩阵入口为 `node-core-e2e.py`，无需 GPUI；旧 `desktop-e2e.py` 已移除。
- `packaging/`：各平台安装模板与入口。它们由打包脚本消费，不能仅根据本机 OS 判定无用。
- `tests/`：独立进程、真实协议和 CLI 集成测试；模块内部细节测试与源码同域放置。
- `examples/`：可执行测试/演示入口，受 Cargo feature 约束。
- `docs/specs/`：现行契约与历史设计约束；物理路径以本文为准。
- `docs/task-reports/`、`docs/gpui-mvp/`：历史任务和验收证据，保留来源与链接；不代表当前全部功能已上线。`THIRD_PARTY_NOTICES.md` 仍承载共享文本控件的归属说明。
- `assets/`：源码内嵌及打包使用的图标。使用方仍经编译和打包验证。
- `target/`、`release/`：已被 Git 忽略的生成物；本次不删除缓存、已安装二进制或发布包。
- `service-navigation.html`：用户的本机服务目录模板，按 `AGENTS.md` 单次同步；不是另一个后台服务。私有凭据副本不在仓库。

[交互架构图](architecture-current.html) 根据当前分层提供四个视图，节点数据维护在 `docs/architecture/architecture-map.json`。编辑数据后运行 `python3 scripts/update-architecture-map.py` 将其嵌入单文件 HTML；图离线运行，无需额外服务。历史文档中的旧源码路径可通过 Git 历史追溯。

## 检查与后续维护

```sh
python3 scripts/check-architecture.py
python3 scripts/update-architecture-map.py --check
cargo fmt --all -- --check
cargo check --locked --no-default-features --all-targets
cargo check --locked --all-features --all-targets
cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4
python3 scripts/package-desktop-tests.py
python3 scripts/node-host-tests.py
```

边界脚本检查显式 crate 路径，覆盖测试与当前主机未激活的条件分支，已接入现有 GPUI CI；它不替代 Rust 名称解析或运行验收。跨层导入采用规范路径，不绕过公共层级重导出来隐藏依赖。桌面核心中的会话与文件执行仍是较大的状态机；进一步拆业务算法需连同并发、事务及协议负例独立验证，不能只按行数切碎。

本次清理与验证见 [遗留代码清理记录](task-reports/LEGACY_CLEANUP.md)；首次分层证据见 [分层整理验证记录](task-reports/REPOSITORY_REORGANIZATION.md)。

当前节点只有 `wire.rs` 协商和 `dispatch.rs` 分发入口。`protocol.rs` 是 P2PF 文件/测速载荷；`services.rs` 负责服务 ID 开流，字节搬运复用 `src/tunnel/mod.rs`。旧 `core/tunnel.rs`、PeerWireMode 与双路入站循环已删除。

## 无信令连接入口

`core/connectivity.rs` 维护直连配置、独立 mDNS 命名空间、地址校验和有界过期缓存；`core/session/direct.rs` 负责 IP 直连的身份接纳与超时。局域网、手动地址和信令共用 `finish_peer_transport`，不复制文件、服务或认证实现。`nearby` 及 `connect-address` 经同一管理 API 接入桌面、CLI 和 Web。

发现记录不授予权限，完整 NodeId 固定预期身份，原密码/可信设备、资源 ACL 和代际门禁保留。无信令时跳过公网 STUN/Relay，专用 Sub2API 组合显式禁用通用节点发现与直连接入。流程与配置见[无信令连接说明](SIGNAL_FREE_CONNECTIVITY.md)。
