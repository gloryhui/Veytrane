# 遗留兼容与废弃入口清理

> 后续更新：本报告记录第一轮清理；其中保留旧协议的状态已由 [节点协议统一](NODE_PROTOCOL_UNIFICATION.md) 取代。

日期：2026-10-21。基线：`f840afb`。范围：只维护当前使用版本，删除无当前生产调用的入口、别名和配置升级分支；没有修改用户数据、端口映射或已安装服务。

## 已清理

- `node/mod.rs` 的旧平铺 public 别名：集成测试和残留内部导入改用实际所属层路径，根模块只声明六层。
- 旧桌面数据导入：删除 `adapters/migration.rs`、三个 migrate 命令、首次启动迁移卡片、退出时迁移状态分支、宿主迁移标记分支，以及已失效的导入规格。
- 旧配置：删除 schema 1–10 升级、旧目录发现、Upload 自动改为 Both、旧通知/后台偏好及其独占 `preferences.rs`。移除只有旧偏好 UI 使用的五个写入入口、旧目标筛选器、迁移专用任务读取函数及失效错误类型。
- 未实现的 `LegacyCliV3`：配置校验从未允许此模式，现删除枚举及三处不可达会话分支；JSON 边界直接拒绝。
- NAT 旧方法别名 `is_punchable`、低于 Cargo 当前 MSRV 的托盘 Clippy 豁免。
- 旧 `desktop-e2e.py` 替换为 `node-core-e2e.py`：使用 `--no-default-features --features node --lib node::core::`，不再为核心进程矩阵编译 GPUI。仍要求零失败、零忽略、完整场景和真实进程证明；没有放宽通过条件。
- 同步可点击架构图的适配器、配置模块、启动链路、文件列表及源链接；浏览器验证节点分层视图和 CLI/HTTP 节点详情。

删除了已退役功能的专属迁移/偏好测试，保留当前业务回归。补充/调整了旧版本拒绝且不改写原文件、不能从旧格式导入密码或授权、初始化保留已有身份且拒绝覆盖 node.json、显式上传测速方向保持不变的测试。

## 保留依据

- `core/protocol.rs`、`transfer.rs`、`tunnel.rs` 仍共享当前文件/测速/隧道的类型、codec、授权和恢复实现；`LegacyDesktopV1` 分支仍与独立核心的执行和真实进程测试耦合。本次没有宣称仓库已不存在任何旧协议分支；拆掉这部分需整体切换流分发和对应协议回归。
- `core/config.rs` 的规则校验和测速类型仍由 Agent、Web、桌面使用，配置读写仍由独立核心的认证/信任路径与回归使用，不能整文件删除。Agent 持久格式仍由 `core/settings.rs` 管理。
- `src/main.rs` 仍提供独立 CLI 的发送、接收、隧道和服务管理，不把同名底层模块当作废文件。信令和 Relay 也被当前节点共享。
- 四设备配置样例是当前 NodeSettings 草稿，执行卡仍在引用；打包模板、图标、脚本夹具也有当前消费方。历史报告保留事实并标注新入口。
- `TrustSource::LegacyImport` 是已有授权记录的来源标识，不是自动迁移逻辑，不擅自改写为管理员或密码验证授权。

## 验证

| 检查 | 结果 |
| --- | --- |
| `cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4` | 675 个库测试 + 8 个集成测试通过，0 失败；原有部署测试 1 项忽略。 |
| 配置专项测试 | 22 项通过，包括旧格式拒绝、原文件保留、并发授权/密码写入。 |
| `cargo check --locked --no-default-features --all-targets` | 通过。 |
| `node-core-e2e.py` | PASS：283 项通过，0 失败、0 忽略；13 个场景、21 条真实进程证明全部齐备。 |
| `cargo clippy --locked --all-features --all-targets -- -D warnings` | 通过；仅第三方依赖既有 future-incompatibility 提示。 |
| 打包 / 宿主管理 Python 测试 | 8 + 5 项通过。 |
| 格式、diff、架构边界与图谱源链接 | 通过；22 个模块、4 个视图。 |

全量日志：`/tmp/p2p-clean-regression.log`。核心矩阵：`/tmp/p2p-clean-core-verified/`。所有运行证据写入仓库外临时目录；未提交凭据、运行配置或生成二进制。

现有需要真实部署凭据的网关测试继续保留原有 `#[ignore]`，本轮未增加跳过项。Windows/Linux 原生交互、已安装版本及真实跨设备网络不在此次本机回归范围内。
