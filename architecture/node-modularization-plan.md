# Veytrane 节点框架模块化施工说明

**交给 GPT-6.2-Sol 的实施基线** · 2026-11-20 · `main` / `3374b65`。本文描述目标设计和施工顺序。当前实现见[分层审查](layer-review.md)，线上格式以[节点协议](../docs/specs/NODE_WIRE.md)、[兼容边界](../docs/specs/NODE_COMPATIBILITY.md)和已有字节测试为准。执行前记录实际 HEAD、工作树和暂存区；基线时 `src/cli.rs`、`src/nat/stun.rs` 和五份操作文档已有其他改动。

## 1. 要交付什么

Veytrane 的核心是可信节点之间建立、维持和恢复连接的机制。TCP 服务、测速、文件传输是三个使用该机制的能力。Agent 是一个可长期运行的产品组合。GPUI、管理 CLI 和 Web 使用管理契约。信令服务端可以服务多个节点，其生命周期独立于某个节点上的业务。

以三项结果验收整个工程：

1. **可裁剪**。构建只含 TCP 和测速的 Agent，其生产依赖树没有文件执行库。在全新隔离目录中初始化、启动、认证、测速、访问授权 TCP 服务、重连，整个过程不创建接收目录、文件任务库或其锁。用含现有任务的副本重复启动，停用文件能力期间原数据保持原样。完整版恢复启用后可读取原任务并按原规则继续。
2. **可组合**。同一版本能构建通用 Agent、Live、Sub2API 和独立 CLI。专用网关的容量策略只影响该实例。GPUI、`p2p-node`、导航页可以操作同一通用 Agent；关闭页面、退出 GUI 或重建 P2P 核心各自只影响其原有范围。
3. **可扩展**。一个复用 TCP 的新产品只需声明服务、授权目标与界面。需要新对等端报文的业务才增加能力编号、消息处理器和互通测试。两种扩展均无需在核心加入该业务的任务模型、文件路径或窗口状态。

本轮施工针对源码、测试、仓库文档和受影响的打包入口。既有二进制名称、默认发行组合、安装目录及运行身份是交付契约。真实节点安装、生产迁移、推送和下线独立 CLI 是另行安排的动作。

## 2. 目标模块与依赖方向

下表给出推荐的**最终生产代码归属**。先在现有包中按表分清所有权，再将需要编译隔离的部分提为 Cargo 包。`apps/` 表示产品组装，`crates/` 表示可被多个入口引用的库。名称用于施工和验收；现有包名 `p2p_file`、二进制和脚本入口在过渡期由根 `Cargo.toml` 继续提供。

| 目标位置 | 拥有的职责和数据 | 允许的依赖 |
|---|---|---|
| `crates/veytrane-transport` | 身份、密钥租约、发现客户端、候选、打洞、中继客户端、QUIC、身份握手、有界原始帧和必要 OS 存储原语 | 底层库。生产路径不引入文件清单、业务调度器、HTTP 管理、CLI、GPUI |
| `crates/veytrane-node` | 节点协商、认证和可信设备、会话代际与重连、peer 容量、已认证流分发、撤销与资源租约 | transport。提供能力接入点，只处理稳定标识、经协商的能力、当前授权及生命周期 |
| `crates/veytrane-files` | 节点文件收发、目录、队列、限流、接收根、文件 ACL 判定、任务持久化及恢复 | node、transport；文件专用纯算法可放同库独立模块，供旧 CLI 复用时见第 3 节 |
| `crates/veytrane-speed` | 测速控制、唯一 UNI 数据接收者、租约仲裁、统计与取消 | node、transport；可引用固定线格式的最小编解码模块，独立于文件执行和任务库 |
| `crates/veytrane-tcp` | 服务目录、服务 ID 校验、目标授权、开流、本地监听、规则协调及 TCP 字节转送 | node、transport；Agent、Live、Sub2API 均可直接组装 |
| `crates/veytrane-management` | IPC v1 请求/响应、公开 DTO、客户端、socket/pipe 定位与传输 | 基础类型和 OS 原语；反序列化任务 ID 等不引入能力执行库或宿主 |
| `apps/agent` | 进程锁、初始化、节点与能力装配、IPC 服务端、配置事务、请求去重、状态聚合、重建和退出 | node、management、选装的能力；可执行入口装配可选 Web 模块 |
| `apps/web-management`、`apps/node-cli`、`apps/desktop` | 导航与管理网页、管理命令、桌面/托盘。Web 模块可作为库由两个入口启动 | 通过 management 操作运行中的 Agent；离线初始化与离线配置由 CLI 调宿主提供的专用本机操作 |
| `apps/live`、`apps/sub2api` | 各产品配置、身份目录、准入策略、资源选择和界面；各自组合节点实例 | node、TCP 及共用的运行辅助；两产品之间无源码引用 |
| `apps/standalone-cli`、`apps/signaling` | 既有 `p2p_file` 命令编排，以及可被其 `signal-server` 子命令调用的信令/Relay 服务端启动模块 | 各自使用的传输、协议和服务端库；`apps/signaling` 本阶段可为库模块，沿用现有启动命令 |

**数据路径**：对端 → 已认证连接 → node 协商与有界分发 → 对应能力的实时授权检查 → 能力处理器。**管理路径**：GPUI / Web / `p2p-node` → 本地 IPC → Agent 命令分发 → 对应能力。**独立入口**：`p2p_file` 直接运行现有命令，Live / Sub2API 直接组合节点与 TCP。源码依赖、网络协议和操作系统进程是三种不同的边界。

## 3. 迁移清单

以下源路径均相对于 `src/`。一行包含多个职责时，按符号和生产调用方拆；先迁实现与测试，再替换导入。每个状态对象保留唯一写入者。

| 现有位置 | 移到哪里、如何拆、可以合并什么 |
|---|---|
| `identity.rs`、`instance_lock.rs`、`nat/`、`net/`、`transport/`、`discovery/`、`relay/` | 节点与独立命令共用身份、发现、候选、加密连接和客户端。分离 `discovery/signal.rs` 中的客户端/公共报文与服务端注册、短 ID 数据库、TLS 监听；`relay/server.rs` 和服务端启动交给 signaling。共享报文、域常量各保留一份。检查正常依赖树中信令服务端 SQLite 的来源。 |
| `protocol/frame.rs`、`protocol/message.rs`、`transport/handshake.rs` | 将有界原始帧 I/O 与特定 `ControlMessage` 读写分开。握手只需要现有身份消息子集及签名载荷；独立 CLI 拥有文件、隧道、测速消息。先固定原消息 discriminant、握手域、版本和拒绝行为，再换内部表示。不得通过把完整 CLI 枚举搬入 transport 间接带回 `FileManifest`。 |
| `node/core/wire.rs`、`dispatch.rs`、`session.rs`、`session/` | node 留下基础握手、鉴权、连接代际、唯一 BI 分发器、候选刷新与重连。把 `Services/ServiceList/OpenTcp/TcpReady` 的执行移到 TCP。`StreamKind` 的既有编号是线契约；队列注册和协商位由组装时提供。将会话中 `files/services/speed` 各自的启动、停止、重连业务处理交回能力。 |
| `node/core/control.rs`、`network_state.rs`、`remote_auth.rs`、`trusted_devices.rs`、`authorization.rs`、`connectivity.rs` | 基础 `NodeControl` 提供连接、断开、可信设备、快照/事件、会话关闭等操作。`SendPaths`、暂停/恢复、目录确认归 files；测速命令归 speed；转发与服务查询归 TCP。信任来源和强撤销归 node。`PeerResources.files/speed` 的持久字段保留在配置 DTO，向能力投影逐 peer 授权。节点验证身份与代际，业务验证具体人、资源和方向。 |
| `node/core/protocol.rs` | 拆成文件载荷与校验、测速载荷与租约、双方共同使用的 P2PF 帧封装。`SpeedArbiter`、`SpeedLease` 属于 speed。用带固定编号的兼容编解码边界承载既有枚举槽位；同一类型流拒绝其他业务消息。测速 ID 可以采用独立内部类型，线上的值、长度和验证保持兼容。 |
| `node/core/transfer.rs`、`transfer_files.rs`、`files.rs`、`drop_send.rs`、`incremental.rs`、`auto_resume.rs`、`publish.rs` | 归 files。入站与出站共享同一个文件服务、队列和持久状态。预览、扫描、路径安全及文件发布在服务端重验。桌面拖放事件在 GPUI，只向服务提交路径；已有共享路径检查可复用无副作用的校验，客户端检查不授予执行权限。 |
| `node/core/task_model.rs`、`task_store.rs`、`task_recovery*`、`task_events.rs`、`queue.rs`、`bandwidth.rs`、`receive_policy.rs`、`space_budget.rs`、`frame_budget.rs` | 归 files，保留恢复、并发缩容、公平分配、磁盘预算、回执及帧预算。若某个帧原语有其他真实调用方，只提取有界读写或租约机制，文件分片及每任务限额留在 files。 |
| `protocol/manifest.rs`、`transfer/`、`storage/` | 将 manifest、分片哈希、`ChunkBitmap`、`PartialDownload` 和文件发布原语放在文件域。独立 CLI 的 sender/receiver 和节点文件状态机各保留原协议。两者确实共用的位图/落盘实现置于文件域的叶子模块；若独立 CLI 会因此带入整个节点，提取小的文件格式/存储库供双方依赖。把 `storage/atomic.rs`、`permissions.rs` 中身份和配置所需的原语留在基础层。这样 `PartialDownload` 与 `transfer::resume` 的双向引用可以消除。`relative_path`、`secure_fs` 按实际调用者放置。 |
| `node/core/speed.rs`、`activity.rs`、`transfer.rs` 中的 `speed_peers`、`FileInlets`、`serve_dispatched`、测速快照 | speed 拥有 peer 列表、租约、唯一 UNI 接收者、控制流和结果。共享的“每 peer 共享/独占活动许可”从 `Activity` 提成节点可提供的小原语，去掉 `TaskId`、文件错误和中文提示依赖。文件持共享许可，测速持独占许可；TCP 保持原并发语义。旧连接清理只能撤销自己的代际/租约，不能误删新连接。 |
| `node/core/services*`、`node/host/forwarding.rs`、`tunnel/{mod,rules,supervisor}.rs` | 将服务目录、按 ID 开流、规则 ACL、受控 TCP 连接、转发监听与所有者归 TCP。两条路径实用的字节桥接及连接配额保留一份。旧 CLI 的 `serve_loop`、`push_file`、裸目标选择、规则重载和管理状态留在 standalone；协议、授权模型和规则形状不同的部分分别保留。 |
| `node/core/config.rs`、`settings.rs`、`node/host/configuration.rs` | 将 `NodeSettings`、`ConfigRecord` 的磁盘表示及全配置事务交给 Agent/共享配置模块；核心只收网络、认证、限额等有效运行参数；各能力持自己的配置和验证。核对 `DesktopConfig`、`SettingsDraft`、测速方向与标签的生产调用，拆开持久格式、业务方向及显示标签。现有进程测试夹具随相应组件迁移。 |
| `node/core/snapshot.rs`、`task_details.rs`、`diagnostics.rs`、`transfer.rs::detail_selection` | 任务状态、路径可操作性、速率和诊断阶段归对应服务；仍需用的筛选、展开、时长格式、中文显示名及重试提示归呈现端。去掉服务端全局选中项。需要查询详情时显式传任务/组 ID。按 ID 定位接收文件仍须在服务端执行路径与权限验证。 |
| `node/api/*`、`node/local/*`、`node/host/{agent,recovery}.rs` | management 提供 IPC v1 DTO、当前用户路径规则及客户端。宿主保有请求生命周期、锁和配置事务。将 `agent.rs` 中各业务命令的执行转给能力句柄；保留同一个请求去重、超时、事件游标和错误投影入口。Agent 私有目录和身份锁由本机宿主与基础设施维护。 |
| `node/adapters/{web.rs,web.html,navigation/}`、`service-navigation.html`、`node/adapters/cli.rs`、`bin/p2p-agent.rs` | 两个 Web 入口归 web-management 的不同子模块。保留 `p2p-node web` 和 Agent 的 `127.0.0.1:3001` 导航；各自路由、权限和页面按现行契约。导航的 SSH/平台桥接只在导航模块。把 `initialize`、`offline_apply` 提入本机宿主操作，供 CLI 和 Agent 使用。导航发现、启动、重建信号及退出由 Agent 入口组装，Python 继续仅单次生成私有页。 |
| `desktop/`、`bin/p2p-desktop.rs` | GPUI 窗口、托盘、文件选择和选中项均在 desktop。按能力呈现操作，由 IPC 返回的状态决定操作可用性。复用已有组件和 `AgentClient`，保留应用退出与 Agent 退出的不同语义。 |
| `node/products/{live,sub2api,recovery}.rs`、`live/` | Live 拥有媒体发布/观看、播放器和其访问限制；Sub2API 拥有其网关/客户端和内嵌配置。将 `authorized_peers`、`log_connection_event`、`stop_control` 中两产品实际共用且语义一致的代码移到共用运行辅助。产品重试决策与 Agent 唤醒/网卡恢复各有触发条件，保留各自策略；共同的状态观测可复用。 |
| `main.rs`、`cli.rs`、`admin/`、`speedtest.rs`、`bin/*`、`build.rs` | 根 `p2p_file` 命令可按网络诊断（`id/stun/discover`）、信令（`signal-server`）、独立文件（`send/recv/push`）、独立服务（`serve/tunnel/forward/rules`）、独立测速（`speedtest`）分模块。`serve` 是多能力组合，继续保留其管理页。`p2p-node` 是管理 Agent 的 CLI，GPUI 与它平行。保留 GUI 打包资源和各产品名称，在目标搬迁时同步 `[[bin]]`、`CARGO_BIN_EXE_*`、`include_*` 和脚本。 |

## 4. 接口和共享状态怎么拆

首遍阅读按真实调用链走一遍：[初始化与入口组装](../src/bin/p2p-agent.rs) → [宿主和 IPC 命令执行](../src/node/host/agent.rs) → [节点控制与文件服务构造](../src/node/core/control.rs) → [协商及会话](../src/node/core/session.rs) → [双向流分发](../src/node/core/dispatch.rs) → [文件与测速的现有组合](../src/node/core/transfer.rs)。配置变更另从[配置事务](../src/node/host/configuration.rs)读到 actor 的 `commit_agent_policy`。这些是基线源码路径，移动后同步修订现行分层文档。

### 4.1 能力装配与连接生命周期

1. 在产品入口明确生成本实例的节点参数和已装配的文件、测速、TCP 句柄。通用 Agent 从有效配置选择能力，Live/Sub2API 维持当前 TCP 组合。分别处理“构建包含”“管理员启用”“双方协商支持”“当前 peer 有权限”四个条件。
2. 从已装配处理器生成本地能力位。当前必需位及 File=1、Service=2、Speed=3 的流编号固定。业务双向流由同一个 dispatcher 读取首字节、限时并投递至有界队列。未协商、未知类型、队列满分别走已有拒绝语义。
3. 只向处理器交付已经认证的 peer、连接代际、已协商能力、撤销/关闭通知、其专属流入口及受控出站开流手段。每条连接由一个所有者收尾。测速独占 UNI 接收；同一业务 BI 队列只可有一个消费者。仅在启用对应能力时创建其任务及资源。
4. 先用现有三种能力实现静态装配。核心可保存“流编号 → 队列”的有界映射或等价的具体结构；注册冲突在启动时失败。保留当前 header 超时、队列上限和取消路径。无需动态库发现、插件市场、消息总线或给每个函数配 trait。
5. 跨能力互斥只有一个原子判定点。原文件扫描/预约流与测速申请并发时，只能有一方成功；许可持有至异步清理完成。撤销与重连时验证旧令牌无法解除新租约。首次验收必须包含双能力同时启用的组合。

### 4.2 配置、授权和事件的写入顺序

- **文件配置的表示与执行分离。** 继续读写现有 `node.json`、`schema_version`、`revision`、`last_request` 及现有字段。完整发行版保持原默认值；裁剪构建初始化时默认关闭未装配能力。记录 `receive_root` 的路径字符串可以是纯配置；`p2p-node init`、`p2p-agent --init/--initialize`、宿主启动和应用配置均不得仅为停用的文件能力创建目录或打开 `tasks.json`。裁剪版本读取到明确要求启用缺失能力的配置，应在写入或启动前报告不支持的能力，不得静默改写配置。
- **保持提交点。** 现有 `host/configuration.rs::commit_settings` 会校验 revision、预备监听、让会话 actor 完成持久化并提交策略，再发布快照/切换监听。拆包时可由上层提供一次性持久化操作和已准备好的策略更新，让同一串行提交点完成写入与发布。磁盘失败时不得扩大授权；遇到已持久化但运行态发布失败，沿用 `recovery_required` 停止并从已提交记录恢复。保持 request ID、摘要去重、重试和配置脱敏。逐字段沿用原在线生效或需重建的行为。
- **授权分两次检查。** node 确认当前连接身份、信任来源、代际；各能力在实际操作时检查当前资源权限。服务只接受 service ID，在开流时解析当前目标和 ACL；禁止缓存目录替代授权。强撤销要终止受影响的活动；普通服务策略修改仍遵照现有新流规则。文件能力保留路径、句柄、空间、回执检查。
- **保留管理客户端可见结果。** `Tasks` 当前同时返回 `tasks`、`queue`、分页、`directory_previews` 和 `speeds`。拆后由宿主分别查询 files 与 speed 再组装旧响应。无文件有测速时，仍能从同一查询读到测速进度，文件列表为空，队列按原有可空语义表示。业务修改命令遇未装配能力时在 IPC 返回稳定错误并保证无副作用；已有成功查询不能被一概改成错误。`StopSpeed.task` 仍接受当前 JSON ID 表示。
- **事件连续性。** 保留 `epoch`、序列、日志下界和 `resync_required` 的关系。节点代际归 node，能力事件在宿主有序聚合；快照和订阅切点必须一致。核心重建、事件积压及两个并发客户端都需覆盖。业务服务提供事实，各前端分别持有选中、展开及筛选状态。

### 4.3 明确可合并与须分别维护的实现

| 可以合并或抽取 | 必须保留的区别 |
|---|---|
| transport 的长度前缀、各协议传入上限；文件域的位图/清单与安全落盘 | P2PN 节点、P2PF 载荷、独立 CLI 控制协议的标识、判别值、握手签名、状态机各自有固定契约。先固定字节向量，不能靠两个新端互通证明兼容 |
| 一个每 peer 资源许可实现；一套基础会话、认证和重连 | 文件与测速分别决定自己的业务许可及命令；关闭文件不应改变测速、TCP 或节点发现 |
| 单组 TCP 转送和流所有权；共用产品关停/事件日志 | 节点按服务 ID 授权，独立 CLI 有自己的目标规则；Live 的只读访问面与通用代理的权限不同 |
| 管理客户端、IPC 封装、当前用户锁和离线初始化 | Web 导航、`p2p-node web`、GPUI、旧 CLI 管理页各保留实际调用方需要的路由与生命周期，不合并为一个全能 HTTP 接口 |

**拆编解码的具体做法。** 先在现有实现上保存并检查握手、测速和文件代表帧的确定性字节；固定版本、字段顺序、postcard 判别值和长度校验。拆开实现时，选用能按原槽位编码/解码的薄适配器：握手适配器暴露身份消息，测速适配器暴露测速消息，文件适配器暴露文件消息。传输层只调用握手适配器，测速独立构建时只编入测速所需的载荷类型。不能直接为拆出来的 Rust 枚举重新派生连续编号。为每种适配器同时验证“旧字节可读、写出与旧字节相同、另一类流上的消息会被拒绝”，覆盖基线已支持的控制消息。固定向量测试归适配器，完整收发测试归对应产品。

## 5. 分阶段施工工单

每阶段有独立产出，按下表顺序提交。大阶段可以再按可运行的垂直切片提交。依赖阶段在提交正文引用实际前序短哈希。每次仅运行与当前变化有关的检查；阶段验收尚未完成时注明实况。

| 阶段 / 前置 | 改动顺序 | 退出条件 |
|---|---|---|
| **S0 固定基线** / 无 | 用 `cargo metadata --locked --offline --no-deps` 记录目标、feature、脚本调用。运行分层脚本记录既有两项；盘点 `accept_bi/accept_uni`、`TransferService`、`MAX_PEERS`、各配置提交入口、各二进制初始化、任务和协议测试。复用已有固定向量与测试；仅在缺少旧版与新版对照时生成脱敏、合成的固定夹具。 | 得到可以重放的节点/CLI 线格式、IPC 示例、配置及断点样本；列出实际运行的基线检查、失败或未跑项。工作树已有修改单独记录。 |
| **S1 修正容量策略** / S0 | 用实例级容量替代 `network_state::MAX_PEERS` 的产品 feature 分支。逐一替换 peer 接纳、重连/恢复、快照截断、短 ID 缓存等实际读点。产品入口选值，`Status.budgets.peers` 读同一实例值。 | 同一次编译实例化通用节点 16、Sub2API gateway 128，其余产品用原预期通用值；分别验证上限、第一个超限 peer、恢复、报告。组合 feature 不能串改实例。 |
| **S2 拆测速并打通无文件节点** / S1 | 先迁 `Activity` 许可和测速 peer 生命周期，再从文件服务取出测速命令、BI/UNI 处理及快照。将 `session` 的能力位和 dispatch 接入改为独立装配，补管理 `Tasks.speeds` 聚合；修复 `initialize` 与 `spawn_with_storage` 的无条件落盘及无条件文件设置调用。此阶段允许仍在同一 Cargo 包。 | 新目录两端都只开 TCP+测速，完成认证、测速、访问 TCP、断连重连；磁盘上没有文件任务库及接收目录。文件+测速组合仍互斥；测速独占期间 TCP 可用，停止后许可归还。 |
| **S3 提取文件能力与协议边界** / S2 | 迁任务/存储和控制器，再迁文件载荷、入站/出站、文件 ACL、队列与恢复。将选中态、列表投影、诊断文案移至客户端。移除通用控制句柄中的文件执行类型；拆握手与独立 CLI 厚枚举。逐步把基础传输、node、files、speed 提为独立包。 | 可单独编译 node 和 speed，生产依赖树中无 files。已启用文件的旧任务库/断点可恢复、正确发布并只写一次回执。固定报文字节双向通过；两个客户端选择不同任务互不影响。 |
| **S4 提取 TCP 与专用产品组合** / S3 | 迁 `services`、按 ID 开流及 forwarding。保留独立 CLI 的旧协议入口。用产品参数组装 Live 和 Sub2API，抽共用运行辅助，清除 `live → sub2api`。 | 通用 Agent 和两种专用产品都能运行 TCP；无授权目标不可打开、修改/撤销规则和身份后效果正确。Live 只发布原授权的服务，Sub2API 容量维持 S1 行为。 |
| **S5 提取管理契约、配置事务和 Web** / S4 | 瘦身 IPC DTO、迁本机离线操作、宿主聚合。拆 `host → adapters`，把两侧启动与重建/退出信号放到 `p2p-agent` 组装处；GUI/CLI/Web 仅依赖管理契约和必要本机操作。原配置事务按第 4.2 节迁移，先保留单一提交点。 | 三平台 `3001` 仍是 Agent 的回环导航，配置缺省、`--no-navigation`、端口占用、核心重建、进程退出均可预测；独立 Web 入口可用。revision 冲突、重复请求、强撤销与写入失败通过现有或针对性测试。 |
| **S6 固化工作区并交付** / S5 | 让根包保留面向发行/脚本的二进制目标；将各入口映射到目标模块，分离 signaling/standalone 的组装以及产品专用依赖。限定 feature 转发；更新架构脚本、进程矩阵过滤器、打包配置、现行分层说明和架构图。保持交互图已有八种主题、亮暗切换和导出 PNG。 | 第 6 节矩阵有逐项证据；Cargo 包图无反向依赖；删除过渡重导出前确认仓内源码、测试、样例和打包均已切换。若仍有明确外部调用方，记录其路径及移除条件。 |

**阶段恢复**。尽量使每个阶段只改实现和依赖方向，持久文件与线上字节不变。后续阶段已依赖前序时，按反向依赖恢复代码并使用隔离数据副本重跑受影响用例。停用文件功能只停止装配；禁止通过清空旧任务、重建身份或修改生产配置完成验收。确需变更线上格式、数据 schema、外部命令或部署进程边界时，记录具体调用方、影响和迁移方案，另行确认后实施。

## 6. 验收矩阵与执行入口

**编译时裁剪和运行时停用分别验收。** 根包默认 `node` 保留原发行组合。当前 `agent-base` 内置 TCP 和测速，按 `agent-files` 和 `agent-web` 增加文件和 Web。`enable_tcp`、`enable_speed` 在运行时独立控制协商与业务执行。`agent-full` 发行 Agent 与管理 CLI，可与 `desktop-client` 分开构建。另行选择 `standalone` 构建原 `p2p_file`。

| 场景 | 具体验证 |
|---|---|
| 最小 node、测速+TCP Agent | `cargo check --locked -p veytrane-node`。单独运行 `cargo build --locked --no-default-features --features agent-base --bin p2p-agent` 与 `cargo tree --locked --no-default-features --features agent-base -e normal,features`。保留构建好的裁剪二进制，然后从默认 `node` 构建运行混合进程测试。生产树无需文件执行、GPUI、HTTP 管理或信令服务端 SQLite。 |
| 双端组合与拒绝 | 裸节点仅协商基础能力；TCP-only ↔ 完整版只允许 TCP；speed+TCP ↔ 完整版可测速；文件关闭仍可查询状态与测速。对未协商流、缺失 ACL、撤销、重复注册、慢首部和队列满分别校验。执行目录使用独立临时根，核对内容、文件副作用及进程退出后的锁。 |
| 线协议与旧数据 | 对照基线固定向量覆盖 P2PN、P2PF、独立 CLI 身份握手和有代表性的业务帧；校验编码、解码、越界和尾随字节。用基线 fixture 加载 `node.json`、任务、回执和部分下载；对停用再启用、进程中断后手动/自动续传、同名冲突各核一次。 |
| 文件与测速并发 | 复用 `speed_and_file_activity_exclude_each_other_without_silent_pause`、`speed_cancel_and_stale_uni_token_do_not_pollute_next_test_or_file_transfer` 和许可清理用例。补覆盖无文件的独立测速、取消与新代际清理；并发两端竞争时只产生一个有效测速租约。 |
| 配置与 IPC | `cargo test --locked --test node_agent_process agent_process_config_transaction_idempotency_restart_and_ownership`；覆盖同 revision 并发、重复请求、落盘失败、重建与 `Events` 重同步。`Tasks` 的旧分页、空集合、预览脱敏、测速字段均可被现有客户端读取。 |
| TCP / 授权 / 产品 | `cargo test --locked --test node_agent_process unified_service_tcp_streams_do_not_exhaust_control_slots`，再按改动运行 `three_agent_processes_unified_files_directory_four_forwards_and_revocation`。Live 与 Sub2API 分别验产品授权集合、重连与关停；`scripts/live-local-smoke.py` 仍可用原 CLI 命令启动测试信令。 |
| Web / GPUI / CLI | 导航测试按变动选 `scripts/navigation-native-smoke.py`、`navigation-platform-tests.py`、`navigation-ui-tests.cjs`；先检查各自参数和范围，再在隔离配置执行。验证 Web 启动失败不影响既有转发，核心重建期间 HTTP 可达，退出释放监听；两个桌面/网页客户端选择互不干扰。GPUI 分别记录已构建与已实际交互验证。 |
| 边界和测试有效性 | 当前 `python3 scripts/check-architecture.py` 仅扫描 `src` 和显式导入。改为同时检查 workspace 包的生产依赖和新的源码目录；不得靠改写 import 让检查假绿。`scripts/node-core-e2e.py` 有 `node::core::` 命令过滤和日志正则，随包移动同步更新；核对发现的测试数、指定 case 名和子进程证据。`scripts/e2e.sh` 属于独立 CLI，保留独立验收。 |
| 发行与跨平台 | 核对现有八个二进制、`--build-info` 的既有消费者、原 CLI 子命令、独立 Web、macOS/Linux socket、Windows pipe 与 ACL、桌面打包和服务启动模板。对 Windows/Linux 未在本机可验证的部分记录对应 CI/实机状态，不以本机编译代替。 |

执行记录至少写明阶段、提交、构建 feature、用例、实际结果与未验项。检查功能时使用隔离的本地数据和回环测试节点；单平台或模拟测试分别标注。每笔阶段提交及推送授权遵循仓库 `AGENTS.md`。

### S0 基线记录（2026-11-20）

- 施工从 `29ad7be` 开始。`cargo metadata --locked --offline --no-deps` 确认根包 `p2p_file` 与八个二进制目标。`check-architecture.py` 在基线返回 1，报告 `host/agent.rs` 直接依赖导航以及 `products/live.rs` 的分组导入。旧的工作树修改单独保留。
- P2PN v2 复用 `frozen_v2_bytes_and_request_ids_are_independent_of_current_encoder`；P2PF 复用 `golden_file_frames_reject_other_protocols` 和追加分片帧固定字节用例。独立 CLI 的握手固定为 v3，新增 `v3_identity_frames_and_signature_domain_match_fixed_bytes` 覆盖四种身份报文和签名载荷。
- `ipc_v1_file_and_speed_requests_keep_their_wire_shape` 固定 `tasks`、`resume`、`start_speed`、`stop_speed` 的合成 JSON 请求。配置复用已存在的 schema 1 校验；任务用暂停重启恢复用例，分片用从持久化 checkpoint 恢复用例。进程级 IPC 响应、授权、网络和跨平台场景留给受影响的实施阶段验证。
- `tests/node_agent_process.rs`、`tests/signaling_tls_cli.rs`、`scripts/node_smoke.py`、`scripts/e2e.sh`、`scripts/live-local-smoke.py` 和 `packaging/RUNNING.md` 均依赖现有 `p2p_file signal-server` 入口。`tests/ipv6_cli_send.rs` 依赖独立收发入口。包拆分时先保证这些消费者还能启动原命令。

### S1–S2 实施记录（2026-11-20）

- `f123e91` 将设备容量改为实例参数。定向用例覆盖 16/128 接纳、恢复与快照。开启 `sub2api-gateway` feature 构建后，通用 Agent 真实进程仍报告 16。128 个真实远端同时接入留待专项验收。
- `4d0cefb` 提取共享/独占许可，`b5b9b31` 延迟创建接收目录和任务锁。独立测速由节点会话装配，Agent 从它读取 `Tasks.speeds`。文件关闭时，文件变更请求返回 `capability_disabled`。
- `file_disabled_agents_keep_speed_tcp_and_reconnect_without_file_storage` 在 macOS 隔离目录启动两个真实 Agent，验证了授权 TCP、测速期间并发转发、测速完成、断开重连，及停止后不存在 `receive`、`tasks.json`、`tasks.lock`。这一结果覆盖运行时停用，编译时依赖裁剪仍属于 S3–S6。

### S3 施工记录（2026-11-20）

- `93fc935`、`4d377ca`、`8caa179` 分别消除位图反向引用、拆分身份握手与独立 CLI 帧、移除服务端全局任务选中态。
- `6bdcc81` 将身份和有界原始帧移入传输包，独立 18 项测试通过。`0a5e35d` 将清单、分片位图和下载存储移入文件包，安全存储组合 61 项测试通过，现有 Agent 文件回执和独立 CLI 收发亦通过定向回归。
- `7940534` 将 P2PN 协商和双向流路由移入节点包，8 项独立用例通过。真实双 Agent 无信令文件、TCP 与撤销回归通过。进程矩阵已经纳入迁移后的 wire 入口，整份矩阵留待最终验收。
- `42e590c`、`16faed5` 将测速窄编解码及租约迁入测速包。`2bf2784`、`56d4051`、`6a99a23` 把活动许可、帧预算与会话撤销原语移入节点包。定向验收覆盖并发、预算释放、重获权限时保留旧流撤销以及测速方向授权。
- `fa6f5e4`、`29bed08` 让实例锁、原子替换与当前用户目录保护可由多个包共用。macOS 真实 Agent 配置事务和所有权回归通过，Windows 仅通过传输包交叉编译检查。
- `bda6a69` 完成测速包的实际执行迁移。`veytrane-speed` 的正常依赖无文件包，独立 CLI 与节点共用同一内存读写计量实现。17 项节点测速、6 项独立测速和双 Agent 无文件运行均通过。
- `56aef36`、`e8a0140`、`49e31c4` 迁移文件任务模型、调度与限流、接收空间预算。macOS 通过相关定向用例。Windows 接收预算的交叉编译在 `ring` 构建处缺少 MinGW Clang，尚待对应环境验证。
- `104c262`、`96c32a8` 把 P2PF 文件编解码和文件帧配额纳入文件包。13 项文件协议用例、独立测速字节对照、错误帧处理及实际接收回执验证通过。
- `b7cd3bf`、`4f36ed4` 迁移持久任务库、事件和启动恢复，保留原公开导入。持久快照、损坏数据与锁定回归通过。真实隔离进程杀掉任一端后均可继续已校验的断点。
- `cb0c80f`、`1737a67` 迁移分片、自动续传计划、扫描、目录比较与接收发布。实际增量收发通过。发布进程在 10 个步骤分别被终止后，恢复均保全旧内容、新内容及唯一完成回执。
- `4d17e22`、`a0d1e2d`、`274af9f` 将任务尝试计量、有界快照事实和路径重验迁入文件能力。快照排序、暂停、继续、回执以及路径替换回归通过，现有管理呈现仍从同一份事实转换。
- `0172ef9`、`b8ad91f` 迁移 v3 身份握手报文、签名载荷与 QUIC 会话绑定。固定字节、原始帧往返、旧 CLI 编解码及真实 QUIC 握手定向验证通过。
- `17e6935` 将文件收发运行时和原真实连接测试迁入 `veytrane-files`。61 项运行时用例通过，涵盖重启恢复、增量、队列、完整性和测速互斥；根双进程暂停、存储故障以及新会话续传回归通过。
- `1ae0129`、`ee337ba` 将 peer 容量、代际和会话鉴权迁入节点包。7 项容量与代际、18 项鉴权及现有会话强撤销回归通过。`3fe7063` 由节点组装共享活动许可、变更通知和帧预算。
- `d5a1406` 将既有文件配置值从执行包分出。`agent-base` 保留 `node.json` 字段，仅链接纯配置规则。根包默认 `node` 继续包含全部原发行入口，独立 CLI 使用 `--no-default-features --features standalone` 构建。
- 已实际构建 `cargo build --locked --offline --no-default-features --features agent-base --bin p2p-agent`。同组合正常依赖树无 `veytrane-files`、GPUI 或 Axum。用 `VEYTRANE_TEST_MINIMAL_AGENT` 指向该二进制运行 `file_disabled_agents_keep_speed_tcp_and_reconnect_without_file_storage`，两个进程分别初始化并通过 TCP、30 秒测速和重连，拒绝启用缺失能力且配置字节未变，停止后无接收目录和任务库。`agent-base,agent-web` 的入口也通过编译检查。
- 完整 `node` 构建的真实 Agent 文件、TCP 与撤销回归通过。显式 `standalone` 构建的 IPv4、IPv6 独立收发通过。混合构建组合、Windows/Linux 实机及完整交付矩阵仍需最终验收。

### S4–S5 施工记录（2026-11-20）

- `b891c3a`、`8045c1e` 将服务目录、按 ID 开流和转发监听事务归入 `veytrane-tcp`。Agent、Live、Sub2API 共享实现。配置事务、三进程四路转发及撤销、Live/Sub2API 产品和恢复测试通过。隔离 Live 测试通过 viewer 权限限制、直连播放和发布端重启后的恢复。既有已部署产品授权未在此次隔离验证中读取。S4 已同步 `origin/main`。
- `879e613` 将 IPC v1 DTO、客户端、当前用户路径及 socket/pipe 纳入 `veytrane-management`。IPC JSON 样本、实际 Agent 配置事务与重建、Windows GNU 管理包交叉编译通过。Windows pipe 的实际 ACL 仍需在 Windows 验证。
- `b09eb5d`、`1c3dd72` 分离 Web 鉴权和 `apps/web-management`。Web 包 24 项测试通过。macOS 原生导航 smoke 验证核心重建期间 HTTP 存活、身份与配置字节保留，另验导航配置默认发现、`--no-navigation`、端口占用和独立 Web 带 token 操作同一 Agent。
- `f0a5fa8` 让配置记录及转发适配归 `node/composition`。宿主把已验证的配置投影为运行策略，并提供一次性持久化操作。会话 actor 在同一序列位置验证授权代际、落盘后发布。真实进程测试覆盖模拟写入失败、revision 冲突、去重、重建和事件重同步。S5 已同步 `origin/main`。

### S6 施工记录（2026-11-20）

- `9878798`、`2dc4686` 拆开短设备 ID 与 SQLite 注册，独立 `signaling-server` feature 持有服务端注册、TLS 监听和 Relay，沿用 `p2p_file signal-server`。发现、信令以及 TLS 共 76 项，Relay 24 项，独立 CLI TLS 进程测试通过。
- `b1a3c68`、`ee14ab2` 保留 `node` 与 `gui` 的原组合，新增 `agent-full` 和 `desktop-client`。Live、Sub2API 可单独构建。桌面依赖 IPC 客户端与纯校验，独立桌面编译通过。Linux、macOS、Windows 原包装脚本分别构建 Agent/CLI 和桌面，macOS 本机通过包装脚本语法与 8 项归档检查。
- `scripts/check-architecture.py` 检查源码和生产 Cargo 依赖。`agent-base` 内置 TCP 与测速，排除文件运行包、Web、GPUI 和 SQLite；`agent-full` 包含 Web 和文件，排除 GPUI 和信令服务端 SQLite。独立桌面、Live 和 Sub2API 均排除文件执行。实际依赖验证以脚本中的展开组合为准。
- `84f97e8` 将独立 CLI 进程测试改为动态回环端口。macOS 上 IPv4、IPv6 和强制 Relay 均通过，含 TCP 并发、测速、文件哈希和重新连接。`5cce30d` 将信令服务端、Live 和 Sub2API 的装配移入 `apps/` 各目录，并保留八个根发布入口。完整组合八个目标通过编译，三个管理相关二进制的实际 `--build-info` 均能读取。
- `1f60754`、`07fc4ad` 验证双裁剪 Agent、TCP+测速裁剪端对完整版、仅 TCP 端对完整版、仅基础能力端对完整版。前两种完成 30 秒测速、并行 TCP 与重连。裸节点仍能认证和重连，保存的转发不会开监听，未协商的目录、测速和文件命令返回拒绝。`2fe610c` 验证两个 IPC 客户端并行请求不同任务，各自得到原接收位置和调整后的接收位置。
- `a7f9e9c` 用 `0b0d77b` Agent 实际传输并生成旧任务。裁剪 Agent 在停用期间保留任务库原始字节及收到的文件。完整版重新启用后可按原 ID 查到已完成任务。运行 `file_disabled_agents_keep_speed_tcp_and_reconnect_without_file_storage` 可用 `VEYTRANE_TEST_MINIMAL_AGENT` 指定独立裁剪版，`VEYTRANE_TEST_AGENT_MIX` 选择 `minimal-both`、`minimal-client-full-server`、`tcp-client-full-server` 或 `bare-client-full-server`。运行 `completed_file_task_survives_files_disabled_then_full_reenable` 可加 `VEYTRANE_TEST_BASELINE_AGENT` 指定施工前二进制。
- macOS 上 `cargo test --locked --offline --test node_agent_process` 的 9 项测试通过。最终在 `5a2a4fe` 运行 `scripts/node-core-e2e.py`，14 个场景通过并采集 21 条进程证据。真实发布中断恢复覆盖 10 个步骤。`514527d` 的生产依赖检查在 Cargo 默认和离线两种模式分别通过。
- [现行交互图](layer-review.html) 的八主题、亮暗状态与 PNG 按钮完成生成和静态核验。浏览器审批拒绝了打开本地架构图的操作，点击、视觉布局及实际下载尚待确认。
- 从干净的 `2aad410` 工作树在 Apple Silicon 执行真实发布包装。ZIP 包含桌面、Agent 和管理 CLI，三者源码提交一致，两个管理端报告 IPC v1。ZIP SHA-256 为 `2fc71cf3731d86831043240d7957faeda88f3a5f66f25b02d55e71fdf31bdb44`。候选包为 ad hoc 签名且未公证。包内 Agent 通过隔离导航测试，核心重建期间连续 11 次读取页面成功。包内 GPUI 实际打开工作台、设置、诊断并读到同一隔离节点，退出验证另行记录。
- `dc14b5f` 将 GPUI 退出快捷键接到既有托盘退出逻辑。Apple Silicon 使用 `desktop-client` 原生构建，经隔离 `.app` 包装激活主窗，按 `⌘Q` 后桌面进程自行结束。IPC 复查 Agent 的代际与 revision 保持原值。随后通过 IPC 停止隔离 Agent 并核对进程所有权已释放。桌面自身退出码未采集。
- `6a4a8d1` 的 3 条 BI 分发器回归通过，覆盖待分类流上限、慢头超时、业务队列独立上限和容量归还。`d465060` 增加真实 Windows 管道初始与后继实例的 ACL 验证、同用户往返与释放验证。管理包在 Windows GNU 目标通过测试编译。
- `bbcc4f9`、`3baf0e9` 将完整发布包、原生导航、Linux Unix IPC 和 X11、Windows 命名管道与接收存储验收纳入 GPUI 工作流。首次运行 `44750655733` 在 Windows 遇到 Cargo 元数据解码失败，Apple Silicon runner 已退役。`f82f4f3` 明确使用 UTF-8，`e3ae2f7` 更新为 `macos-15` ARM64。
- 复验 `44751160046` 的 macOS Intel、Apple Silicon、Windows 和 Linux 均通过原生导航和隔离核心重建。Windows Server 2022 另通过命名管道及接收存储步骤。Linux 跨 UID 脚本的 `nobody` 无权穿越 runner 私有目录执行二进制。`ce1df5a` 改为在脚本原有隔离目录内执行，待原生复验。以上是工作流已完成步骤，打包结论单独记录。
- Intel macOS 的 Agent/管理 CLI 已通过交叉编译检查。macOS 上两个 Windows 宿主管理路径的模拟用例在施工前 `0b0d77b` 同样失败。专用 Sub2API 隔离用例通过，部署用例按设计跳过。
- `383dc9a` 将生产服务构造移到 `node/composition/runtime.rs`。Agent、Live 与 Sub2API 通过该入口启动，传入既有的共享活动许可和帧预算。原内部 Rust 入口迁到组合层，现有二进制入口沿用。`9b654cc`、`c8f24e6` 让会话只为已协商且已安装能力注册有界队列，保留裸会话的拒绝及取消行为。七条分发器用例以及四组裁剪版/完整版实际互联与重连通过。
- `47e542e` 将 `Tasks` 的 v1 JSON 投影移到 IPC 宿主直接使用文件包事实。原字段、分页及可用操作通过文件、两客户端定位及无文件测速组合回归。根会话仍协调固定的三类能力调用、生命周期及授权；TCP 和测速独立运行启停，Agent 编译裁剪针对文件、Web 和信令服务端。
- `bab39c9`、`ff29a8e`、`91f4f63` 退出生产路径上的旧任务筛选、展开、详情、显示文案和桌面草稿。任务定位仍按明确任务 ID 由文件包重验。旧源码保留为回归夹具。`2196bf5` 将预览、确认、发起发送和按方向续传的执行交给文件包，沿用会话授权与命令队列。增量、取消、持久回执和三进程用例通过。
- `7c5d14a` 在 IPC v1 `Status` 增加本机当前能力，裸节点和无文件测速节点的真实进程断言通过。`7d2e6b7` 将 GPUI 的设备状态、文件入口与测速入口分开。四项工作台定向用例及独立桌面构建通过，继续兼容仍使用旧 `Status` 和 `Tasks.queue` 的 Agent。
- `44753699993` 在 Ubuntu 24.04 通过真实 Unix 跨 UID 拒绝和同用户复查、X11 隔离托盘隐藏与恢复、状态窗口及 `Ctrl+Q`。严格 Clippy 报告导航与文件代码的 lint。`8134dfc` 将候选包生成和核验放在该检查前，保留工作流失败状态及上传门槛。`44751160046` 的 Apple Silicon 原生包装及校验通过，干净候选包 SHA-256 为 `a189059ac4a959c828d33ee8ceed9c3c0384bdf7ef59ff566e7de4ca8993428a`。
- 同次 `44751160046` 在 Windows Server 2022 实际运行命名管道 DACL、同用户往返和释放。按打开目录探测磁盘空间、重解析路径拒绝、占用目标后的重试三项也逐条通过。Windows x64 原生完整候选包通过校验，SHA-256 为 `a4e0b502dcf12dda4c0babe76db620672fcd07c252d24d55234659c8c92d5133`。Windows 交互式桌面托盘仍待实机。
- `c670aa6` 将旧 `DesktopConfig` 和旧信任编辑写盘限定在回归测试。Agent 生产配置使用 `ConfigRecord` 的同一事务；22 项配置、55 项会话及真实 Agent 事务与撤销回归通过。
- `7d2e6b7` 的 macOS 窗口复验使用两个已互相认证的无文件节点。API 确认文件关闭、测速与 TCP 已装配，桌面进程抽样运行在 AppKit 事件循环。窗口读取两次超时，文件区显隐、点击测速和断开结果尚未观察。两隔离 Agent 已经正常退出且所有权释放。

## 7. 新增或移除业务时的操作示例

**新增一个使用既有 TCP 的产品**：在 `apps/<产品>` 内确定对端、服务 ID、目标与可见目录；使用 node 和 TCP 的公开接口组装；在产品自己的 UI 或 CLI 接入管理命令；测试未授权访问、撤销、重连和关闭。Live 是这种模式的现有实例。

**新增需要自有对等消息的能力**：先定义对端如何协商、编号如何分配、消息大小、授权和取消语义。新增 `crates/veytrane-<能力>` 的处理器、控制句柄和状态，在产品入口按需注册；确需对外管理时扩充有版本约束的管理契约。用“新端 ↔ 缺此能力的端”和“两个均支持的端”分别测试。核心改动限于确有需要的基础协议保留位、共用传输机制及相应规范。

**撤下文件传输**：先在目标产品装配中取消 files，维持已有身份、配置、旧任务和接收目录；移除产品 UI 的文件入口，验证无文件矩阵。若同时要取消 `p2p_file send/recv/push` 的发行，再盘点独立 CLI 用户和打包脚本，单列下线决定。信令、测速和授权 TCP 的生命周期均可独立继续。

## 给施工模型的首条指令

> 以本文 S0 为起点，核对当前工作树与基线，保留其他任务的未提交修改。依次施工 S1–S6，每阶段只纳入本阶段变动、定向验证并本地提交，正文记录依赖提交和验证结果。最先交付能运行的 TCP+测速无文件节点。默认发行行为、线上协议、数据格式及同进程 `3001` 导航按本文保持；遇到真实契约冲突，请列出调用方与可选迁移方案再决策。最终用第 6 节矩阵交付证据。
