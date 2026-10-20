# 本机测试与回归交付记录

日期：2026-10-20，Asia/Shanghai。执行者：Codex。状态：**本机自检通过，待独立审核；真实四设备与发布验收未通过**。本轮按照用户最新指令执行测试并直接修复问题，取代上一轮“测试后置”。

## 精确对象

- 分支：`codex/node-implementation`；本轮起点：`dad09f8c3f93e10a370cfef7d0afb36fe46ff1cc`。
- 主线及本轮 fetch 后 `origin/main`：`7fbd50a332474d9269f1210518c16721a500389c`，未合并。
- 产品代码冻结：`69e3c9216e20bf0b3480235eb5be218327f79009`。
- 测试夹具后续修正：`7c6ac84e970949e0890e6ecf77c190479b24a3e6`；仅修改测试中的时间预算和 Python 冒烟执行路径，不改生产超时/业务协议/权限。
- 最终交付定位：包含本报告的文档提交，准确完整 SHA 在交付回复与远端分支提供。本报告之后没有产品代码改动。
- 原缺陷基线：`eb4b9d9a0b8eb6994faa2c645cb90e841b711b30`，独立 worktree 构建。旧候选/审核报告保留原 SHA 的历史结论。
- Windows 10 19045 x64；Rust/Cargo 1.98.1，Python；本机 Ubuntu WSL，内核 6.18.54.1，Rust 1.98.1。Linux 最终全量以现有 nobody（65534）运行；另以 root 作为跨 UID 拒绝的攻击端。WSL 不是第二台 NAT 设备。

## 发现问题及修复

| 实测问题 | 处理与有效回归 |
| --- | --- |
| init 初始化失败：规则类型 derived Default 的 version=0，校验仅接受1 | 显式 Default 使用当前 schema；真实 Agent/CLI init、重启及宿主安装均通过 |
| 原生“目录预览”实际直接 Send，未进入确认流程 | 增加 PreviewDirectory IPC/CLI/Web，原生选择目录调用它；三 Agent 实际预览→确认→文件/空目录，预览不泄漏绝对源路径 |
| 限速器等待吞掉 pause watch 通知 | next_input 读取并标记 pause 状态；1KiB/s 等待中暂停的核心回归，以及8MiB进程传输暂停/停启保Paused/继续/取消通过。初版 borrow 重复Pause导致Linux7项失败，改borrow_and_update后全量通过 |
| Web 网络/网关失败丢失原变更 request_id | 保留原请求重试，未知结果期间限制其他变更，网关回显ID；实际停Agent保存→重启→重试原请求，revision只提交一次 |
| 未配置信令仍启动STUN并循环连接空地址 | 明确Unconfigured，不准备网络/发起连接；配置后可准备网络；真实status断言无失败诊断 |
| Unix跨UID拒绝使整个监听返回错误、Agent退出 | 拒绝仅关闭该连接并继续accept；root越过0700/0600后仍被UID拒绝，nobody管理仍可用、正常stop和三锁释放 |
| Windows PowerShell5.1在捕获模式加WindowStyle导致宿主SID查询失败 | Python CREATE_NO_WINDOW隐藏子进程；移除不适用参数；真实当前用户Limited计划任务启停、跨SHA升级/卸载通过 |
| desktop-e2e 仍只筛 desktop::，遗漏迁到node的核心测试 | 跑全lib并匹配两模块；13个case、20个真实子进程proof实际执行 |
| 旧CLI端到端脚本同一身份同时起多个进程，被新互锁正确拒绝 | 顺序停止并wait旧隧道释放身份后测速/推文件；新Agent另测同一TCP流跨文件传输；未放松产品锁 |
| 随机临时二进制路径反复触发Windows防火墙提示 | 冒烟二进制改固定target/node-smoke-bin路径，同内容复用；Unix改用户缓存避免WSL挂载copystat权限失败；无系统防火墙/通知/代理修改 |
| Linux满载全量中空闲回收测试在认证前就触发150ms grace | 单独旧用例可通过，全量有1项失败，明确失败在认证而非idle断言；仅测试提供3s认证预算、250ms业务idle、8s退出上界，保留真实认证和最终退出断言，精确grace由暂停时钟测试覆盖；Windows该项及Linux全量复跑通过 |

## 三个原缺陷逐项验收

- BUG-01：旧Direct/Relay都实际观测到只停信令后断流；冻结候选两路径各40轮请求+长流保持，信令恢复后同身份登记、重启客户端建立新会话通过。见[T02](T02.md)。
- BUG-02：旧Ubuntu真实SIGHUP A/B→B/C且C被占时只剩B、API仍A/B且无error；修复同场景保A/B+报错，成功重试切B/C、清错，已接纳A流不被移除监听打断。Windows另验Web。见[T03](T03.md)。
- BUG-03：旧两类管理表单实际浏览器复现；修复后文本空白、启用/自动启动、增删、失败/成功保存和主动重载逐项通过。见[T01](T01.md)。

这三项分别验证，未用架构实现或编译通过替代缺陷证据。这里的“通过”仅是本机专项结果，不是独立审核、关闭Issue或真实网络结论。

## 测试与检查

| 完整命令/范围 | 实际结果（退出0，除明确失败/跳过记录） | 证据 |
| --- | --- | --- |
| `cargo test --locked --all-targets --no-fail-fast`，Windows默认 | 642库+7集成，0失败/忽略 | target/node-regression-default-final.log |
| `cargo test --locked --features gui --all-targets --no-fail-fast`，Windows冻结69e3候选 | 664库+7集成，0失败/忽略；库77.74秒 | target/node-gui-frozen-final.log |
| `cargo test --locked --no-default-features --all-targets --no-fail-fast`，Windows旧CLI | 364库+4集成，0失败/忽略；node进程测试被feature排除，不算通过 | target/node-regression-legacy.log |
| `cargo test --locked --all-targets --no-fail-fast`，Ubuntu nobody，含最终夹具 | 657库+7集成，0失败/忽略；库51.55秒 | target/node-wsl-fixture-fixed-final.log |
| `cargo test --locked --test node_agent_process -- --nocapture`，Windows/Linux | 各3项通过；最终全量又覆盖同样3项 | target/node-agent-process-concurrent-final.log、node-wsl-agent-concurrent-final.log |
| `cargo test --locked --lib tunnel::tests::认证后业务静默会释放_active -- --exact --nocapture`，Windows最终夹具 | 实际1项通过，641项过滤；非0项筛选 | target/node-windows-idle-fixture-final.log |
| `cargo clippy --locked --all-targets -- -D warnings` | 通过 | target/node-clippy-default-final.log |
| `cargo clippy --locked --features gui --all-targets -- -D warnings` | 最终夹具后通过 | target/node-clippy-fixture-final.log |
| `cargo clippy --locked --no-default-features --all-targets -- -D warnings` | 通过 | target/node-clippy-legacy-final.log |
| `cargo fmt --all -- --check`、`git diff --check` | 通过 | 收尾检查 |
| 两Web script提取后 `node --check -`；7个Python变更文件AST | 通过 | 仅语法，不冒称UI测试 |
| `python -X utf8 scripts/node-host-tests.py` | 4项通过 | 宿主边界与不可覆盖版本目录 |
| `python -X utf8 scripts/package-desktop-tests.py` | 8项通过 | 打包约束 |
| `python -X utf8 scripts/macos-intel-packaging-tests.py` | 3项因平台跳过，不算Mac通过 | target/node-macos-package-tests.log |
| `python -X utf8 scripts/desktop-e2e.py --output C:/Users/glory/AppData/Local/Temp/p2p-node-desktop-regression-20261020` | matrix PASS，13case/20真实进程proof，含杀进程续传和发布崩溃恢复 | 输出matrix.json，运行时dad+dirty，非冻结二进制 |
| `P2P_E2E_BIN=/var/tmp/p2p-node-validation-tools/target/debug/p2p_file bash /var/tmp/p2p-node-validation-user/e2e.sh`，Ubuntu nobody | 脚本为仓库e2e.sh去CRLF副本；IPv4、IPv6和受控Relay三场景均通过；1MB/并发、2MB文件hash、测速bytes>0、idle重连 | target/node-cli-e2e-wsl-final.log |
| `cargo build --locked --features gui --bins`，69e3干净工作区 | 三bin build-info同SHA、clean；构建成功 | target/node-clean-candidate-build.log |
| `python -X utf8 scripts/node-host-tests.py --windows-live target/debug` | 当前用户Limited任务，真实启动/IPC stop/三锁/同身份重启通过，测试任务注销 | 本机实跑；不含登录事件 |
| `python -X utf8 scripts/node-host-tests.py --windows-upgrade target/node-host-old target/debug` | dad+dirty旧构建→69e3clean新构建，实际CLI安装/升级/卸载，身份/revision/非默认配置不变；旧版本/数据保留 | target/node-host-upgrade-final.log |
| `python3 scripts/node-ipc-uid-smoke.py --agent /var/tmp/p2p-node-validation-tools/target/debug/p2p-agent --node /var/tmp/p2p-node-validation-tools/target/debug/p2p-node`，Ubuntu root | 拒UID0；UID65534仍可管理，目录700/socket600、退出及锁正常 | target/node-frozen-uid.log |

证据摘要与原始日志SHA256见[脱敏证据清单](LOCAL_REGRESSION_EVIDENCE.json)。原始日志/临时截图在ignored target或外部临时输出，未上传公网探测地址/身份/凭据。bin测试目标显示0单元测试不累计为测试通过；真实失败曾出现并如上逐项处理，没有按历史DNS/127.0.0.2问题豁免新失败。

## Agent完整流程与管理入口

真实信令+3个独立Agent/CLI进程，使用测试新身份；“mac/win”是角色名称，实际分别在Windows和Linux本机重跑，不是物理Mac。默认网络有真实NIC候选及STUN探测，不能称所有Agent流量都隔离loopback。证明统一连接、管理员授权来源、TCP-only能力、两peer各两规则、同一TCP长流跨单文件传输、字节/hash和双端完成；目录预览后确认/空目录；限速暂停/停启保持Paused/继续/持久取消；测速任务启动/停止；服务撤销及强撤销隔离另一peer。测速任务本轮不声称完成30秒性能测量。

真实CLI另验证超限IPC长度及API版本拒绝、三锁、初始化不覆盖、配置监听/磁盘失败不提交、revision冲突、同ID重试/异参冲突、手动停止不随保存复活、重启配置；迁移以非空取消任务（generation42）验证原始任务字节/身份/LegacyImport来源和源数据保持，过期预览指纹拒绝。

真实浏览器新Web：跨表编辑、增删保留、非法监听保存失败、成功保存、CLI另入口推进revision后的冲突、主动重载；停Agent→保存失败→重启后按原ID重试成功。HTTP实际断言缺token401/错token401/跨Origin403/外Host403/正确认证200。截图 `target/node-agent-web-conflict.png` 和 `target/node-agent-web-retried.png`。

原生Windows窗口实际创建并关闭，前后Agent仍运行且同身份/revision。系统截图API持续超时，GPUI控件AX不可读，故**未验收内容渲染或控件操作**；只按当前窗口状态发送Alt+F4关闭。没有操作系统安全弹窗，也没有把Web交互当作原生交互。

## 打包预检

实际运行 `python -X utf8 scripts/package-desktop.py --binary target/debug/p2p-desktop.exe --target x86_64-pc-windows-msvc --output C:/Users/glory/AppData/Local/Temp/p2p-node-package-preflight-20261020 --allow-dirty`；再以 `scripts/verify-desktop-package.py` 对输出candidate.json加 `--allow-preflight` 完整验证通过。schema v2、三二进制、836依赖来源材料。

该包是dad+dirty/debug快照的**本地预检，candidate=false**，不是69e3干净构建的发布包。归档182592856字节，SHA256 `5035229b69d9ec2aab7357e44074fbd6089c0639bcc1567b8d68d4828367a5f5`。没有发布、签名或部署。

## 清理、限制与交接

所有自建fixture、echo、signal、Web/Agent/原生测试进程均已退出；本机查询无p2p测试进程，当前用户测试计划任务已注销。普通smoke私有身份目录由Fixture自动清理。额外删除baseline worktree与原生UI临时目录的组合清理命令被自动审批以“blocked by policy”拒绝，未重试删除：`target/node-baseline`（干净detached eb4）、`C:/Users/glory/AppData/Local/Temp/p2p-agent-ui-gep_i_n3`（仅本轮合成身份/配置/二进制）仍保留。没有保留运行服务；稳定执行副本、ignored日志、WSL测试工具链及本地预检包保留供复查。防火墙规则与全局通知均未改变。

未验证项：真实Mac构建/原生交互、Windows另一普通账号的IPC负例、真实登录/重启自启、全部历史旧二进制互通矩阵、持久配置各提交点杀进程、全升级失败/回退矩阵、双peer持续流公平性p95门槛、真实双NAT/四设备R01–R12和公网TLS/Relay部署。相关阶段出口保持未完成，T35不发布。没有权限降级或自动回滚已撤销授权。

提交保持现有Author/Committer，开发提交添加一次指定Codex共同作者。推送本分支后需以完整最终SHA独立审核；未经明确授权不合并main、不部署真实设备、不关闭Issue。先前静态报告和TNN报告的“待测试”均为历史记录，以本报告所列覆盖范围更新状态，不批量改为任务全部验收通过。
