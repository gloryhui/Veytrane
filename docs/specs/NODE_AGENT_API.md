# T22：本地 Agent API v1

状态：本轮实现规格；静态审核不替代进程及跨平台验收。用户已授权继续代码实施，测试后置。

## 所有者与路径

`p2p-agent --data-dir DIR` 前台运行，拥有一个身份、一套 NodeControl/TransferService、配置仓库和管理端点。CLI、Web、桌面仅持客户端。路径统一由 `node/agent_paths.rs` 定义：`node.json`、`identity.key`、`tasks.json`、`agent.lock`、Unix `agent.sock`。Windows pipe 名由规范化数据路径与当前用户 SID 摘要确定，禁止远程客户端，DACL 仅当前用户与 LocalSystem。Unix 目录 0700、socket 0600，并校验所有者；不得把公共临时目录当端点根。

数据目录锁在读写身份前取得。所有身份登记入口必须取得同一按 NodeId 索引的用户级身份锁，防复制密钥到另一数据目录同时登记。不同 OS 用户的隔离边界由 OS 账户权限决定；本机锁不能协调其他机器的私钥副本，复制私钥跨机器运行不受支持。遗留入口未接入身份锁前，不宣称具备完整互锁。

## 帧与响应

模块：`node/ipc.rs`（类型与 codec）、`node/ipc_transport.rs`（OS 端点）、`node/agent.rs`（宿主/路由）、`node/agent_client.rs`（共享客户端），新增二进制 `src/bin/p2p-agent.rs`。客户端不能得到 QUIC、私钥、验证器或业务运行时句柄。

本地可靠字节流，4 字节大端长度 + UTF-8 JSON，上限 1 MiB。请求包含 `api_version=1`、非空 ASCII `request_id`（最多64字符）、`timeout_ms`（1..30000）、类型化 command。响应回显版本/ID，包含结果或稳定错误 code/message，不返回凭据。未知字段、未知命令、API 版本不符明确拒绝，认证不降级。握手/首帧5秒，命令最多30秒，连接最多32，每连接串行，不允许无限任务排队。

查询：status、config（敏感字段只返回是否存在）、tasks、services(peer)、events(after)。变更：connect、disconnect、send/pause/resume/cancel、转发、资源授权、密码、配置应用及stop。配置应用必须带 expected_revision。IPC 管理权来自本地 OS 身份，peer 授权仍独立校验，不因管理权产生远程密码证明。

任务查询每页128，offset默认0，返回total/next_offset；同时返回有界目录预览和测速快照。PreviewDirectory(peer,path)只接收一个绝对本机目录路径，返回accepted/request，异步产生预览；ConfirmDirectory前不创建发送任务。Send保留直接发送文件/目录的明确语义，原生“选择文件夹并预览”、Web“预览目录”及CLI preview-directory必须调用PreviewDirectory。目录预览ID/peer由核心生成，peer投影为规范文本，源只投影128字符名称；必须显式ConfirmDirectory或CancelDirectory，不能由管理端篡改比较结果/路径后确认。预览缓存32条/300秒，失效仍由核心验证。任务名称展示限256字符并标name_truncated，完整绑定保留在核心。StartSpeed/StopSpeed复用当前会话与能力协商，管理确认不等于测速完成。status附带最多32条、每条512字符诊断，规则错误256字符，管理错误4096字符；不包含管理凭据。在途管理变更另限32，已超时未完成任务仍占名额，不能依靠不断重建连接突破预算。

变更请求的 ID 在单次进程内最多缓存1024条/5分钟，相同 ID+相同规范命令返回相同响应，不重复执行；同ID不同命令拒绝；执行中的请求也必须保留去重记录。到期/重启的重试只能通过 revision、任务 ID 和状态确认，不承诺跨重启的任意命令 exactly-once。配置持久提交额外保存最后一次请求 ID/摘要，以识别同revision重试。超时区分未入队与结果未知，不能因为客户端断开回滚已提交结果。

快照包含配置 revision 与事件 sequence，二者含义不同。事件环有界256条；游标过旧/进程 epoch 不同返回 resync_required 和新快照。文件进度通过任务快照查询；网络事件序号不能充当文件持久 checkpoint。IPC事件响应单批最多128条，不持无界推送队列。

## 退出与离线

启动不要求 peer 在线。IPC就绪不等于信令已连接。SIGINT/SIGTERM/stop：停止接纳管理命令，停止新业务，通知核心退出，等待已有核心清理（上限5秒，超时报告），删除仅自己创建的 Unix socket，释放身份锁，最后释放目录锁。异常退出允许下次持锁者清理残留 socket，不抢占活进程。不能从窗口 Drop 顺带停止 Agent。升级必须等待旧进程释放两把锁，不能在超时后强行启动新所有者。

## 后置验收

两管理入口并发、超大/截断帧、重复ID、超时后查询、跨用户IPC、remote pipe、目录/身份双启动、复制密钥、事件断档、离线管理与干净退出。Windows/Unix 均需真实进程实测；本轮仅编译/static 检查。
