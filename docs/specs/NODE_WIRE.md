# 统一节点协议 v2

2026-10-21：当前 Agent、桌面附着入口和 Sub2API 共用本协议。旧桌面协议适配已删除；测试也使用同一协商与分发器。

## 连接安全和版本空间

信令 v2/v3、Relay v1、已有 Ed25519/TLS exporter 身份握手保持各自版本。统一节点连接在身份握手后打开唯一协商双向流，双端使用 NodeId 固定 QUIC 发起角色并沿用已验证的身份绑定。随后新基础协商、远程认证、授权完成，才发布业务可用状态。

新协商 magic 为 `P2PN\x01`（十六进制 `50 32 50 4e 01`），包体先 4 字节小端长度，再 magic + postcard 的 `NodeFrame { version: u16, request_id: u64, message: NodeMessage }`。version 固定 2，完整帧上限 64 KiB。解码使用 take_from_bytes 并拒绝多余字节，先限长再分配。协商超时 5 秒。Hello/Ready 的 request_id 为 0；目录/TCP 请求与响应使用非零请求 ID。错误无秘密或本地路径。

NodeMessage postcard 变体顺序固定：0 Hello{capabilities:u64}；1 Ready；2 Services；3 ServiceList{services:Vec<ServiceDescriptor>}；4 OpenTcp{service_id:String}；5 TcpReady；6 Error{code:NodeErrorCode}。仅允许尾部追加，不能按 Rust repr 假设 postcard 编号。ServiceDescriptor 为 id/name 字符串，不发送目标 IP、未授权条目或设备信任表。ID 按既有规则 ASCII 1..64，名称 1..128 字符，不含控制字符；目录最多 128 项。

NodeErrorCode 顺序固定：0 UnsupportedVersion；1 UnsupportedCapability；2 Unauthorized；3 UnknownService；4 Busy；5 InvalidRequest；6 TargetUnavailable；7 Internal。业务拒绝用稳定错误码，不返回任意 OS 错误文本。未知枚举、版本、超长、非法 ID/请求号立即拒绝。

两边先各发 Hello，再读取对方 Hello，校验安全必需能力，分别发 Ready 并校验对方 Ready。任意失败关闭候选连接；不尝试 P2PD/P2PA/旧 CLI 作为回退。Hello 在身份绑定的 TLS 连接中交换；不提供逐 peer 协议模式。

## 能力

保留现有任务子协议位 0..12 的编码，供复用已验证状态机：TASK_PROTOCOL、RELATIVE_ENTRIES、PAUSE_RESUME、SPEED_OWNERSHIP、FILE_TRANSFER、DIRECTORY_TRANSFER、SPEED_EXECUTION、TCP_TUNNEL、REMOTE_AUTH、TRUSTED_DEVICE_AUTH、FILE_FLOW、AUTO_RESUME、INCREMENTAL_DIRECTORY。

新增基础位 16 NODE_BASE、17 SERVICE_DIRECTORY、18 RESOURCE_AUTHORIZATION。必需 NODE_BASE | REMOTE_AUTH | RESOURCE_AUTHORIZATION | TRUSTED_DEVICE_AUTH；文件、目录、测速、TCP 均可选。未知可选位忽略，返回双方交集。文件服务未装配时不公布文件/目录/恢复能力；测速未装配时不公布测速。TCP-only 节点可协商。服务目录能力不代表访问权限。

## 业务流

每条新双向业务流先写一个字节的业务类型：1 文件任务；2 服务目录/TCP；3 测速控制。文件/测速载荷使用 `P2PF\x01` 帧；只包含文件、目录和测速消息，已删除 Hello/Ready/TunnelOpen/TunnelReady/TunnelError。相对目录、任务绑定及持久收据语义保持。测速控制复用现有有界控制帧与单一 UNI 数据所有者。类型 2 使用本规范 NodeFrame，成功 TcpReady 后直接搬运字节。文件流禁止夹带旧 TunnelOpen，测速流禁止当文件控制流使用。

单一 dispatch 所有者读取连接的 accept_bi，按业务类型投递到有界队列；文件/TCP/测速处理器不得并发抢读同一连接的入站队列。文件原有首帧 5 秒、全局帧预算 64 MiB、每任务 8 MiB、分片 1 MiB 和原并发上限保持。控制队列/公平预算在 T23 冻结；本阶段有界队列容量 32，超过明确 Busy，不能无限堆积。未知流类型拒绝。UNI 数据由测速唯一所有者管理，其他业务不开放 UNI 类型。

入站首字节最多同时等待 64 条，各 5 秒；首部读取并发进行，单条慢流不阻塞其他类型。尚无法读取请求号时用固定 QUIC reset/stop 码拒绝：0x101 Busy、0x102 UnsupportedCapability、0x103 InvalidRequest、0x105 Unauthorized。已取得控制请求号后使用 NodeFrame Error 响应。Dispatcher 随连接业务所有者析构取消首部任务和队列。

服务查询必须已授权入站业务，并只返回当前 peer 被授权的 enabled 条目。OpenTcp 使用稳定 service_id，开流时在同一策略快照中重新检查方向、资源、启用状态与当前目标，再进行受控连接。缓存目录不能成为授权。客户端必须提供 service_id，目标 SocketAddr 仅由服务端配置解析；没有裸地址开流接口。

## 版本与执行边界

| 组合 | 当前行为 |
| --- | --- |
| 当前 Agent / 桌面附着入口 / Sub2API | 唯一 P2PN v2 协商，File / Service / Speed 分发 |
| P2PN v1 或旧 P2PD 桌面端 | 握手明确拒绝，不探测、回退或接收旧消息 |
| 独立 CLI | 仍使用其独立线格式；没有 Agent 协议适配 |

P2PN 的 magic 尾字节 01 是帧封装标识，NodeFrame.version=2 才是此次业务契约版本。删除文件消息变体改变了 postcard 编号，因此必须先在节点握手拒绝 v1，不能让旧端进入业务阶段误解码。P2PF 载荷固定字节测试约束当前枚举编号；后续变更需重新审查协议版本。

`NodeSessionConfig::new` 直接创建当前协议会话，不存在 wire_mode、peer_modes 或兼容构造器。`NodeSettings` 删除 peer_modes，未知字段直接拒绝；每条转发必须有 service_ids 映射。部署新二进制前，两端需同步升级并移除旧配置中的 peer_modes 字段，保留身份、密码验证器、资源授权及任务库；本次源码变更没有修改实际运行的节点配置。

## 授权和任务兼容约束

远程密码 Argon2id、TLS 会话绑定及可信设备控制仍复用经过验证的版本；不得改已有安全域常量来假装协议兼容。支持能力/启用能力/授权来源分开。管理员预置信任只能记为 administrator，不写 password=true。普通服务撤销只影响提交后新流；设备/凭据强撤销失效当前权限、关闭相关传输尝试。文件显式暂停、取消、断开不触发自动恢复；当前任务持久格式与恢复状态机保持。

固定合成黄金向量位于 `src/node/core/wire.rs` 和 `protocol.rs` 的测试。授权、TCP 开流、双向文件强撤销、进程终止恢复与能力缺失回归统一执行当前协议，不保留旧消息实现来满足测试。

授权控制流为必需能力，始终交换并持续更新可信设备授权；删除缺少该能力时的一次性密码认证分支。
