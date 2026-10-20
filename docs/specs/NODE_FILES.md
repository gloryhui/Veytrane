# T19：统一节点文件流与取消语义

- 状态：本轮实施契约冻结，测试后置。
- 路径：node/transfer、frame_budget、speed、session、dispatch、task_model、task_store、control；保留原 CLI 与旧桌面编码/发布策略。

Unified 在授权后启动唯一 Dispatcher。文件与测速入站流分别投递有界队列，交给原 TransferService/SpeedPeer 的流处理；旧模式继续原 accept_bi 路径，两者不能同时拥有该队列。所有生产 open_bi 在连接的显式模式下写 File 或 Speed 类型前缀；连接标识使用 Quinn stable_id，旧代退出只移除自身绑定，不覆盖新代。协商和远程认证流不带业务前缀。UNI 测速数据保留原专属所有者，不作为文件流。

资源策略按连接 peer 在出队接纳时复查：未授权方向、设备禁用、文件或测速未授权分别拒绝；已接纳的普通资源权限变更自然结束，强撤销通过现有 LiveAuthorization guard 关闭。文件流不得路由到旧裸地址 TunnelOpen，Unified 的 TCP 只能经 service_id；Speed 类型仅接收 Speed 控制。文件与测速共享已有状态机和字节/空间预算；协议协商的能力交集控制当前连接可执行业务。

取消使用原有 schema 已存在的 `TaskState::Failed` + `TaskErrorCode::UserCancelled` + retryable=false，不新增状态枚举/格式。显式取消在任务库写锁中落盘这一终态，然后撤销排队 nonce、自动恢复意图并通知当前尝试停止；旧 worker 不得覆盖终态，重启/重连/旧 TaskId Offer 不能复活。已进入 publish_prepared 发布提交边界、已完成或收据已提交的任务拒绝取消，不删除已接收文件；新选取可创建新 TaskId。取消与发布共用任务库存储串行点，先取消则后续发布必须失败；先持久完成则取消拒绝。未收到远端确认不假称远端任务状态已同步，旧端看到流中断后若重试同一 TaskId 会被原非重试失败状态拒绝。

同一节点的 GUI/CLI 最终均经 NodeControl 调用；正式 Agent 管理入口在 P4/P5 接入。目录、增量、完成回执、暂停/恢复及原接收冲突策略保持，不以新 magic 重写文件结果。Golden、hash、进程中断、丢回执、取消/发布竞争及磁盘失败全部待后续测试。
