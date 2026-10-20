# T27：宿主、管理入口与升级

本轮编码规格；发布仍受T34实测门槛约束。

## 目录与进程

二进制：`p2p-agent`持有业务，`p2p-node`提供结构化CLI管理/初始化/离线配置，`p2p-desktop`默认原生Agent管理窗口，`p2p-node web`是可选loopback HTTP→IPC网关。四者通过`--data-dir`选同一AgentPaths；默认用户模式为本机用户数据目录`p2p-file/node`。GUI不得读取私钥/TaskStore，不解析CLI输出。原桌面仅显式`--legacy`暂留，不与同身份Agent并行。旧CLI子命令保留原线格式/退出码，取得统一身份锁；旧协议转换不支持时明确说明，禁止暗开第二个宿主。

新增模块：`node/management_cli.rs`、`node/agent_web.rs`、`desktop/agent_shell.rs`、`node/migration.rs`；二进制`src/bin/p2p-node.rs`。现有原生输入组件可复用；新桌面后台线程只运行IPC客户端，不能拥有网络节点业务。关窗口只停止管理轮询，显式“停止节点”才发Stop。离线编辑持目录锁与身份锁，revision加1并原子替换，运行中拒绝，不根据连接失败就假定Agent已停止。

## 管理行为

CLI status/config/tasks/services/connect/disconnect/send/pause/resume/cancel/forward/password/trust/config-apply/request，JSON响应保持API错误code。密码从stdin或私有输入读取，不出现在进程参数/日志；普通命令失败非零，accepted只表示入队，不伪称任务完成。低层request保留ID用于重试。初始化必须显式，已有配置不覆盖。配置export隐藏verifier；offline apply也保留verifier，密码有独立设置入口。

Web只连接IPC，无核心句柄/独立身份；复用原loopback Host/Origin/Bearer token/请求上限，禁公网bind、禁通配Host、拒跨源与无授权调用。token由私有文件读取，不放URL；不允许网页任意调用离线迁移/安装。草稿独立于已提交revision，结构编辑先收集；冲突/错误保留草稿，用户主动重新加载才替换。

原生窗口提供状态、文件任务/发送、连接、授权服务查询及本机转发增删启停。配置编辑有独立草稿；离线时仍可显示明确连接错误，不能创建新身份顶替原Agent。事件断档以快照重建，完成状态只来自Agent任务记录。

显式首次操作“初始化并启动节点”允许启动同包`p2p-agent --initialize`，仅宿主创建缺失身份/配置；窗口不读私钥、不解析子进程stdout。普通打开窗口只附着，后台登录启动不隐式初始化。信令等重启字段提供`ConfigureRestart`：用户明确提交重启配置，沿用revision/全量校验/持久根提交，核心结束，监听与任务收尾，响应后宿主退出；后续用户再启动。提交后restart_pending拒绝新变更，结果未知可同ID重试。此动作允许有计划地中断业务，不伪装无损热更新；普通ApplyConfig仍拒restart字段。

## 迁移

迁移先预览：源配置/schema、身份NodeId、授权来源、任务schema、文件冲突策略及目标冲突。执行必须持目标目录锁与源旧实例锁，确认身份锁可得。备份源到新的只读候选包，绝不删除源或覆盖已存在目标；按legacy desktop显式peer模式保留旧P2PD，不悄悄宣称已升级统一协议。旧CLI的配置导入只能明确保留其精确peer/target和“不覆盖文件”策略；没有语义等价适配时拒绝导入，不转成桌面备份发布策略。损坏/未知schema拒绝。导入来源必须LegacyImport，不能伪造密码证明。

任务文件必须通过TaskStore校验，保留TaskId、checkpoint、取消和发布回执，不从摘要重建任务。旧所有者退出后才能复制一致快照。恢复备份是独立显式动作，禁止自动回退权限或覆盖迁移后新文件。

## 安装与升级

新增`packaging/node/`：Linux systemd用户服务模板、macOS LaunchAgent模板、Windows当前用户登录任务脚本；不默认注册系统管理员服务。Ubuntu Server允许专用受限用户systemd系统服务，明确目录权限与HOME，禁止root作为默认建议。安装仅复制用户选择的二进制并生成配置，不在本轮实际执行。

包中同时携带agent/node/desktop且版本一致；Linux无GUI包只需agent/node。升级顺序：查询API与build版本→Stop并等进程退出/两把锁释放→备份二进制和配置根→替换版本→启动→查询revision/NodeId。API不兼容保留错误，不自动启动内嵌旧节点。失败保留新数据；旧二进制仅在能读取当前schema且不会回退撤销时回退，否则保持停机供手动处理。卸载只移启动项/二进制，数据需单独明确选择。

跨平台编译、安装、登录后台、窗口重开、双所有者、升级故障全部测试后置。模板存在不代表已安装，编译通过不代表上述行为已验收。
