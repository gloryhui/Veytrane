# Sub2API 固定中继版分发与验收

日期：2026-10-28（Asia/Shanghai）。本次替代同日较早的无中继 Windows 包，满足专用分发必须内置固定中继的要求。优先尝试直连，直连未及时建立时自动竞速中继，收件人无需填写中继或服务器地址。

## 实际配置

- 在现有固定公网节点的原信令进程启用 UDP Relay，TCP 信令和 UDP 中继共用端口号 8900。服务仍由原 systemd 单元管理，通过新增 drop-in 保留原启动参数、短设备 ID 数据库和现有可执行文件。
- 用户已在云端放行 UDP 8900；之后公网实际抓包收到中继登记、响应和业务数据的双向 UDP 流量。
- Ubuntu 专用网关的 `gateway.json` 与客户端私有构建输入配置同一固定 `relay_server`。网关程序仍是 `6b08419` 构建，SHA-256 为 `e3e63d79e8adabb9844235272d60cb9798bc4d0e174a60d6aff130f729f1d9ff`；本次只更新中继配置并重启专用网关。
- 网关优雅停机、确认 PID 为 0 并取得目录锁后备份配置。只有 `relay_server` 字段改变，设备身份、密码校验值及其他配置保持一致。信令节点也在停机后保存原数据库与启动定义。
- 不修改通用 Agent 端口映射、服务导航或 Sub2API 应用配置。中继仍转发端到端加密的 QUIC 数据报，原服务权限检查保持生效。

## 构建与防漏配

Windows 程序从干净提交 `edcddaf53baef1bce32754b416b36b11cfbefd8e` 构建，采用既有 Rust 1.98.1、官方 gnullvm 标准库和 LLVM-MinGW 工具链。文档在构建后补充，不改变该程序的来源。

打包命令现在必须传入私有 `--profile`，缺少固定中继、EXE 与给定 profile 不匹配时会拒绝打包；profile 文件不进入 ZIP。原私有 profile 只增加固定中继，原网关身份和专用访问密码不变。

| 制品 | 值 |
| --- | --- |
| 桌面分发文件 | `Veytrane-Sub2API-Windows-x64-20261028-Relay.zip` |
| ZIP 大小 | 4,731,329 字节 |
| ZIP SHA-256 | `86a2d380eddab4b48d0653b89704a9a153a9ccc0c3b53085bfc2c258f317ce6a` |
| EXE SHA-256 | `d4f2f7a2a9164c3104ffb9aef898b859d8b95273f7311a2573abfb82b97bfa2d` |

ZIP 仍只有 `Sub2API.exe`、`Start-Sub2API.bat`、`使用说明.txt` 与许可证四个文件。PE x64、ZIP CRC、BAT CRLF 与路径引号、系统 DLL 导入及嵌入 profile 的一致性均通过检查。没有预生成的设备身份或单独的凭据文件；包内 EXE 本身仍含共享接入凭据，应私下分发。

## 实际中继验收

在 Ubuntu 创建临时独立测试身份及单独的 systemd 用户测试服务。防火墙规则只匹配该测试服务的 cgroup，允许访问固定公网中继的 UDP 8900，阻断其他 IPv4 UDP 与 IPv6 UDP；正常网关和其他进程不受测试规则限制。

显式运行专用网关的部署验收测试，并启用 `SUB2API_LIVE_REQUIRE_RELAY`：

- 断言实际 QUIC 对端地址就是配置的固定中继，不能用直连成功替代中继验收。
- 防火墙统计确认 6 个直连 UDP 包被阻断、38 个发往固定中继的 UDP 包被允许。
- 通过中继完成设备与专用凭据认证，实际 Sub2API HTTP 请求返回 200。
- 服务目录仅列 `sub2api`，访问 `ubuntu-admin`、`samba`、`streamdesk` 被拒绝。
- 该测试 1 passed、0 ignored；在不阻断直连的环境中，使用新 profile 的实际网关验收也为 1 passed、0 ignored。
- 测试结束后临时服务停止，IPv4 / IPv6 测试规则与链均已删除。专用网关、通用 Agent / Web 均 active，Sub2API `/health` 返回 200。

本地回归：36 项中继相关 Rust 测试、8 项专用产品测试、3 项打包防漏配测试通过，格式及差异检查通过。实际中继验收另行执行，如上所述。

Windows 朋友电脑尚未接入；交叉构建和 Linux 共享核心验收不替代 Windows 双击 BAT、浏览器与朋友网络环境验证。当前中继使用 UDP；完全封锁 UDP 的网络仍不在该通道的覆盖范围内。

## 使用与留存

朋友关闭旧程序，完整解压新的 `-Relay.zip` 后双击 `Start-Sub2API.bat`，保留 `%LOCALAPPDATA%\Sub2API-Client`。网页与 API 入口仍为 `http://127.0.0.1:18081/` 和 `http://127.0.0.1:18081/v1`。账号与 API Key 按朋友各自的 Sub2API 业务权限分配。

Mac 构建、profile、manifest 和完整验收日志保存在私有目录 `~/p2p-file-builds/sub2api-relay-20261028/`；此前由本次工作生成的桌面无中继包已移入其中的 `superseded/`。Ubuntu 配置备份为 `~/.local/share/p2p-file/configuration-backups/sub2api-relay-20261028/`，公网信令节点备份为 `/opt/p2p_file/backups/sub2api-relay-20261028/`。私有配置和制品不进入源码仓库或公开 Release。
