# T16：统一节点协议与显式兼容路由

- 状态：实施编码契约冻结；固定字节与实际旧二进制测试按用户要求后置，兼容尚未验收。
- 日期：2026-10-20；实现起点 `d5c5926`。
- 路径：T17 `node/wire.rs`；T18 `node/session.rs`、`node/remote_auth.rs`、`node/control.rs`；T19 `node/dispatch.rs`、`node/transfer.rs`、`node/frame_budget.rs`、`node/speed.rs`；T20 `node/services.rs`、`node/session.rs`、`node/tunnel.rs`；T21 兼容适配、旧命令及 fixtures。

## 连接安全和版本空间

信令 v2/v3、Relay v1、已有 Ed25519/TLS exporter 身份握手保持各自版本。统一节点连接在身份握手后打开唯一协商双向流，双端使用 NodeId 固定 QUIC 发起角色并沿用已验证的身份绑定。随后新基础协商、远程认证、授权完成，才发布业务可用状态。

新协商 magic 为 `P2PN\x01`（十六进制 `50 32 50 4e 01`），包体先 4 字节小端长度，再 magic + postcard 的 `NodeFrame { version: u16, request_id: u64, message: NodeMessage }`。version 固定 1，完整帧上限 64 KiB。解码使用 take_from_bytes 并拒绝多余字节，先限长再分配。协商超时 5 秒。Hello/Ready 的 request_id 为 0；目录/TCP 请求与响应使用非零请求 ID。错误无秘密或本地路径。

NodeMessage postcard 变体顺序固定：0 Hello{capabilities:u64}；1 Ready；2 Services；3 ServiceList{services:Vec<ServiceDescriptor>}；4 OpenTcp{service_id:String}；5 TcpReady；6 Error{code:NodeErrorCode}。仅允许尾部追加，不能按 Rust repr 假设 postcard 编号。ServiceDescriptor 为 id/name 字符串，不发送目标 IP、未授权条目或设备信任表。ID 按既有规则 ASCII 1..64，名称 1..128 字符，不含控制字符；目录最多 128 项。

NodeErrorCode 顺序固定：0 UnsupportedVersion；1 UnsupportedCapability；2 Unauthorized；3 UnknownService；4 Busy；5 InvalidRequest；6 TargetUnavailable；7 Internal。业务拒绝用稳定错误码，不返回任意 OS 错误文本。未知枚举、版本、超长、非法 ID/请求号立即拒绝。

两边先各发 Hello，再读取对方 Hello，校验安全必需能力，分别发 Ready 并校验对方 Ready。任意失败关闭候选连接；不尝试 P2PD/P2PA/旧 CLI 作为回退。Hello 在身份绑定的 TLS 连接中交换，改变模式必须是本机显式配置操作。

## 能力

保留现有任务子协议位 0..12 的编码，供复用已验证状态机：TASK_PROTOCOL、RELATIVE_ENTRIES、PAUSE_RESUME、SPEED_OWNERSHIP、FILE_TRANSFER、DIRECTORY_TRANSFER、SPEED_EXECUTION、TCP_TUNNEL、REMOTE_AUTH、TRUSTED_DEVICE_AUTH、FILE_FLOW、AUTO_RESUME、INCREMENTAL_DIRECTORY。

新增基础位 16 NODE_BASE、17 SERVICE_DIRECTORY、18 RESOURCE_AUTHORIZATION。必需 NODE_BASE | REMOTE_AUTH | RESOURCE_AUTHORIZATION；文件、目录、测速、TCP 均可选。未知可选位忽略，返回双方交集。文件服务未装配时不公布文件/目录/恢复能力；测速未装配时不公布测速。TCP-only 节点可协商，旧桌面 REQUIRED_CAPABILITIES 仍保持旧规则。服务目录能力不代表访问权限。

## 业务流

统一模式每条新双向业务流先写一个字节的业务类型：1 文件任务；2 服务目录/TCP；3 测速控制。文件任务复用现有 task-flow-v1 的 P2PD 帧编码、相对目录和持久收据语义，该 magic 是已有子协议版本标识，不再代表 GUI 角色。测速控制复用现有有界控制帧与单一 UNI 数据所有者。类型 2 使用本规范 NodeFrame，成功 TcpReady 后直接搬运字节。文件流禁止夹带旧 TunnelOpen，测速流禁止当文件控制流使用。

单一 dispatch 所有者读取连接的 accept_bi，按业务类型投递到有界队列；文件/TCP/测速处理器不得并发抢读同一连接的入站队列。文件原有首帧 5 秒、全局帧预算 64 MiB、每任务 8 MiB、分片 1 MiB 和原并发上限保持。控制队列/公平预算在 T23 冻结；本阶段有界队列容量 32，超过明确 Busy，不能无限堆积。未知流类型拒绝。UNI 数据由测速唯一所有者管理，其他业务不开放 UNI 类型。

入站首字节最多同时等待 64 条，各 5 秒；首部读取并发进行，单条慢流不阻塞其他类型。尚无法读取请求号时用固定 QUIC reset/stop 码拒绝：0x101 Busy、0x102 UnsupportedCapability、0x103 InvalidRequest、0x105 Unauthorized。已取得控制请求号后使用 NodeFrame Error 响应。Dispatcher 随连接业务所有者析构取消首部任务和队列。

服务查询必须已授权入站业务，并只返回当前 peer 被授权的 enabled 条目。OpenTcp 使用稳定 service_id，开流时在同一策略快照中重新检查方向、资源、启用状态与当前目标，再进行受控连接。缓存目录不能成为授权。旧 SocketAddr 业务只在显式旧模式按精确白名单处理。

## 旧端矩阵及路由

| 组合 | 明确模式与范围 |
| --- | --- |
| 新节点 ↔ 新节点 | UnifiedV1，基础协商及独立业务流，任意 headless/GUI/CLI 入口共享 |
| 新节点 ↔ 旧桌面 | 按 peer 显式 LegacyDesktopV1；原 P2PD/P2PA、原密码/可信设备授权和原能力检查，不写新前缀 |
| 新节点 ↔ 旧 CLI | LegacyCliV3 仅在明确的节点/资源 allowlist 适配被实现及验证后开放；方向、身份握手角色和首流未覆盖的组合拒绝并要求升级，不能伪造密码 grant |
| 旧 CLI ↔ 旧 CLI | 保留原命令与线格式路径；同身份 Agent 已运行时禁止第二次登记 |
| 旧 CLI ↔ 旧桌面 | 不支持；不因均使用 QUIC 宣称兼容 |

NodeSessionConfig 记录固定模式和每 peer 覆盖。过渡 GUI 与原单元测试构造器保留 LegacyDesktopV1，新的 Agent 配置显式选 UnifiedV1；P5 GUI 改为附着 Agent 时不再自行选择网络模式。模式在连接尝试前确定，认证失败不改变。LegacyCliV3 尚无实现时返回 UnsupportedCapability，不临时启动另一份同身份 CLI 进程。

## 授权和任务兼容约束

远程密码 Argon2id、TLS 会话绑定及可信设备控制仍复用经过验证的版本；不得改已有安全域常量来假装协议兼容。支持能力/启用能力/授权来源分开。管理员预置信任只能记为 administrator，不写 password=true。普通服务撤销只影响提交后新流；设备/凭据强撤销失效当前权限、关闭相关传输尝试。文件显式暂停、取消、断开不触发自动恢复；取消的持久语义及旧任务迁移在 T18/T19 补充具体类型后实现。

旧 Frame 编码器、枚举与配置 schema 不在本协议卡中改写。黄金向量必须来自固定合成样本，待测试阶段补入 `tests/fixtures/node-v1/`，并核对新/旧版本、截断、超长、未知枚举、错误凭据、TCP-only、禁用文件、错方向、重放、资源撤销与队列超限。本卡不自行宣布上述验收通过。
