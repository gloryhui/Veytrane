# Veytrane 默认端口转发与可选文件传输施工方案

基线为 `8ecd261`。本方案调整默认发行组合和文件应用的装配边界。端口转发随标准 Agent 发布，原生文件传输继续维护，由用户显式选择文件增强版。标准版保留设备管理、测速诊断和现有本机导航。节点核心继续负责设备身份、可信关系、能力协商、建连、重连和撤销。

## 1. 现状与改动边界

| 位置 | 当前行为 | 施工目标 |
|---|---|---|
| [Cargo.toml](../Cargo.toml) | 默认 `node` 带 `agent-full`、`standalone`、`sub2api-runtime`。`agent-base` 已有 TCP 和测速，`agent-full` 加入文件及管理入口。`desktop-client` 自身引用文件策略包。 | 默认组合排除文件执行、文件策略和界面。增强版的 Agent、桌面和 Web 通过显式 feature 装配。旧独立 CLI 和专用产品显式构建。 |
| [节点装配](../src/node/composition/runtime.rs) 与 [Agent 宿主](../apps/agent/src/host/agent.rs) | 文件运行时已有独立 crate。开任务库、接收目录、IPC 动作、任务快照和配置事务仍跨多个目录。 | 文件包提供运行、配置、IPC 和界面的入口。宿主装配可选能力，继续提供统一生命周期、IPC 和配置事务。 |
| [GPUI 工作台](../apps/desktop/src/agent_shell/workbench.rs) 与 [Web 管理](../apps/web-management/src/web.html) | GPUI 首页以收发和任务为主，已有服务及转发管理；Web 同页含转发与文件操作。 | 首页优先设备、远端服务和转发。文件的菜单、窗口、拖放、任务、设置及 Web 面板由同一个包提供。 |
| [桌面打包](../scripts/package-desktop.py) 与 [节点安装](../scripts/node-host.py) | 打包按三个二进制的 SHA、目标、版本和 IPC 校验；安装目录以 SHA 为键。 | 打包和安装另外校验发行组合。同一提交的标准版与文件增强版可各自安装、切换和回退。 |

## 2. 目标构建和入口

复用现有二进制名称。下列是施工后的 feature 合约。

| 使用方式 | Cargo 组合 | 运行入口与内含能力 |
|---|---|---|
| 默认 Rust 构建 | `default = ["agent-standard"]` | `p2p-agent` 和 `p2p-node`。新节点默认启用 TCP，提供设备、服务授权、转发、测速和管理。 |
| 最小无界面 Agent | `--no-default-features --features agent-base --bin p2p-agent` | 保留 TCP、测速和本机管理 IPC，HTTP 管理由另外的管理客户端提供。 |
| 标准桌面包 | Agent/CLI 用 `agent-standard`，桌面用 `desktop-client` | 三个二进制同包，桌面首页展示设备、服务和转发。 |
| 文件增强桌面包 | Agent/CLI 用 `agent-full`，桌面用 `desktop-files` | 三个二进制同包，安装后明确启用文件能力，文件入口由本机和对端的实际能力决定。 |
| 旧独立命令与信令 | `standalone` 构建 `p2p_file`；`standalone-files` 增加旧文件命令 | `p2p_file signal-server` 沿用命令、参数和注册库。旧 `send`、`recv`、`push` 及 `serve` 的文件分支由文件 feature 装配。 |

- `agent-standard = ["agent-base", "agent-web", "node-cli"]`。保留内置回环 `3001` 导航及独立 `p2p-node web` 两个现有入口。标准构建保留导航的映射、服务和 Agent 管理。
- `agent-full = ["agent-standard", "agent-files"]`。`desktop-files` 叠加 `desktop-client` 和文件包的 GPUI feature。`gui` 调整为标准桌面开发组合，既有显式 `node` 组合继续覆盖完整回归。旧 CLI 的文件用例改为显式 `standalone-files`。`live`、`sub2api-client` 和 `sub2api-gateway` 继续使用各自的构建入口。
- `p2p-agent` 的最低编译条件继续为 `agent-base`。文件增强版新建节点也以 `enable_files=false` 初始化，用户经现有配置重建事务启用后才打开任务库、接收目录及协商能力。沿用当前 `enable_tcp`，保留原来明确关闭 TCP 的节点设置。初始化时不自动发布任何服务或创建转发规则。
- Agent/CLI 与桌面使用两次独立、显式的 `--no-default-features` 构建，标准版与增强版使用不同输出目录，避免 Cargo feature 合并及同名产物覆盖。先让对应的新构建在 CI 成为必过项，最后切换 `default`。

## 3. 文件应用收口到一个 Cargo 包

沿用 `crates/veytrane-files/`。`runtime`、`management`、`desktop`、`web` 和 `legacy` 是包内可按需编译的代码边界，沿用已有 `node-runtime` 等 feature 能表达的语义。Agent 的运行组合不拉入 GPUI 或 Web。桌面的文件组合只带文件 UI、IPC 客户端及输入校验，不拉入收发运行、多平台安全存储和接收任务恢复。Web 组合使用文件包提供的页面片段、事件脚本及契约。

| 源码 | 交给文件包的内容 | 调用边界 |
|---|---|---|
| `crates/veytrane-files/src/`、[veytrane-file-policy](../crates/veytrane-file-policy/src/lib.rs) | 原执行代码、路径批次、限速、并发和空间策略、任务队列、目录预览、恢复、收件及本地文件操作。将 `veytrane-file-policy` 合并为包内轻量配置模块。 | 核心网络配置与转发配置无文件策略依赖。 |
| `src/node/core/{session,control,config,task_details}.rs` 及文件专用分出口；`src/node/composition/{runtime,business,settings}.rs` | 文件命令的类型、参数校验、执行、应用事件、任务投影，文件配置和接收存储的准备与提交。 | 节点 actor 仍串行核对授权代际、协商结果和会话所有权。文件包通过小型会话接口提交操作，复用有界排队和会话取消能力。根包通过装配注入句柄，禁止文件包反向依赖根包。 |
| `apps/agent/src/host/agent/files.rs`、`agent.rs`、`configuration.rs`、`crates/veytrane-management/src/protocol.rs` | 文件动作与查询的请求体、响应、`FilePage`、预览缓存、分页和脱敏；文件配置热更新回调。 | 管理层保留 IPC v1 封装、认证、帧限制、查询与变更分类、请求去重及配置 revision。对外沿用原命令名和 JSON 结构，文件请求只在装配点分派。 |
| `apps/desktop/src/agent_shell.rs`、`agent_shell/{workbench,windows}.rs` | 文件/目录选择、拖放和增量发送、收件设置、任务列表与详情、目录预览与确认、历史清理、暂停和续传。 | 桌面外壳负责主窗口、系统托盘、键盘导航、通用组件、设备选择及基础 IPC。文件入口通过窄的挂载函数取得选中的设备和 IPC 客户端。需要复用的样式与组件通过现有 GPUI 类型或外壳传入，禁止模块引用 `AgentShell`。 |
| `apps/web-management/src/{web.rs,web.html}`、`apps/node-cli/src/commands.rs` | Web 的文件动作、任务面板、前端事件绑定，以及管理 CLI 的文件子命令和参数解析。 | Web 复用现有访问校验、同源请求、token、revision 与不确定结果重试。基础 Web 只保留服务、转发及公共状态。默认 CLI 帮助里只有编入的命令。 |
| `apps/standalone-cli/src/application.rs`、`src/transfer/`、`src/tunnel/mod.rs`、文件专用 `src/storage` / `src/protocol` | 迁入旧文件命令的业务体、接收分支及文件专用存储。使用包内现有清单、分块、校验和续传代码。 | 保留旧 P2PF 文件格式及调用行为，统一节点侧继续使用现有 P2PN 文件流。共享通道、身份、TCP 监听和信令留在各自基础模块。 |

文件应用复用 `veytrane-transport` 的帧与标识、现有安全文件基础设施、`veytrane-node` 的授权、流分类、限额与连接生命周期。端口转发继续由 `veytrane-tcp` 提供。线路编号、能力位和跨应用共享的活动许可留在基础层。它们有对端互操作和资源调度的实际调用方。文件包缺席时仍能识别历史文件流类型，直接按未协商能力拒绝，避免破坏同一连接上的 TCP。

禁止为搬运代码而复制通道、另建 QUIC 连接、缓存一份可信设备表或绕开连接 actor。移植文件命令时核对开始、完成、恢复三处的授权和连接代际。任务操作、撤销和重连要保持可观察行为。保持既有磁盘格式、校验与断点恢复语义。对于已无调用的文件重导出，在迁移调用者和测试后随同阶段清理。

## 4. 配置、IPC 与界面行为

1. **文件数据。** `node.json` 继续沿用现有 revision 和字段形状。文件包拥有 `receive_root`、`send_concurrency`、`auto_resume`、`receive_safety_mib`、`file_limits` 的类型及校验。Agent 配置装配层只保留已有文件配置的识别与原样传递，用于同一节点在不同发行组合间切换。标准版保存转发配置时要保留已有文件字段字节语义，阻止通过配置接口启用缺席的能力或编辑其设置。传输任务库、锁、暂存文件及接收数据在标准版启动和停止期间保持原样。
2. **已启用文件的节点。** 当前校验会拒绝裁剪版加载显式启用了文件的配置。升级预检应在停止旧 Agent 前检测，提供两种明确路径：选择文件增强版并保留原设置，或先通过旧 Agent 的 revision 事务显式关停文件，再切换至标准版。若有运行中的任务，先等待完成或走现有暂停与恢复流程。切回增强版使用相同身份和数据目录，由用户启用后恢复原任务。脚本不得静默改写 `enable_files` 或清理文件数据。
3. **管理契约。** 沿用 `Status.capabilities.files` 的生效语义，并由 Agent 另报已编入但未启用的能力，供增强版管理端决定是否提供启用入口。沿用文件方法名、旧 `Tasks` 分页及测速字段。标准版保留旧客户端需要的空任务页及 `capability_disabled` 错误。用当前发布的客户端夹具锁定这些兼容行为。默认构建的兼容层只识别旧方法名和空状态结构，文件 DTO、执行、数据读取和路径校验都由文件包提供。HTTP 和 IPC 经同一个宿主分派，共用现有帧大小、请求重试及能力校验。
4. **GPUI。** 将现有服务目录和转发管理入口提到首页。保留设备连接状态、服务 ID、目标端、监听端、配置启用和实时监听状态，沿用现有 revision 草稿和启动/停止动作。标准版移除文件菜单、快捷键、拖放接收器、文件专属窗口和文件任务轮询。增强版在设置提供启用入口，只在 Agent 报告能力已生效后挂载文件工作台。发送动作经当前连接协商和授权。关闭能力后收起面板、终止文件轮询并清掉未提交的界面选择。
5. **Web 与 CLI。** 默认 Web 只渲染现有节点和转发表单，文件增强版按 feature 引入文件资源，再以运行时能力显示。文件与测速的共用权限区只在增强版呈现文件授权项，默认版保存时保留已有授权的文件字段。发送和预览的本地路径语义保持现状，相关提示随文件面板迁移。默认 CLI 保留 `status`、连接、`services`、转发启停、配置与 `web`。直接调用旧文件子命令的脚本继续使用增强版 CLI。仍通过 IPC 发送历史文件请求的客户端收到明确的能力错误。

## 5. 打包、安装和升级

- 改 `packaging/macos-{arm64,x86_64}/build.sh`、`packaging/linux-x86_64/build.sh`、`packaging/windows-x64/build.ps1` 及 `packaging/deploy-windows.bat`。默认打标准版，显式选择 `--with-files`（PowerShell `-WithFiles`）才打增强版。桌面包仍包含桌面、Agent 和 CLI，同一版本的三者整体发布。
- `scripts/package-desktop.py`、`verify-desktop-package.py` 及依赖/许可证生成同时读取真实构建组合。`--build-info` 用有值域约束的 `edition`、能力角色及现有 SHA、目标、IPC 信息验证包内一致性。标准/文件增强归档、校验和、候选清单和原生签名证据使用可区分的文件名及输出目录。默认候选跟随原发布渠道，增强版使用明确标注的独立资产。
- `scripts/node-host.py` 将新候选的不可变安装键扩为提交和发行组合，沿用 `installed.json` 的真实路径进行定位，继续识别已安装的 SHA 目录。允许同一提交的两个组合并存。安装预检核验 Agent/CLI 的组合、SHA、版本和 IPC，保留既有启动项、数据目录、授权与回退记录。部署换版沿用现有停启和就绪检查，回退读取已核验的旧路径。
- 更新 `.github/workflows/gpui.yml`、`windows.yml`、`scripts/check-architecture.py`、发布说明及仓库架构索引。将信令部署示例改为带 `--no-default-features --features standalone` 的构建。含旧文件操作的例子使用 `standalone-files`。现有生产信令命令、TLS、Relay 和 SQLite 路径沿用原设置。

## 6. 施工顺序和阶段验收

| 工单 | 修改与交付 | 完成条件 |
|---|---|---|
| P1 文件运行和策略收口 | 合并文件策略包，搬移文件运行和配置操作。先保持现有默认构建与公开行为。 | 既有文件续传、目录确认、任务恢复以及代理转发定向用例通过。生产依赖证明节点核心、运输入口、TCP 和无文件装配没有反向包依赖。 |
| P2 管理边界收口 | 移植宿主文件命令、配置校验、快照与旧协议兼容。核对 legacy 分支与信令调用方，完成独立构建。 | IPC 固定请求、响应与帧限制通过，重复请求和 revision 冲突语义保持；旧受支持 CLI 文件互操作及信令回环通过。用同一个数据目录验证关闭文件、裁剪、重新启用。 |
| P3 GPUI、Web、CLI 装配 | 页面提取到文件包，复用服务/转发页作为基础入口，完成标准和增强两个显式构建。 | 标准桌面和 Web 无文件节点、资源、动作及轮询。增强客户端连接标准 Agent 时，文件入口正确收起。两端具备能力时验证发文件、目录预览、接收、暂停续传。拖入路径到标准工作台不触发文件请求。 |
| P4 发行与切换 | 三平台脚本和 CI 产出两个候选，新增安装身份、升级前检查及回退。核对既有信令和专用产品构建。 | 同一提交的双候选可独立安装，错误混包在停旧进程前被拒绝；已有安装能保留身份、信任、规则和任务数据完成升降级。候选内容、依赖清单和哈希可核对。 |
| P5 切换默认与移除演练 | 将 Cargo、桌面打包入口、CI 默认候选和文档切到标准版，更新架构图的装配边界。 | 干净源码在 macOS、Windows、Linux 构建默认候选。在任务专用的隔离副本中，仅移除文件包目录、Cargo 注册、根装配挂载和文件专属测试/发布分支，再次构建并跑可信连接与 TCP 转发。归档涉及的触点。 |

每个工单分成可审核的小步提交，提交说明记录前置提交和实际检查。依赖树检查必须使用正式生产边，分别证明标准 `p2p-agent`、`p2p-node`、桌面和 Web 都无需 `veytrane-files`、旧 `veytrane-file-policy` 或文件专属存储；文件增强版继续分离 UI 与执行的编译依赖。`Cargo.lock` 可保留仓库中显式可选包的记录，验收根据解析后的生产依赖和产物。

最后在隔离配置完成两端标准版、标准版对增强版、两端增强版及已有版本升级四组测试。每组跑服务目录授权、真实 TCP 传输、并发、断线重连和撤销后监听/连接回收。增强版验证旧任务恢复及文件传输期间 TCP 并发。至少以实际候选覆盖三平台启动与回退。生产部署、线上信令切换及真实数据迁移分别确认后执行。

### P1 施工与验收（2026-11-21）

- `98fe06e` 迁入接收目录探针与错误分类，22 项配置测试通过。`d759050` 合并旧文件策略包，裁剪 Agent 保留旧文件配置并拒绝修改。独立裁剪构建和源码、生产依赖边界通过。裁剪 Agent 对完整 Agent 的可信 TCP、重连和文件能力拒绝实测通过。
- `cc1fc1f` 将任务库、接收目录和运行配置的启动移入文件包。`93e5ef2` 迁入文件命令配额与执行。`6b1d838` 迁入自动续传所需的方向授权，节点继续负责连接代际和任务所有权。三节点测试通过，覆盖文件、目录、四路转发和授权撤销；停用后恢复任务及配置 revision 事务通过。
- macOS arm64 的 `scripts/node-core-e2e.py` 验收 15 个场景，收集 21 条进程证据。矩阵启动时 `HEAD` 为 `93e5ef2`，工作树已包含随后提交为 `6b1d838` 的续传调整和既有未提交改动。证据保存在 `/private/tmp/veytrane-p1-matrix.TkUgkR/matrix.json`。当前桌面仍以轻量方式链接文件配置，界面裁剪归 P3。

### P2 施工与验收（2026-11-21）

- `7ef9487` 迁入管理任务分页和预览缓存。三节点、停用后恢复及架构检查均通过。
- `a524817` 将文件管理 DTO 归入文件包并保持 IPC v1 方法、字段和查询分类。基础 IPC 仅识别历史方法，文件增强版验证参数并报告 `invalid_request`。固定线协议、未知字段和重复字段拒绝、基础 CLI、桌面编译和三节点回归通过。
- `5b0fcb2` 将文件命令输入、任务动作和目录确认移入文件包。已有管理 IPC 处理认证、请求边界和 revision，节点 actor 仍独占队列及授权检查。两个构建的混合 TCP、文件能力拒绝、四路转发和授权撤销通过。真实裁剪及恢复测试确认同一数据目录中的文件任务保留。
- `ac92f7d` 将独立 CLI 的文件执行迁入文件包。`b9a22c3` 将独立 `standalone` 与 `standalone-files` 分开。基础组合的实际 TLS 信令、授权 TCP 回环和帮助检查通过，增强组合的 TLS 文件推送、IPv4/IPv6 文件收发、分片上限及 TCP 回环通过。两个组合保留相同的转发编号，基础解码拒绝文件清单。源码和生产依赖检查通过。

### P3 装配与本机验证（2026-11-21）

- `3c389e7` 将 Web 文件面板移入文件包。`bf99ba8` 和 `80eccc0` 提取共享 GPUI 控件和文件任务投影。`5000926` 将授权服务和转发操作提到首页。`9cf4ddc` 将 GPUI 文件工作台、设置、快捷键、拖放、任务窗口及管理 CLI 装入 `veytrane-files`。`ce3a8c2` 将新节点文件能力改为显式启用。`b5b6305` 用当前连接代际的协商结果限制增强桌面的发送入口。`c6ee03c` 收口文件任务 CLI 和 Web 权限列。
- 标准桌面、增强桌面、标准 Agent/CLI、增强 Agent/CLI 分别编译。生产依赖检查确认标准组合排除文件包，增强桌面排除文件安全存储和业务执行依赖。文件桌面 5 项、两套桌面各 3 项、两套 Web 各 26 项测试通过。标准 CLI 帮助不显示文件及任务命令，增强版保留 `tasks --offset`。历史 v1 请求仍可读取空任务页或收到能力错误。
- 9 项隔离 Agent 进程验收通过，覆盖混合版型、文件校验和目录确认、任务保留、TCP 并发、恢复与撤销。[进程日志](/private/tmp/veytrane-p3-agents.lmvVbT/agent-process-final.log)。
- 两种 Web 与两种 Agent 的四个组合通过实际回环 HTTP 和原生 IPC。标准页面没有文件面板资源，增强页面按 Agent 能力判断是否挂载，历史任务页仍有响应。[HTTP 证据](/private/tmp/veytrane-p3-entry.pdkpCo/web-ipc-proof.json)。
- Ubuntu 24.04 x86_64 使用原生 GPUI、隔离 X11 会话和模拟 Agent IPC 实际投递拖放。标准桌面连接文件增强 Agent、文件增强桌面连接标准 Agent 时均未请求 `tasks` 或 `send`。两端均为文件增强且对端能力已协商时，收到一次包含正确对端与文件路径的 `send`。文件执行由上述真实 Agent 进程验证。[拖放结果](../tmp/p4-linux-evidence.5K8JYj/xdnd.json)。
- 同一 Linux 增强候选中，点击任务行打开任务详情，收到新的目录预览时打开预览窗口。模拟 Agent 停用文件能力后，两个实际窗口关闭，任务轮询停止，转发工作台继续运行。[窗口结果](../tmp/p3-file-windows.xLzOPv/visual-results.json)、[任务详情截图](../tmp/p3-file-windows.xLzOPv/verified-task-details.png)、[目录预览截图](../tmp/p3-file-windows.xLzOPv/verified-directory-preview.png)。
- 使用 `c6ee03c` 的两个 macOS arm64 候选，在隔离 Chrome 中认证并读取四种 Web/Agent 组合的配置。四组均显示转发编辑器。标准 Web 无文件面板和三项操作的 DOM；增强 Web 仅在 Agent 启用文件能力时显示文件面板，点击刷新任务完成，清空该测试节点的管理 token 后面板隐藏。[Chrome 结果](../tmp/p3-web.hVtmyC/run.Mg3Ppr/results.json)。

### P4 双候选与安装切换（2026-11-21）

- `57f3d05`、`37da50b`、`e6676aa` 分别实现发行版型标识、双候选和只读升级预检。macOS arm64 的同 SHA 双候选从干净源码打包，校验架构、最低部署版本、三二进制元数据、实际依赖清单、原始源码材料、归档 SHA256 和 ad-hoc 签名。Windows、Linux 及 Intel Mac 配置了独立候选及 CI 校验流程。
- 使用 `18d114b` 的 SHA 目录旧安装与 `37da50b` 的两个原生候选，实测旧版 → 标准 → 增强 → 原 SHA 目录回退。混包和开启文件时切到标准版均在停止旧 Agent 前被拒绝。验证同一身份、信任、转发、配置与任务库保留。系统启动项由隔离测试适配，实际 Agent 进程通过真实 IPC 停启和检查所有权锁。[换版证据](/private/tmp/veytrane-native-p4.8v44vH/install-proof-hkpaqdkn/result.json)。
- 补测 macOS 实际 LaunchAgent 时，外置盘上的测试 plist 被 `launchctl bootstrap` 拒绝。从用户 LaunchAgents 目录加载同一临时标签后，向外置盘重定向日志的进程报告 `EX_CONFIG`，日志文件未生成。以 `/dev/null` 对照时 `launchd` 启动了隔离旧版进程，管理 IPC 未就绪，未进入换版。测试任务和临时注册文件均已清理，原 Agent 的 PID 和注册定义已保留。[启动诊断](../tmp/host.mac-runtime-ab4c/runner.log)、[对照结果](../tmp/host.mac-stream-ab4c/runner.log)。
- 用同一旧版二进制直接以 `--managed` 启动的对照，IPC 在 109 毫秒内就绪，并经 CLI 正常停止。将临时 LaunchAgent 的标准错误单独重定向到用户日志目录后，仍未生成 IPC socket，且标准错误为空。故障尚未定位到应用代码或具体系统限制，启动测试未进入换版。[直接启动证据](../tmp/p4-mac-ipc.HcO1Ad/results.json)、[LaunchAgent 结果](../tmp/host.mac-ipc-ab4c/runner.log)。
- 在临时 LaunchAgent 中加入执行探针后，确认任务可以访问外置盘上的测试配置和可执行文件。该任务调用旧版 `p2p-agent --build-info` 时超时，进程采样显示其 shell 等待子进程。同一已安装二进制用精简环境从终端执行 `--build-info` 成功。现象出现在 IPC 和节点配置加载之前，尚无证据指向具体系统限制。临时注册和日志已清理，原 Agent PID 未变。[探针日志](../tmp/host.mac-stack-ab4c/agent.stderr.log)、[采样](../tmp/host.mac-stack-ab4c/stack.txt)、[终端对照](../tmp/host.mac-stack-ab4c/direct-build-info.json)。
- 上一项验证的是文件路径和权限元数据。后续在独立 LaunchAgent 中直接读取同一外置盘上的合成文件内容，两次均得到 `Operation not permitted`，终端可读同一文件。该探针在启动 Agent 前退出，因此未采集到子进程堆栈，也不能据此确定此前超时的具体调用。已证实当前测试任务的外置盘文件读取受限。保持原权限，清理测试注册和日志，现有 Agent PID 与启动定义未变。[探针输出](../tmp/p4-child.xOZCRS/joint-job.log)、[对照结果](../tmp/p4-child.xOZCRS/results.json)。
- 将 `18d114b` 旧版和同 SHA 的 `ab4c34a` 标准版、文件增强版的 Agent 与 CLI 复制到系统盘隔离运行目录，原生 LaunchAgent 实际完成旧版 → 标准 → 增强 → 旧版。每次换版后通过 IPC 确认 TCP 能力、节点身份、revision 和配置字节。混包与文件仍启用时切到标准版均在停止进程前被拒绝。测试使用合成节点配置和独立标签，结束后卸载临时任务，现有 Agent 的 PID 与启动定义 SHA-256 一致。文件能力在测试中开启和关闭，未执行系统重新登录。源码、编译缓存、发行归档和验收记录保留在外置盘，此次复用已有候选。[换版结果](../tmp/p4-mac-system.6EQbja/verified-results.json)、[运行二进制 SHA-256](../tmp/p4-mac-system.6EQbja/verified-runtime-sha256.txt)。
- 16 项宿主测试及 9 项包验证测试通过。Linux 使用原生 systemd 用户临时单元实测标准 → 文件增强 → 标准，三个进程均经真实 IPC 就绪、退出并释放所有权，保留相同的身份、配置及 revision。两阶段比较了现有 Agent 服务的 PID、启动调用 ID 和定义路径。[systemd 换版结果](../tmp/p4-linux-evidence.5K8JYj/systemd-switch.json)。
- Linux 在隔离 HOME 与 XDG 配置中，使用 `node-host.py` 生成用户服务，再通过真实 `systemctl --user` 的客户端注册逻辑分别验证两版型的启用、`default.target.wants` 指向及禁用。标准 → 增强 → 标准均从各自注册后的 `ExecStart` 启动原生临时单元，并核对原有 Agent 的 PID、启动调用 ID 和定义路径。验证未执行物理重新登录，也未重载现有用户管理器。[注册与启动证据](../tmp/p4-login.Nz3ASU/results.json)。
- 从干净 `4f3be51` 源码在 macOS arm64 分别用 `--no-default-features` 构建 `live` 的 gateway/viewer、`sub2api-gateway` 和 `sub2api-client`，均生成原生可执行文件。客户端使用无私有配置的编译组合。三套构建均报告四条未使用的节点策略 API 告警；验证涵盖构建和目标架构。[Live](../tmp/p4-products.1GBs3O/live.log)、[Sub2API Gateway](../tmp/p4-products.1GBs3O/sub2api-gateway.log)、[Sub2API Client](../tmp/p4-products.1GBs3O/sub2api-client.log)。
- [首次原生 CI](https://github.com/gloryhui/Veytrane/actions/runs/44878747014) 在 `fdac2e6` 通过 Linux、Apple Silicon 和 Intel Mac。Windows 回归通过后，正式打包被干净工作树检查拦截。MSVC 探针默认把 `sdk_probe.obj` 写到仓库目录，`de9af87` 将它显式输出到探针临时目录。
- [修复后原生 CI](https://github.com/gloryhui/Veytrane/actions/runs/44880833831) 在 `de9af87d516efea8f0cf2dab0c235ea59815e1b2` 的 Windows x64、Intel Mac、Apple Silicon 和 Linux 全部通过。Windows 和 Intel Mac 分别使用正式宿主脚本构建两版型，归档校验通过。Linux 的标准版与增强版均通过严格 Clippy。
- `d639375` 更新 Windows [计划任务换版验收入口](../packaging/node/README.md#windows-双版型隔离验收)，允许同 SHA 的标准与增强候选往返切换，并核对任务指向、TCP 能力、身份与配置。18 项本机 Python 宿主测试通过。Windows Server 2022 CI 提供了可用于实测的双版候选，仍需具有已登录测试用户和 PowerShell 7 的隔离 Win10 22H2 或 Win11 x64 环境执行任务注册、切换和注销。

| `de9af87` 候选 | GitHub Actions 产物 ID | 归档 SHA-256 |
| --- | --- | --- |
| Windows x64 标准 | `14699490790` | `bfed8d06901ced7caf124451f3dcbd782bf4049d8adfa3dca9f60b41bf663170` |
| Windows x64 增强 | `14699490793` | `85314ff88adfc42026fee1e6626b98c3161cf027e46f9bdcce29dd94a1f8d7bf` |
| Intel Mac 标准 | `14699890428` | `abb166e59e6260c8b4d098767fe052c6abeb3545902f671c297b4bca3755e5e4` |
| Intel Mac 增强 | `14699661002` | `1781257b7e2331151fccc37f9cd8377c01a3049711bf92a1dc2e6aa495d0e3ec` |

四份归档已下载至外置盘，重新核对归档、内含三种可执行文件及依赖材料的 SHA-256。[本地归档与验证记录](../tmp/p4-ci-44880833831.hkoY9I/)。

### P5 默认切换与物理移除（2026-11-21）

- `3ae273f` 将默认构建切到 `agent-standard`，保留单独的 `standalone`、`standalone-files` 和专用产品 feature。`fc9e582` 更新[八主题架构图](layer-review.html)及可点击源码地图。
- 本机 `8093` 站点返回的架构图与仓库文件 SHA-256 一致。使用 agent-browser 驱动隔离的本机 Chrome 窗口，逐一验证八种主题的亮色、暗色，共 16 种状态，每种均包含全部 14 个节点。实际点击导出并重新打开三张 PNG，终端暗色与蓝图亮色为 2544×2374，等距暗色为 3812×2528，三张均完整显示图例、节点和边缘连线。[浏览器记录](/private/tmp/veytrane-diagram-qa.Ark9pn/browser-results.json)。
- 以 `c6ee03c` 的干净源码构建 macOS arm64 [标准候选](/private/tmp/veytrane-native-p4.8v44vH/verified-standard/candidate.json)与[文件增强候选](/private/tmp/veytrane-native-p4.8v44vH/verified-files/candidate.json)，均通过完整归档校验，第三方依赖数分别为 536 和 545。
- `a94b8a0` 调整公共工作台及诊断文案后，从干净 `ab4c34a` 源码使用正式 macOS arm64 脚本重新构建两个候选。标准与增强的三二进制版型、归档和 ad hoc 签名验证均通过，证据列出同一 SHA 的两个归档校验和。[标准候选](../tmp/p4-macos-final.3ZkW41/standard/candidate.json)、[增强候选](../tmp/p4-macos-final.3ZkW41/files/candidate.json)、[汇总](../tmp/p4-macos-final.3ZkW41/results.json)。
- 同一源码在 Ubuntu 24.04 x86_64 通过标准和增强两个 `-D warnings` Clippy 配置，打包脚本及归档验证均通过。Linux 标准版和增强版分别列出 591 和 601 个第三方包。标准版另通过真实回环导航 API、核心重建保留 HTTP，以及 X11 窗口、托盘和退出验收。[Linux 标准候选](../tmp/p4-linux-evidence.5K8JYj/standard-candidate.json)、[增强候选](../tmp/p4-linux-evidence.5K8JYj/files-candidate.json)、[导航](../tmp/p4-linux-evidence.5K8JYj/navigation-standard.json)与[桌面](../tmp/p4-linux-evidence.5K8JYj/background-standard.json)记录保存在外置盘工作区。
- 在从提交 `c6ee03c` 导出的隔离源码中，将 `crates/veytrane-files` 移到源码目录外。只改两份 manifest，去掉根 workspace 注册、增强版组合与依赖、文件专属测试注册，并去掉 Web 的 `files` feature 及依赖。Cargo 重新解析锁文件，生产依赖树与锁文件均无该包。同一份标准 Rust 源码构建出 Agent、CLI 和 GPUI。保留的可选装配 `cfg` 名称在剪掉注册的临时源码中触发检查提示。
- `0195687` 的真实进程用例使用隔离复制品构建的两个 Agent，覆盖可信授权目录、四路实际 TCP、测速时并发 TCP、重连、撤销信任后旧流和授权失效、停止转发释放监听端口，并验证两端未创建文件任务库。[移除演练日志](/private/tmp/veytrane-p5-verified.ehCYB7/tcp-proof.log)。

## 给施工模型的起步指令

> 以本文件和当前工作树为准，先执行 P1 的源码清册与依赖核对。文件能力从既有 `veytrane-files` 收口，复用节点会话和授权机制。保留用户工作树中的其他改动，按 P1 到 P5 分步提交，直到双候选和移除演练都有实际证据。每次对外契约变化都写明调用方，记录有效的升级与回退路径。
