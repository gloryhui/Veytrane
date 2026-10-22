# 节点宿主安装与升级

默认使用当前用户，不需要管理员权限。数据目录与`p2p-node`、原生窗口的`--data-dir`必须一致。2026-10-22 已部署 Mac / Ubuntu 主节点的网络恢复补充修复，使用 `f9067aa` 基线加部署时未提交的修复构建，配置与 8 条映射保留，真实 Docker 网桥增删时持续 SSH 和原 P2P 通道均不中断，见[网络恢复部署验收](../../docs/task-reports/NETWORK_RECOVERY_DEPLOYMENT.md)。独立网关保留 `ec72ed4`；此前的[品牌更新验收](../../docs/task-reports/VEYTRANE_DEPLOYMENT.md)和[首次协议升级](../../docs/task-reports/P2PN_V2_DEPLOYMENT.md)作为历史记录。协议保持 P2PN v2；其他平台或安装方式仍须各自验收。

## 构建和首次配置

Ubuntu Server无需GUI：`cargo build --locked --release --bin p2p-agent --bin p2p-node`。桌面包的三平台构建入口现同时构建桌面、Agent和管理CLI；Mac三者在同一`Contents/MacOS`目录。不要只复制桌面二进制。

新安装先运行 `p2p-node init`。旧桌面迁移命令已删除。当前节点采用 P2PN v2，两端须同步升级；旧节点配置中的 `peer_modes` 字段需在停机备份后移除，每条转发必须具有 `service_ids` 映射。保留既有身份、密码验证器、资源授权和任务库；不要用重新初始化来处理升级错误。

首次信令配置可从Agent运行后的原生窗口“保存信令并停止节点”，再启动；CLI可`config`导出响应里的`result.settings`，通过`offline-apply <草稿.json> --revision 0`停机应用。密码由`p2p-node password --revision <当前revision>`从stdin输入；不要放进进程参数。服务与转发通过原生窗口、Web或`apply <草稿.json> --revision N`管理。

## 用户登录启动

将包内`node-host.py`与版本一致的二进制一起保留。以下是生命周期命令参考；安装、启停、启用或卸载命令会修改本机安装或登录启动项，按实际需要执行。

```text
python node-host.py install --bin-dir <包含agent和node的绝对目录>
python node-host.py enable
python node-host.py start
python node-host.py status
python node-host.py stop
python node-host.py disable
python node-host.py uninstall
```

可传`--data-dir`选已初始化的数据目录。install只复制到不可覆盖的SHA版本目录，不创建身份、不启用启动项。enable前先停止现有所有者；Mac加载LaunchAgent可能立即启动。Linux为systemd用户服务，Mac为LaunchAgent，Windows为当前用户SID的Limited交互登录任务；Windows Agent使用无控制台子系统。首次登录后管理窗口可以任意关闭/重开，Agent独立持有业务。

Ubuntu无桌面且无用户登录会话时，管理员可按`p2p-file-agent.system.service`模板建立专用受限用户服务；须明确HOME、数据目录与身份锁所在用户，不能把同一私钥交给不同系统用户运行。`loginctl enable-linger`和系统服务安装需要管理员明确操作，本工具不自动执行。

## 升级与失败恢复

### 从含 peer_modes 的配置升级到 P2PN v2

这次协议升级必须协调两端，不能只替换一端，也不能把含旧字段的配置直接交给新 Agent。`node-host.py upgrade` 不负责自动移除 `peer_modes`。

1. 先完成两端同一源码提交的构建，核对 Agent、管理 CLI 与桌面包内组件的构建元数据。
2. 在仓库外的私有目录备份配置、身份、授权、任务库、安装元数据与服务定义；优雅停止 Web / Agent，确认所有权锁释放后再备份一致状态。
3. 离线仅移除 `settings.peer_modes`，检查每条转发的 `service_ids` 完整；与停机备份比较，确保身份、凭据、授权、revision、任务和其他配置均保留。不要执行 `init` 重建节点。
4. 更新两端二进制与宿主启动路径，恢复 Agent / Web；Mac 保留 `--managed`，桌面包同时更新。保留旧版本与恢复元数据。
5. 核对运行构建、双向连接与信任、授权服务目录和转发监听，执行双向文件传输及校验，再按入口协议验证各服务。服务重启会中断已有 TCP 流，恢复后以新连接验证。

本次按此顺序完成 Mac / Ubuntu 主节点升级，7 条映射保留，双向 1 MiB 文件校验通过。详细入口结果、认证状态码与验收边界见[部署验收记录](../../docs/task-reports/P2PN_V2_DEPLOYMENT.md)。后续已部署独立 Sub2API v2 网关并生成新版 Windows 包，朋友 Windows 实机尚未安装验收，见[专用路径记录](../../docs/task-reports/SUB2API_P2PN_V2_DEPLOYMENT.md)。备份和凭据不得加入仓库。

### 宿主行为与失败恢复

Agent 的网络核心在所有平台和前台/托管模式下，每 5 秒观察本机接口。接口变化连续确认两次后先保留仍正常连接的通道，待传输关闭或超时才刷新候选；Docker 网桥增删、虚拟接口或地址变动不会仅凭接口变化就拆掉现有 SSH / TCP 会话。后台停顿/休眠间隔达到 30 秒时，仍会在原进程内关闭旧网络通道、重新收集 UDP/STUN 候选并重新注册信令。连续三次传输阶段失败且没有健康连接时也会刷新候选，两次刷新至少间隔 60 秒；认证失败不触发该策略。单纯信令失联仍按原退避流程处理，保留健康 Direct 流。

活跃映射和用户请求的连接按退避自动重试；只使用当前信任或进程内已有密码，不绕过鉴权。手动断开、停用设备、删除/停止映射及显式 Stop 的边界保持有效。刷新保留 Agent PID、管理 epoch、身份、授权、JSON、监听端口和临时停止状态；旧 TCP 流会断开，需要重新连接。文件任务通过原网络中断恢复策略处理，不自动恢复用户暂停的任务。

macOS 登录宿主使用 `p2p-agent --managed`，保留 `RunAtLoad`、异常退出自动重启及 30 秒优雅退出等待。`--managed` 仅为网络核心实际退出保留宿主兜底，不再因唤醒或接口变化退出 Agent。用户显式 Stop 或停止节点以保存需重启的配置仍正常退出。Mac 日志位于 `~/Library/Logs/p2p-file-node/agent.stderr.log`；恢复原因也进入管理状态的 diagnostics。

`python node-host.py upgrade --bin-dir <新版本目录>`先验证两个二进制SHA/版本/target/API一致，再请求旧Agent停机并证明目录、身份、任务锁释放；不强杀、不强行接管。保留旧版本目录与`previous-install.json`，新启动核对NodeId、revision不倒退和build SHA。失败保持数据，禁止自动回退旧授权或覆盖新任务。

失败恢复先停新Agent并确认锁释放，检查当前schema/revision和授权变更；仅在旧版本能读取当前根且不会丢失撤销/任务字段时，人工按`previous-install.json`恢复旧启动路径。否则保持停机，用同schema修复版本恢复。备份恢复不是自动数据回滚。uninstall只卸载启动项，保留全部身份、配置、任务、业务文件和版本目录。

当前 Mac / Ubuntu 的安装、服务重启及主节点互通已有上述实机证据，Mac 桌面包通过 ad hoc 签名验证。物理重启 / 重新登录、睡眠唤醒、升级中断注入、Mac 公证发布与 Windows 计划任务行为不在这次验收范围；模板/打包脚本语法检查不能替代对应实机证据。
