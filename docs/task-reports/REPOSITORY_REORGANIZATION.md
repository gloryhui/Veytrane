# 仓库分层整理验证记录（2026-10-21）

> 后续状态：本记录描述分层提交时的结果。随后 NAT/STUN 四项失败已修复，内嵌旧桌面已退役；最新验证见 [修复与退役记录](NAT_MACOS_AND_DESKTOP_RETIREMENT.md)。

## 范围

基于 `f1e54f7` 工作树，按 [仓库分层说明](../REPOSITORY_LAYOUT.md) 整理源码依赖与目录。未更改部署配置、已安装程序或运行服务。交付包含本次源码分层及交互架构图。

首次源码整理保留了原有 README 和 HTML 编辑；随后按要求更新交互图，并将原有文档编辑一并纳入交付。架构图含 23 个模块、4 个视图，支持模块搜索、内部职责展开、源码跳转和深链接。部署说明保留为历史快照。

## 已执行检查

- `cargo check --locked --all-targets`：改动前基线通过。
- `cargo check --locked --no-default-features --all-targets`：整理后通过，独立 CLI 可编译。
- `cargo check --locked --all-features --all-targets`：整理后通过，包含 GUI、Agent 和 Sub2API 两种产品特性；项目自身无编译警告。
- `cargo clippy --locked --all-features --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`、`git diff --check`：通过。
- `python3 scripts/check-architecture.py`：通过；另用临时源码验证了核心→宿主、API→界面、存储→节点、默认桌面→旧窗口、共享 UI→窗口所有者这 5 类违规均会被拒绝。
- `python3 scripts/package-desktop-tests.py`：8 项通过。
- `python3 scripts/node-host-tests.py`：5 项通过。
- 入口文件改用显式层路径后，重新执行 `cargo test --locked --all-features --test node_agent_process -- --test-threads=4`：4 项通过，覆盖迁移、事务/重启/所有权、多转发资源预算及三节点撤权；该复验不重复计入下方总数。

第三方依赖 `block 0.1.6`、`proc-macro-error2 2.0.1` 仍有 Rust future-incompatibility 提示；本次未升级依赖或改写锁文件。

## 完整回归与已知失败

命令：

```sh
cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4
```

结果：库测试 **686 通过、4 失败、1 忽略**；集成测试 **8 通过**（IPv6 CLI 2、分配前块大小校验 1、Agent 真实进程 4、信令 TLS CLI 1）。合计 **694 通过、4 失败、1 忽略**。零测试的二进制/示例目标不计入通过数。

4 项失败都在未改动的 NAT 测试中：

| 测试 | 当前表现 | 改动前复核 |
| --- | --- | --- |
| RFC 5780 EIM / ADM / APDM 三项 | macOS 绑定 `127.0.0.2` 报 `AddrNotAvailable` | 同样失败；现有 GPUI CI 显式配置此回环别名 |
| STUN 解析服务器地址 | `256.256.256.256:1` 被本机解析器返回结果，未满足测试期望的错误 | 同样失败 |

复核方式：从 Git `HEAD` 导出独立临时源码，执行 `cargo test --locked --lib nat:: -- --test-threads=4`，结果 **46 通过、同样的 4 项失败**。本次没有修改本机网络设置、DNS 或跳过这些测试来制造全绿结果，也没有将其算成已通过。

## 整理过程中修正的回归与旧测试假设

- 旧进程测试写死 `desktop::e2e`、`node::publish`、`node::transfer`，搬迁后子进程会运行零个测试。改为基于 `module_path!()` 定位，重新执行多进程传输、暂停、目录碰撞、发布中断与恢复测试均通过。
- Sub2API gateway 的 peer 上限为 128，普通节点为 16。诊断文本上界与路径缓存回归原先隐含固定 16 的假设；现按实际容量验证，未放宽运行时配额或改动诊断算法。
- 无调用的 NAT 映射占位模块及其“恒定返回不支持”测试已删除；因此该占位测试不再计入当前用例总数。

## 交互架构图验证

- `python3 scripts/update-architecture-map.py --check`：23 个模块、4 个视图的数据与 HTML 一致，节点及现有源码链接有效；此检查已加入 CI。
- HTML ID 唯一，两个内联脚本均通过 `node --check`。
- Chrome 实测视图切换、模块点选、内部下钻、搜索定位、键盘选择、刷新恢复深链接、缩放、深色主题及原有 TCP 转发流程切换。
- 页面继续内联 CSS、JavaScript 和图数据，运行时不依赖外部库或额外后台进程。

## 验收边界

这是本机源码、编译和隔离进程回归证据，不代表 Windows/Linux 原生桌面、真实睡眠唤醒、跨公网长期传输或部署升级验收。旧 `--legacy` 桌面、独立 CLI 和旧协议兼容路径继续保留。
