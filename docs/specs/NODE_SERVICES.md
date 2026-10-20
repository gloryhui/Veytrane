# T20：服务目录、TCP 及测速接入

- 状态：实施契约冻结，测试后置；服务 RPC 组件作为 T20a 先供 T19 分发接入使用。
- 路径：`node/services.rs`、`node/session.rs`、`node/tunnel.rs`、`node/control.rs`、`node/speed.rs`、`node/transfer.rs`。

服务 ID 复用发布规则稳定 id，名称只供展示，目录不含目标 IP。只列当前 peer 在 allowed_peers 中的 enabled 服务，最多 128 项，超限 Busy。每条 Service 流仅处理一项 Services/OpenTcp；请求号非零、响应号匹配。RPC 完整操作上限 10 秒；目标连接沿用已有受控连接超时，成功 TcpReady 后才进行原 TCP 字节搬运。

入站 Service 接纳：先检查当前 LiveAuthorization 的 inbound、资源禁用状态；读取有效规则快照按 service_id 找到当前目标及 NodeId ACL；目标连接后再次读取规则确认 id/目标/授权未改变，再发送 TcpReady。缓存目录只用于选择，不能跳过检查。服务规则普通撤销仅阻止提交后尚未接纳的流；强撤销由 LiveAuthorization 关闭已经运行的流。

目录查询/开流独立于文件服务装配，TCP-only 节点也运行 Dispatcher 和服务 RPC，未装配的文件/测速队列明确拒绝。每 peer 最多 8 条尚未完成的服务控制 RPC；进入 TcpReady 后立即释放控制槽，TCP 字节搬运持有共享节点 TCP 预算的许可直到流结束。预算耗尽时在 TcpReady 前返回 Busy，服务目录查询仍可用。文件和测速各自有界，不让文件流耗尽 TCP 接纳槽；HTTP keep-alive、WebSocket 和 SMB 长连接也不能占满服务控制槽。

本机规则的 service_id 在新配置中显式保存，T23 定义磁盘 schema；P3 暂用 NodeSessionConfig 的规则 ID→service ID 运行时映射，旧桌面/CLI 规则仍保存裸目标并走显式旧模式。Unified 缺少 service_id 拒绝启动，不把 target 反推成授权。NodeControl 提供已认证对端的目录查询；不暴露远程配置写接口。

规则停用/删除只取消监听者，新接入立即停止，已接纳流交给同一节点级 ForwardStreamOwner（默认总量 256，单规则 32）自然结束；节点退出、强撤销或真实传输失败仍收尾。监听任务退出确认与已接纳业务流的最终结束分开。产品错误反馈队列容量 32、非阻塞发送，不能按连接错误无限增长。实施路径补充 `src/tunnel/mod.rs` 的共享监听辅助函数；旧公共辅助函数保留原清理语义以兼容已有调用，新节点使用明确的 owner 形态。

测速控制经 Speed 类型流，UNI 数据仍由唯一 SpeedPeer 消费。未启用测速、未授予测速资源或未获当前方向权限时拒绝。公平和资源阈值最终由 T23/T26 配置；本轮不得把未验证的吞吐和无卡顿当成结论。

待验：未授权目录隔离、过期缓存、改目标/撤销竞争、TCP-only、连续 TCP、文件与测速并发、限额、目标离线可停止、不同 peer 隔离。均不以编译代替。
