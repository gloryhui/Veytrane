# 当前仓库分层与维护边界

本说明描述当前实现。施工阶段和验收记录见[节点模块化施工说明](../architecture/node-modularization-plan.md)，网络协议和兼容要求见 [ARCHITECTURE.md](ARCHITECTURE.md) 与 [NODE_COMPATIBILITY.md](specs/NODE_COMPATIBILITY.md)。

## 入口与依赖方向

```text
src/bin/p2p-desktop.rs → desktop/agent_shell → node/api + node/local
src/bin/p2p-node.rs   → node/adapters（CLI、HTTP）
src/bin/p2p-agent.rs  → node/host + 可选的 node/adapters/navigation
src/bin/sub2api-*.rs / p2p-sub2api-gateway.rs → node/products
src/main.rs          → 既有独立 CLI（admin、tunnel、transfer 等）

node/core → 节点/传输、TCP、测速及选装的文件能力
node/api  → core 的数据类型 + local 的 IPC 连接
node/local → OS 路径、实例锁、通用私有目录权限
node/host → core、api、local
node/adapters / node/products → 以上服务与契约
```

默认构建保留现有二进制入口、`node.json`、IPC JSON 和线上协议。Agent 可选装文件和导航。根包的部分重导出仍供现有调用方使用。

## 源码职责

| 位置 | 职责与边界 |
| --- | --- |
| `src/node/core/` | 节点会话、配置与运行装配。底层认证、流分发及业务执行由各能力包承接。 |
| `src/node/core/settings.rs` | `NodeSettings`、持久配置记录及核心提交参数。`CommitOptions` 不再由宿主定义，核心可独立接收配置事务。 |
| `src/node/host/` | Agent 进程所有权、管理命令执行、配置协调、转发监听和运行恢复。 |
| `src/node/local/` | 当前 OS 用户的节点路径、IPC socket/pipe 连接与监听；权限实现由通用存储层提供。 |
| `src/node/api/` | 管理请求/响应、版本信息与客户端。客户端不持有 Agent 实例、身份或网络会话。 |
| `src/node/adapters/` | 管理 CLI、独立 HTTP 网关和私有导航。Web 经本机 IPC 调用 Agent，可由 Agent 入口在同进程装配。 |
| `src/node/products/` | Sub2API 限定用途组合及其恢复策略；复用核心与监听机制。 |
| `src/desktop/agent_shell/` | 唯一 IPC 桌面及其工作台、窗口、托盘宿主。 |
| `src/desktop/ui/` | 工作台与二级窗口共用控件、文本输入、视觉主题、品牌。无业务运行时所有权。 |
| `src/desktop/platform/` | 桌面共用托盘、窗口隐藏/显示、展示状态；不反向引用任一窗口所有者。 |
| `src/storage/` | 基础存储和独立 CLI 文件存储的兼容入口。 |
| `src/discovery/`、`nat/`、`net/`、`transport/`、`relay/` | 共用的发现、打洞、连接竞速、QUIC、Relay。 |
| `src/protocol/`、`transfer/`、`tunnel/`、`admin/` | 原独立 CLI 仍在使用的协议与执行路径；不能因与 node 同名就删除。 |
| `crates/veytrane-transport` | 身份、绑定到 QUIC 会话的握手、端点、有界原始帧和 OS 存储原语。 |
| `crates/veytrane-node` | P2PN 协商、双向流分发、会话授权、设备容量、连接代际及活动许可。 |
| `crates/veytrane-files` | P2PF 文件协议、文件收发、调度、限速、落盘、回执、任务库及断点恢复。 |
| `crates/veytrane-file-policy` | 保留配置中的限速值和接收余量校验，供裁剪 Agent 读写同一份 `node.json`。 |
| `crates/veytrane-speed` | 测速控制、数据流、租约及结果。 |
| `crates/veytrane-tcp` | TCP 规则校验、流配额、监听和 QUIC 字节桥接。 |

`core/settings.rs` 定义当前 `node.json`。`core/config.rs` 仍有共享规则校验、测速类型和独立核心配置读写调用；已移除旧 schema 1–10 升级、历史目录发现、通知/后台偏好及无生产调用的偏好写入接口。旧桌面导入适配器与对应 CLI / UI 已删除。

`core/process_tests.rs` 保留原先位于旧桌面目录的真实进程业务回归，直接依赖核心，随 node 特性测试执行，全部使用 P2PN v2 协商及统一分发器。

会话回归在 `src/node/core/session/tests.rs`，文件收发回归在 `crates/veytrane-files/src/transfer/tests.rs`。`scripts/node-core-e2e.py` 同时运行根节点、节点包及文件包的用例。

## 构建组合

```sh
cargo build --locked --no-default-features --features node --bin p2p-agent --bin p2p-node
cargo build --locked --no-default-features --features agent-base --bin p2p-agent
cargo tree --locked --no-default-features --features agent-base -e normal,features
cargo build --locked --no-default-features --features standalone --bin p2p_file
```

`node` 包含完整发行的文件、测速、TCP、导航与独立命令。`agent-base` 提供节点、测速、TCP 和 IPC，运行依赖树无需文件执行包、GPUI 或 HTTP 管理。`agent-files` 增加文件能力，`agent-web` 增加同进程导航。通过 `--no-default-features` 单独构建旧命令时，显式选择 `standalone`。已有安装版命令用法沿用。

裁剪 Agent 初始化时把 `enable_files` 设为 `false`。读取或提交明确启用文件的配置时返回能力错误，保留当前配置文件。文件命令返回 `capability_disabled`，`Tasks` 仍返回空文件列表、空预览、可空队列与测速结果。

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
cargo check --locked --no-default-features --features standalone --all-targets
cargo check --locked --all-features --all-targets
cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4
python3 scripts/package-desktop-tests.py
python3 scripts/node-host-tests.py
```

边界脚本覆盖显式分层导入，包括测试与当前主机未激活的条件分支。Cargo 依赖树和实际进程分别用于核对编译边界与运行行为。

本次清理与验证见 [遗留代码清理记录](task-reports/LEGACY_CLEANUP.md)；首次分层证据见 [分层整理验证记录](task-reports/REPOSITORY_REORGANIZATION.md)。

节点的 P2PN 协商与双向流分发位于 `veytrane-node`。文件与测速分别解码已分配的 P2PF 消息，`services.rs` 负责按服务 ID 开流并复用 `veytrane-tcp` 的字节搬运。

## 无信令连接入口

`core/connectivity.rs` 维护直连配置、独立 mDNS 命名空间、地址校验和有界过期缓存；`core/session/direct.rs` 负责 IP 直连的身份接纳与超时。局域网、手动地址和信令共用 `finish_peer_transport`，不复制文件、服务或认证实现。`nearby` 及 `connect-address` 经同一管理 API 接入桌面、CLI 和 Web。

发现记录不授予权限，完整 NodeId 固定预期身份，原密码/可信设备、资源 ACL 和代际门禁保留。无信令时跳过公网 STUN/Relay，专用 Sub2API 组合显式禁用通用节点发现与直连接入。流程与配置见[无信令连接说明](SIGNAL_FREE_CONNECTIVITY.md)。
