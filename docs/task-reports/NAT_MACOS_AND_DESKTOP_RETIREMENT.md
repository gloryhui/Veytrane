# macOS NAT 测试修复与内嵌旧桌面退役（2026-10-21）

## NAT / STUN 修复

- 保留原有 EIM、ADM、APDM 和服务器地址解析四项测试，没有添加忽略标记或环境不足时提前返回。
- RFC 5780 编排抽出私有查询边界；生产路径仍使用同一个 UDP socket 和原有 STUN 查询。测试将两个逻辑目标 IP、两个逻辑目标端口映射到三个 `127.0.0.1` 随机端口，继续发送、解码和核对真实 STUN 报文。
- 测试额外断言探测次数、目标顺序、OTHER-ADDRESS 和观测目标；EIM 多发 Test III 会失败。无须管理员权限、额外网卡或 `127.0.0.2` 回环别名，CI 中的别名设置已删除。
- 两个 STUN 解析入口都在 DNS 前验证纯数字点分 IPv4，非法值不再被当作主机名解析。合法 IPv4、IPv6 和主机名继续支持，新增对应正反例。

## 旧桌面退役

按只维护当前版本的要求，删除 `p2p-desktop --legacy` 入口、`desktop::run` 重导出及 `src/desktop/legacy/`。

- 删除旧窗口、内嵌后台、登录启动和通知实现；当前 Agent 工作台及托盘继续保留。
- 原进程回归框架及 4 个业务场景（共 5 项测试）迁入 `src/node/core/process_tests.rs`，保留文件/目录、暂停、信令重连、权限错误、端点崩溃与恢复覆盖；子进程入口随 `module_path!()` 定位，脱离 GUI feature。
- 保留当前托盘图标回归；删除仅测试已退役界面功能的用例及旧通知专属的 ignored fixture。
- 清理无调用的界面组件、命令、并发输入模式、旧会话类型别名和专属依赖。Cargo.lock 仅移除不再需要的依赖及引用，共减少 16 个包，没有新增或升级包版本。
- Linux 原生托盘脚本改用当前 Agent 桌面、隔离空节点目录及独立子窗口语义；删除旧通知脚本与 CI 步骤。当前 Agent 工作台未实现原生系统通知，文档明确标注。
- 交互架构图同步为 22 个模块、4 个视图，移除旧桌面节点及依赖。

## 验证

- `cargo test --locked --all-features --lib nat:: -- --test-threads=4`：51 项通过，0 失败，0 忽略；原四项测试均通过。
- `cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4`：库 682 项、集成 8 项，共 **690 项通过、0 失败、1 忽略**。忽略项仍是要求 `SUB2API_LIVE_PROFILE` 和部署网关的既有线上测试，与 NAT 无关。通过数变化包含退役的旧界面专属测试。
- `cargo test --locked --no-default-features --features node --lib node::core::process_tests -- --test-threads=4`：5 项通过，0 失败、0 忽略；确认迁移后的进程回归不需要 GUI。
- `cargo check --locked --no-default-features --all-targets`、`cargo check --locked --all-features --all-targets`、`cargo clippy --locked --all-features --all-targets -- -D warnings`：通过；项目自身无编译警告。
- `python3 scripts/package-desktop-tests.py`：8 项通过；`python3 scripts/node-host-tests.py`：5 项通过。
- 架构边界检查、HTML 图数据与源码链接检查、内联 JS 语法检查、Python 脚本语法、Rust 格式与差异空白检查：通过。
- macOS `lo0` 检查没有 `127.0.0.2` 别名；本次没有添加别名或跳过上述四项测试。

Linux 原生托盘脚本本次仅做语法及源码路径审阅，原生运行由 Linux CI 执行；不能以 Mac 的编译结果替代 Windows/Linux 桌面验收。本次仅修改仓库，不替换已安装程序、不重启服务、不修改本机回环地址。
