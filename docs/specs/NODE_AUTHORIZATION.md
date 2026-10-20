# T18：节点授权、来源与强撤销实施细则

- 状态：运行时契约冻结，持久化格式由 T23 冻结后接入；测试后置。
- 路径：`node/authorization.rs`、`node/session.rs`、`node/control.rs`、T19 dispatcher 和 T20 services 的接纳检查。

1. NodeSessionConfig 固定默认 wire_mode、按 NodeId 覆盖，以及业务启用意图。兼容构造器 new 保留 LegacyDesktopV1；unified 构造器显式 UnifiedV1。每次尝试开始冻结模式及实际装配的能力，TLS/密码失败不换模式。LegacyCliV3 未实现时在发布业务前明确拒绝，不借用密码 grant。
2. 运行时 ResourcePolicy 以规范 NodeId 为键，记录入站文件和测速的允许/禁用。Unified 默认未列出即拒绝；TCP 始终逐 service_id 按实际发布服务 ACL 判断，文件/测速允许不授予 TCP 或管理权限。旧桌面模式保留原能力边界。每次新流、排队后开始处理与服务开流检查当前策略；正在传输的资源普通停用自然结束，设备禁用强撤销。
3. TrustSource 仅为本机授权记录来源：LegacyImport（原记录来源无法逐条证明）、PasswordVerified（当前会话已实际证明密码才可写）、Administrator（本地显式预置）。不得因为本地预置而将 AuthorizationGrant.password 设为 true。运行时来源与凭据算法分开；旧 DesktopConfig schema 11 不追加新字段，新 Node 配置承载来源由 T23 定义。
4. 用户显式断开：加入当前宿主的阻止重连集合，清理该 peer 缓存凭据/候选/恢复意图，失效授权并关闭相关 QUIC/监听；不影响其他 peer。只有新的显式连接动作才清除阻止标记，旧代回调或内部自动重连不能解除。断开不撤销磁盘信任，下一次显式连接仍重新认证。
5. Unified 设备信任撤销/安全禁用为强撤销，即使会话同时有 password grant 也失效该 peer 全部当前权限并关闭连接，且不会通过重连复用缓存密码。旧桌面兼容路径保留原协议行为；新 GUI 附着统一 Agent 后使用新策略。密码轮换沿用原全连接失效流程；正常信令重连不强撤销业务。
6. 权限更新、接纳和 revoke 由同一 session actor 确定先后。读取共享资源策略时用单一最新快照，禁止跨两个旧/新对象拼凑授权。提交后排队尚未接纳的流必须重新检查；已接纳流由 LiveAuthorization guard 处理强撤销。

待测试：TLS 身份错配、密码错误、重放、方向错误、管理员预置真实来源、普通/强撤销区别、开流竞争、旧代回调、排队后撤销和其他 peer 隔离。不得以编译或设计覆盖声称这些已通过。
