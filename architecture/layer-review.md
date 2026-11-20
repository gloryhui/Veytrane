# Veytrane 分层审查

2026-11-20，基于 `main` 的 `0b0d77b` 与当时的工作树。此文记录施工前的审查。检查对象包括 Cargo 构建目标、生产调用路径、条件编译、核心状态归属及分层检查脚本。[现行交互图](layer-review.html) · [施工前 PNG](layer-review.png) · [现行目录说明](../docs/REPOSITORY_LAYOUT.md)

## 施工后的边界

通用 Agent、Live 与 Sub2API 都会装配节点会话，各自选择容量和接入策略。对等端通过认证后才能进入节点流分发，文件、测速和 TCP 分别应用各自的资源授权。三类能力的执行包可以分别检查，文件能力可以从 Agent 二进制排除。[当前施工与验收记录](node-modularization-plan.md)

| 范围 | 现行归属 |
|---|---|
| 节点与网络 | `veytrane-node` 提供协商、认证、peer 容量、代际和有界流分发。`veytrane-transport` 提供身份、QUIC、原始帧、打洞与 UDP 地址族规则。根会话仍负责网络切换和业务连接的生命周期。 |
| 文件任务 | 文件包持有任务、接收根、队列和恢复。Agent 直接读取同一个文件服务的快照和执行本地任务操作。需要当前对端授权的命令仍由会话 actor 串行处理。客户端各自管理选中项。 |
| 管理和导航 | GPUI 窗口和托盘位于 `apps/desktop`。Agent 入口装配 `apps/web-management` 的可选导航。Web、GPUI 与管理 CLI 使用 IPC v1，内置导航继续在 Agent 进程内提供回环 HTTP。CLI 通过宿主的专用本机入口进行离线初始化和配置。 |
| 独立命令 | `apps/standalone-cli` 编排旧协议的文件、隧道、服务和测速。`apps/signaling` 提供独立信令服务端，继续由 `p2p_file signal-server` 启动。两者与管理 Agent 的命令属于不同入口。 |

## 从基础设施到应用

| 层次 | 现有代码和职责 | 边界判断 |
|---|---|---|
| 身份、网络、传输、存储 | `identity`、`discovery`、`nat`、`net`、`relay`、`transport`、`protocol`、`storage` 提供签名、发现、路径建立、QUIC、帧、文件清单和落盘能力。 | 网络建连可以组合多个底层模块。存储和文件传输之间有一处双向模块依赖，见下文。 |
| 节点业务核心 | `node/core` 管理会话、认证授权、服务协议、文件任务、队列、恢复和测速。`NodeControl` 提供操作入口。 | 授权判断、任务状态转换、路径安全和资源限额执行属于核心。产品选择哪种限额、窗口选中了哪条任务应由上层决定。 |
| 管理契约和本地通信 | `node/api` 定义请求、响应和客户端，`node/local` 处理本机路径、Unix socket 与 Windows 命名管道。 | 进程间调用确实经过 IPC。桌面源码也直接复用 `TaskId`、服务 ID 和拖放路径校验等核心类型、函数，因此源码依赖比进程调用图更宽。 |
| Agent 宿主 | `node/host` 创建节点、监听管理请求、协调配置版本和转发监听。找到导航配置时，Agent 在同一进程中提供导航 HTML 与 API。 | 核心重建与导航 HTTP 有不同的生命周期。具体导航实现的启动位置形成了反向引用。 |
| 管理应用 | `desktop` 处理窗口、托盘、文件选择和视图状态。管理 CLI 和 `p2p-node web` 调用同一个 Agent。浏览器导航由 Agent 的 `adapters/navigation` 提供。 | 改桌面交互从 `desktop` 入手。改业务规则从 `core` 入手。两个 Web 入口由不同进程提供。 |
| 专用产品与独立 CLI | `products/sub2api` 与 `products/live` 直接创建核心实例，复用宿主转发、恢复等能力。独立 `p2p_file` 有自己的文件收发、隧道、管理入口与信令服务端。 | 不能假定所有可执行程序都经过 `p2p-agent`。直播产品另有对 Sub2API 授权设备枚举、事件日志和停止函数的横向依赖。 |

现行入口可从 [Agent 启动](../apps/agent/src/application.rs)、[IPC 客户端](../crates/veytrane-management/src/client.rs)、[桌面工作台](../apps/desktop/src/agent_shell/workbench.rs)、[专用网关](../apps/sub2api/src/runtime.rs)、[直播产品](../apps/live/src/runtime.rs) 和 [独立 CLI](../apps/standalone-cli/src/application.rs) 追踪。

## 应优先处理的边界

| 优先级 | 已核实的行为和影响 | 建议及完成标准 |
|---|---|---|
| 高 · 产品策略进入核心 | `network_state::MAX_PEERS` 根据 `sub2api-gateway` 编译特性在 16 和 128 间切换。会话、重连和状态投影均使用它。若用该特性同时构建通用 Agent，核心上限会变为 128，但 `Status` 仍固定报告 16。这个差异由源码可推出，实际部署二进制的构建特性需单独核实。 | 由各入口在创建节点时选择容量，核心统一执行，状态接口读取同一有效值。验证同一次构建中的通用 Agent 为 16、专用网关为 128，并核对实际接纳、恢复和对外报告。见 [核心上限](../src/node/core/network_state.rs)、[会话恢复](../src/node/core/session/recovery.rs)、[状态接口](../src/node/host/agent.rs)。 |
| 中 · 客户端选中项进入文件服务 | `TransferService` 保存 `detail_selection`，`ui_snapshot()` 在查询任务时读取该状态。`NodeControl::task_snapshot()` 与 Agent 的 `Tasks` 使用该快照。当前桌面点击任务时另行设置 `ui.selected`，然后按 ID 从响应中查找详情。仓内直接设置核心选中项的调用可追到测试，未发现桌面经 IPC 写入它。 | 让客户端持有选中项。列表查询返回任务事实，确实需要单独详情时按请求传入 `TaskId`。保留任务授权、路径查验、计量和磁盘访问。完成标准是两个客户端能独立选中不同任务，列表查询无需读取全局界面状态。见现行[文件服务](../crates/veytrane-files/src/transfer.rs)、[控制接口](../src/node/core/control.rs)、[工作台](../apps/desktop/src/agent_shell/workbench.rs)。 |
| 中 · 视图投影和文案进入核心 | `Snapshot::filtered_list` 接收展开集合并决定是否显示子任务，`GroupRow::label` 拼装中文列表标题。`diagnostics` 还保存“在主界面重试”等提示。代码存在于核心，但核心列表投影在当前仓内可见的直接调用主要是模块内部和测试。当前工作台自己处理筛选。 | 核心保留任务分组关系、状态、诊断阶段及允许操作。显示顺序、展开与筛选、翻译及重试提示归各呈现端。先核实已有 API 和调用者，再移动仍需使用的投影。见现行[任务快照](../src/node/core/snapshot.rs)、[诊断模型](../src/node/core/diagnostics.rs)、[工作台列表](../apps/desktop/src/agent_shell/workbench.rs)。 |
| 中 · 宿主引用具体导航适配器 | `host::agent::run_integrated` 直接调用 `adapters::navigation::serve`。导航本机命令通过 `AgentClient` 回到 IPC。部署上的同进程和代码上的反向依赖是两个独立问题。 | 把组装放到 Agent 可执行入口或专用启动模块，由它组合宿主与导航。继续使用同一进程，覆盖导航关闭、核心重建期间 HTTP 可用以及退出时释放监听三种场景。见现行[启动组装](../apps/agent/src/application.rs)、[宿主](../src/node/host/agent.rs)、[导航桥接](../apps/web-management/src/navigation/platform.rs)。 |
| 较低 · 存储与传输存在类型回环 | `transfer::receiver` 调用 `storage::PartialDownload`，后者持有并在公开方法中返回 `transfer::resume::ChunkBitmap`。两处属于同一 Cargo 包，模块级回环不触发 Cargo 的包循环检查。 | 如果要独立复用或拆包，将位图归入双方都能依赖的分片数据模块。先盘点公开路径的调用者，验证位图字节格式、旧断点恢复与原子发布。见 [接收端](../src/transfer/receiver.rs)、[下载存储](../src/storage/mod.rs)、[位图定义](../src/transfer/resume.rs)。 |

## 两条跨模块的实际调用

- **节点转发与独立 CLI 共用 `tunnel`。** `node/core/services` 调用 `bridge_tcp` 和 `connect_allowed_target`，`node/core/session` 把按服务 ID 开流的回调传给 `tunnel::forward_with_owner`。同一模块也有独立 CLI 使用的 `serve_loop` 和 `push_file`。后续可将共享监听、流所有权与字节转送放在中性模块，将 CLI 的命令编排留在应用入口。移动时覆盖节点按服务 ID 转发和独立 CLI 的目标选取两条路径。[节点服务](../src/node/core/services.rs) · [节点会话](../src/node/core/session.rs) · [隧道模块](../src/tunnel/mod.rs)
- **身份握手与独立 CLI 业务消息共用线上编码。** `transport/handshake` 读取 `ControlMessage::Hello` 等消息，同一枚举还定义文件清单、隧道和测速消息。`protocol/message` 已记录文件完成语义变更带动协议版本、握手域升级。先明确握手子集和调用接口，涉及序列化时用已知字节样本验证消息编号、版本和签名载荷，再决定是否拆类型。[握手](../src/transport/handshake.rs) · [消息与版本](../src/protocol/message.rs) · [帧编码](../src/protocol/frame.rs)

## 分层检查结果

`cargo metadata --locked --offline --no-deps` 确认当前是一个 Cargo 包、八个二进制目标。`python3 scripts/check-architecture.py` 返回 1，报告两项。第一项是 `src/node/host/agent.rs:501` 的 `host → adapters`。第二项是 `src/node/products/live.rs:4` 使用 `crate::node::{...}` 分组导入；展开后指向 `core` 和 `host`，两者都在产品层许可范围内。后者属于检查脚本对导入写法的要求。

脚本按显式路径匹配。它能指出某些模块引用，却无法判断 `detail_selection` 应归谁、某段文字属于哪个客户端、feature 是否改变了通用 Agent 的容量，或共享类型是否放错位置。以上判断均另行追到了字段、调用方或构建条件。[分层检查脚本](../scripts/check-architecture.py)

## 查看与重建

打开同目录的 `layer-review.html`。在图表工具栏通过「主题样式」选择暗夜终端、晒图蓝图、牛皮纸制图、白板手绘、粗野撞色、荧光终端、柔光卡片或等距沙盘。使用「亮色」「暗色」独立调整配色，再点「导出 PNG」保存当前样式和配色的完整图。

`layer-review.png` 是施工前的暗夜终端暗色版。修改 `layer-review.spec.json` 后，在装有 arch-diagram 和 Node.js 的开发机运行 `node architecture/build-layer-review.mjs` 可重建现行交互页。页面使用技能自带的 PNG 渲染器在浏览器本地生成当前样式的图片。
