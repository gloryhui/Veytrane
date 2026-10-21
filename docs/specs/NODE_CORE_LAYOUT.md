# T11：无 GUI 核心物理边界与控制契约

> 以下保留迁移时的路径清单；2026-10-21 的实际物理分层见 [仓库分层说明](../REPOSITORY_LAYOUT.md)。

- 状态：本轮实施规格冻结；阶段测试验收后置。
- 输入代码：`5992c2d`；物理形式：同一 crate 下 `src/node/`，暂不新增 crate。

## 特性和依赖

新增 `node` 特性（默认启用），包含 Argon2/HMAC/SHA2/getrandom、dirs、cap-std/cap-fs-ext、unicode-normalization 、Unix rustix 与 Windows 文件系统接口 windows-sys；`gui` 依赖 `node`，仅追加 GPUI、托盘、通知、原生窗口和界面字符串依赖。无 GUI 编译命令 `cargo check --locked --no-default-features --features node --all-targets`。既有纯 CLI 可通过 `--no-default-features` 构建。不得给 node 添加桌面反向依赖。

## 分批精确清单

| 任务 | 移动 desktop → node 的模块 | 必要适配 |
| --- | --- | --- |
| T12 | config、remote_auth、trusted_devices、protocol、task_model、task_store、task_recovery、task_events、bandwidth、instance_lock（任务库存储锁的直接依赖） | 新建 preferences 保存 BackgroundOptions；新建 receive_policy 保存安全余量默认及校验，旧 background/space_budget 重导出。protocol 与 task_model 有实际相互类型依赖，先一并迁移，保留线格式。Cargo、lib.rs、desktop/mod.rs 特性与重导出 |
| T13 | activity、auto_resume、drop_send、files、frame_budget、incremental、publish、queue、secure_fs、space_budget、speed、transfer_files、transfer、task_details、ui_model | ui_model 是无图形依赖的领域投影，改为 node/snapshot.rs 并以 ui_model 别名过渡；network_state 纯模型一并迁移以满足投影依赖，tunnel 线格式适配器因文件分发直接调用也一并提前迁移。将 storage 中目录 capability 的 feature 门槛及引用由 gui 改为 node；windows-sys 文件系统依赖移入 node；修正测试 worker 字符串路径，不重新生成夹具 |
| T14 | session、diagnostics（tunnel 已在 T13 随分发器迁移） | 全部使用 node 内领域类型；instance_lock 已在 T12 随任务库迁移；共享 relay/tests.rs 的信令 fixture 门槛由 gui 改为 node；原 DesktopSessionConfig/Handle 名作为兼容别名，新增 NodeSessionConfig/Handle 作为正式名 |
| T15 | 新建 node/control.rs；修改 desktop/mod.rs 的命令及事件入口 | 进程内控制宿主复用同一 session，不另启网络；不返回 Quinn/GPUI 对象 |

旧 desktop 文件以 module 重导出适配，不能复制业务实现。UI、主题、布局、托盘、通知、autostart、background 窗口行为留在 desktop。允许模型的 crate 内可见性扩展供两边调用；不以抽取改变协议 magic、枚举序号、auth context、密码算法、schema、发布顺序、默认冲突策略。

## T15 控制接口（进程内）

`NodeControl` 独占 NodeSessionHandle 及 TransferService。界面只取得其 clone；发送命令由控制接口委托当前状态机处理，禁止解析 CLI 文本。操作保留身份、会话代际、任务绑定及显式暂停意图。

当前进程内实现以 NodeSessionHandle 的公开控制别名 NodeControl 保持既有命令适配，增加 ControlState、NodeSnapshot、SequencedEvent 和 NodeCommand；state 内保存同一 TransferService clone，不创建第二个任务库或网络。SessionEvents 在原有界事件队列接纳时同步更新权威网络快照并发布容量 256 的广播；订阅者通过同一快照锁取得订阅点及快照，Lagged 必须重读。原单消费者 ControlEvents 保留给内嵌桌面，宿主负责持续读取或 drain；管理订阅者使用广播。文件任务通过同一 TransferService 的 task_snapshot 查询，网络序号不伪装成文件 checkpoint 序号。纯领域投影和诊断类型可公开，无 GPUI/Quinn 对象；原有细节事件也可供进程内入口消费，IPC 序列化使用 T22 独立白名单 DTO。

- 请求：连接/断开 peer（密码只存在 zeroizing 值）、更新转发、选择文件、预览/确认目录、暂停/继续/取消任务、测速/停止、查询快照、停止宿主。
- 结果：有类型的领域结果或统一错误；提交成功仅代表已接纳，不代表任务完成。文件完成仍由持久收据决定。
- 快照：网络生命周期、peer 代际/授权、规则状态、文件/目录任务、队列、测速及诊断；只包含有界投影，不含文件清单、大块数据、凭据或 Quinn 对象。
- 事件：单调序号及领域事件；保留权威快照，订阅通道容量有界。落后/断档返回需要重读快照的显式信号，不伪造完成。
- 取消：管理请求超时不自动撤销已经提交的配置或已接纳文件任务；显式取消任务经现有 TaskId 状态机。停止服务是独立动作，与 GUI 关窗分开。

P2 保持内嵌 GUI 过渡方式；P4/P5 改为 IPC 附着时控制 API 的公开序列化契约由 T22 冻结。任何需要新权限或业务协议的行为留给 T16/T18，不在目录迁移中夹带。

## 待验

旧协议固定字节、任务/身份/信任数据、CLI/GUI 原有功能、Linux 无图形开发包运行及所有相关原测试均待后续验证。当前仅编译/特性依赖和静态检查。P1/P2 不标记已验收。
