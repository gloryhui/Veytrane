# 当前仓库分层与维护边界

本说明描述当前实现。施工阶段和验收记录见[节点模块化施工说明](../architecture/node-modularization-plan.md)，网络协议和兼容要求见 [ARCHITECTURE.md](ARCHITECTURE.md) 与 [NODE_COMPATIBILITY.md](specs/NODE_COMPATIBILITY.md)。

## 入口与依赖方向

```text
src/bin/p2p-desktop.rs → desktop/agent_shell → veytrane-management
src/bin/p2p-node.rs → apps/node-cli → veytrane-management
                       ├─ 离线命令 → Agent 宿主
                       └─ web → apps/web-management → veytrane-management
src/bin/p2p-agent.rs → apps/agent → Agent 宿主与可选 Web 导航
                   └─ node/composition → node/core → 节点、传输及能力包
Live / Sub2API → 各自的产品组装 → node/composition、TCP
src/main.rs → apps/standalone-cli
           └─ signal-server → apps/signaling → 信令、短 ID 注册库和可选 Relay
```

默认构建保留现有二进制入口、`node.json`、IPC JSON 和线上协议。Agent 可选装文件和导航。根包的部分重导出仍供现有调用方使用。

## 源码职责

| 位置 | 职责与边界 |
| --- | --- |
| `src/node/core/` | 会话、连接代际、网络恢复及已认证流生命周期。根会话接收已装配的服务，在每条连接上为双方已协商的业务注册有界队列，协调关闭与撤销。各能力的执行代码由对应包持有。 |
| `src/node/composition/` | `NodeSettings`、持久配置记录、配置到会话策略的投影，以及 TCP 转发连接适配器。`runtime.rs` 供 Agent、Live 与 Sub2API 组装会话及所需的测速、文件服务。 |
| `src/node/host/` | Agent 进程所有权、IPC 服务端命令、配置 revision、请求去重、事件聚合与关停。 |
| `apps/agent` | `p2p-agent` 的启动参数、可选导航与宿主装配，以及核心重建和退出协调。 |
| `crates/veytrane-management` | IPC v1 请求和响应、AgentClient、当前用户路径、Unix socket 和 Windows pipe。`node/api` 给旧 Rust 调用方转出契约和客户端，并提供根二进制版本信息。 |
| `apps/node-cli` | `p2p-node` 的命令编排，正常请求直接使用管理 IPC 契约，离线操作调用宿主配置入口。`node/adapters` 保留已有 Rust 导入路径。 |
| `apps/web-management` | `p2p-node web` 管理页和 Agent 默认 `3001` 导航。两个入口经同一 IPC 使用 Agent。内置导航与 Agent 同进程，核心重建时导航继续运行。 |
| `crates/veytrane-http-auth` | 旧独立管理页与节点管理页共用的回环 Host、Origin、Bearer 和令牌文件逻辑。 |
| `apps/live`、`apps/sub2api` | 专用产品各自的装配、授权、容量和界面。共用恢复退避及节点事件辅助仍在 `src/node/products/`。 |
| `src/desktop/agent_shell/` | 唯一 IPC 桌面及其工作台、窗口、托盘宿主。 |
| `src/desktop/ui/` | 工作台与二级窗口共用控件、文本输入、视觉主题、品牌。无业务运行时所有权。 |
| `src/desktop/platform/` | 桌面共用托盘、窗口隐藏/显示、展示状态；不反向引用任一窗口所有者。 |
| `src/storage/` | 基础存储和独立 CLI 文件存储的兼容入口。 |
| `src/discovery/`、`nat/`、`net/`、`transport/`、`relay/` | 共用发现客户端、打洞、连接竞速、QUIC 和 Relay 客户端。服务端模块只在 `signaling-server` 及测试构建启用。 |
| `apps/signaling` | `p2p_file signal-server` 的启动、TLS 监听、设备登记与短 ID SQLite，以及可选 Relay 服务端。 |
| `apps/standalone-cli` | `p2p_file` 的命令编排、进程退出、交互输出及配置热加载。根二进制入口和公开参数解析路径继续供现有调用方使用。 |
| `src/protocol/`、`transfer/`、`tunnel/`、`admin/` | 原独立 CLI 仍在使用的协议与执行路径；不能因与 node 同名就删除。 |
| `crates/veytrane-transport` | 身份、绑定到 QUIC 会话的握手、端点、有界原始帧和 OS 存储原语。 |
| `crates/veytrane-node` | P2PN 协商、双向流分发、会话授权、设备容量、连接代际及活动许可。 |
| `crates/veytrane-files` | P2PF 文件协议、文件收发、调度、限速、落盘、回执、任务库及断点恢复。 |
| `crates/veytrane-file-policy` | 限速值、接收余量及路径批次格式校验，供配置、文件服务和桌面共用。 |
| `crates/veytrane-speed` | 测速控制、数据流、租约及结果。 |
| `crates/veytrane-tcp` | TCP 规则校验、流配额、监听和 QUIC 字节桥接。 |

`composition/settings.rs` 定义当前 `node.json`。宿主验证并投影策略，提交一次性持久化操作。会话 actor 在同一事务点确认连接代际、写盘并发布有效授权。`core/config.rs` 提供共享校验及运行类型，旧桌面配置读写和旧信任编辑路径保留在回归测试中。

`core/process_tests.rs` 保留原先位于旧桌面目录的真实进程业务回归，直接依赖核心，随 node 特性测试执行，全部使用 P2PN v2 协商及统一分发器。

会话回归在 `src/node/core/session/tests.rs`，文件收发回归在 `crates/veytrane-files/src/transfer/tests.rs`。`scripts/node-core-e2e.py` 同时运行根节点、节点包及文件包的用例。

## 构建组合

```sh
cargo build --locked --no-default-features --features node --bin p2p-agent --bin p2p-node
cargo build --locked --no-default-features --features agent-full --bin p2p-agent --bin p2p-node
cargo build --locked --no-default-features --features desktop-client --bin p2p-desktop
cargo build --locked --no-default-features --features agent-base --bin p2p-agent
cargo tree --locked --no-default-features --features agent-base -e normal,features
cargo build --locked --no-default-features --features standalone --bin p2p_file
```

`node` 保留完整发行组合和独立命令。`agent-full` 构建完整 Agent 和管理 CLI，排除旧 CLI、信令服务端及其 SQLite。`agent-base` 内置测速与 TCP，排除文件执行、GPUI 和 HTTP 管理。`agent-files` 增加文件能力，`agent-web` 增加同进程导航。`desktop-client` 可单独构建桌面。`standalone` 提供旧 CLI，保留原信令启动命令。Live 和 Sub2API 可用各自 feature 构建。

裁剪 Agent 初始化时把 `enable_files` 设为 `false`。读取或提交明确启用文件的配置时返回能力错误，保留当前配置文件。文件命令返回 `capability_disabled`，`Tasks` 仍返回空文件列表、空预览、可空队列与测速结果。

`Status.capabilities` 表示当前 Agent 本机已装配的业务。GPUI 根据它控制文件与测速入口，设备连接状态独立读取 peer 快照。`Tasks` 的文件事实来自 `veytrane-files`，IPC 宿主维持现有分页、脱敏和诊断投影。

## 本次审阅后的处置

| 发现 | 处理 |
| --- | --- |
| desktop 下 29 个三行文件只转发 node 模块；多处“未来任务将接入”的注释掩盖实际依赖 | 删除转发文件，旧桌面直接导入唯一 core 实现；移除已经失去意义的模块级死代码豁免。 |
| 默认桌面从旧 6,493 行窗口文件通配导入控件、品牌和原生后台能力 | 默认入口与旧运行时分开，共用能力归入 UI/platform，内嵌旧入口随后按单用户当前版本策略移除。 |
| 会话依赖持久配置记录 | 宿主持有持久配置文件，会话只接收当前策略和一次性持久化操作。 |
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

[节点架构图](../architecture/layer-review.html) 提供八种主题、亮暗配色和当前图的 PNG 导出。[源码索引](architecture-current.html) 提供四个可钻取视图。节点数据维护在 `docs/architecture/architecture-map.json`，编辑后用 `python3 scripts/update-architecture-map.py` 更新离线 HTML。历史文档中的旧源码路径可通过 Git 历史追溯。

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
