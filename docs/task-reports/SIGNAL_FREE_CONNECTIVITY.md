# 无信令发现与 IP 直连验证

日期：2026-10-21。实现基于 `9ebe2dd` 后的通用 Agent 源码；本记录描述本次代码与测试，不表示现有 Mac / Ubuntu 安装已升级。

## 实现范围

- Agent 支持独立的节点 v2 mDNS 公告/浏览、IP 直连与已保存地址；桌面节点管理、Web 和 CLI 通过相同 IPC 管理。
- 未配置信令时仍建立原生 IPv4 / IPv6 QUIC 端点，且不探测公共 STUN / Relay。`nearby` 提供实际监听候选与有界、过期的发现提示；短 ID 仍需信令。
- 所有路径复用原身份握手、TLS 会话绑定、P2PN v2、密码/可信设备、方向权限和资源授权门禁。未知入站先执行 QUIC Retry，握手并发与超时有界；发现不创建信任或自动连接。
- 监听/发现生命周期配置要求停止后应用；设备地址可走 revision 配置事务在线保存。专用 Sub2API 组合显式关闭通用发现和直连接入，保持既有权限范围。

## 本次自动验证

| 检查 | 结果与实际范围 |
| --- | --- |
| `cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4` | 694 passed，0 failed，1 ignored。既有 ignored 项为要求 `SUB2API_LIVE_PROFILE` 和独立实部署网关的测试，本次未提供该外部环境，未将其算入通过数。 |
| IPv4 / IPv6 无信令核心 | 两种应用层身份排序均完成真实 QUIC 和授权服务查询；保留方向权限。 |
| 安全负例 | 错误预期身份不发布连接；错误密码不能查询服务，正确凭据可在新连接重试；并发发起只发布一个授权会话。 |
| 无外部探测 | 监听本地 STUN 探针，在无信令模式启动后未收到探测包；配置了外部地址也不启用该探测或 Relay。 |
| 发现消息与缓存 | 协议隔离、身份/地址校验、地址族端口、上限、过期和 goodbye 移除。 |
| 实际 Agent / CLI 进程 | macOS 同一宿主的隔离双 Agent 通过真实 mDNS 发现；发现后未自动连接，显式请求后认证成功。 |
| 文件 / TCP / 恢复 / 撤销 | 无信令 Agent 通过 CLI 建连；发送 256,123 字节文件，两端完成且内容 BLAKE3 一致；授权 TCP 回显通过。仅重启临时测试 Agent 后按需恢复保存的地址，身份文件不变；禁用设备后服务访问失效。 |
| Python 宿主及打包 | Python 3.14 下宿主 5 项、桌面打包 8 项、Mac Intel 打包 3 项，共 16 项通过。 |
| 源码与文档 | Rust 格式、分层约束、23 模块 / 4 视图的架构图来源、Markdown 链接/锚点、SVG 与 Web JavaScript 语法检查通过；GitHub GFM 渲染与架构模块点选核对通过。 |

核心回归见 [`session/direct_tests.rs`](../../src/node/core/session/direct_tests.rs)；发现缓存回归见 [`connectivity.rs`](../../src/node/core/connectivity.rs)；进程与业务回归见 [`tests/node_agent_process/direct.rs`](../../tests/node_agent_process/direct.rs)。使用方法见[无信令连接指南](../SIGNAL_FREE_CONNECTIVITY.md)。

## 运行环境与未覆盖范围

所有新增运行验证使用随机测试身份、独立临时目录和测试进程。没有更改正在运行节点的身份、授权、任务、服务映射或配置，没有替换二进制或重启现有服务。

最后只读核对中，Mac 仍运行 `ec72ed4`、revision 10、Agent PID 83890，信令 online、对端 connected，7 条映射仍就绪；Ubuntu Agent PID 3075859，启动时间仍为 2026-10-21 19:50:17 CST。这里只保存运行版本及非凭据状态，不包含私钥、令牌、真实密码、NodeId 或完整私有配置。

未完成新的两台物理机器间局域网验收、公网 IPv6 / 单端公网 IPv4 的运营商拓扑验收、Windows 实机运行、休眠/漫游及长期弱网验证；本地回归不替代这些环境验证。使用新功能需要另外升级对应端的 Agent 和管理入口，升级应保留原身份与数据目录。
