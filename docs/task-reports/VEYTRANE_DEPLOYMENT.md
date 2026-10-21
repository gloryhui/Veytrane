# Veytrane 更名与部署验收

最终核对：2026-10-21 19:56（Asia/Shanghai）。部署源码为干净提交 `ec72ed43bb13fdfeab8974bb4a06d1338318f9ad`。产品名统一为 **Veytrane**，内部协议仍是 **P2PN v2**；文档提交不改变已安装程序的构建 SHA。

## 当前安装与交付

| 环境 | 实际结果 |
| --- | --- |
| Mac / aarch64-apple-darwin | Agent、管理 CLI / Web 和 `/Applications/Veytrane.app` 均更新至上述提交。应用包内三个组件的构建 SHA 与 clean 状态一致，ad hoc 签名验证通过；主窗口已启动并正常渲染，显示 Veytrane 与信令在线。 |
| Ubuntu / x86_64-unknown-linux-gnu | 主 Agent、管理 CLI / Web 与独立 Sub2API 网关均更新至同一提交。三个 systemd 用户服务均 enabled / active；主节点和网关信令 online。 |
| Windows x64 | 使用官方 Rust gnullvm 标准库和既有 LLVM-MinGW 工具链重新构建专用客户端，生成 Veytrane 分发包并校验。没有连接朋友的 Windows 电脑，未宣称完成其安装或实机验收。 |

Mac、Ubuntu 主节点相互 connected，双向信任有效。Mac 的 7 条映射均监听且 `peer_ready=true`。两端管理网页保留 Mac / Ubuntu 角色区分并显示 Veytrane；本机 Nginx 导航也已生效。

桌面交付文件名为 `Veytrane-Sub2API-Windows-x64-P2PN-v2.zip`；ZIP 内根目录为 `Veytrane-Sub2API-Windows-x64`，保留 `Sub2API.exe`、`Start-Sub2API.bat`、使用说明与许可证。程序与 BAT 显示 Veytrane for Sub2API。此前由本次工作生成的旧品牌桌面 ZIP 已移入私有备份，旧 Mac 应用也保留在私有备份中。

| 制品 | SHA-256 |
| --- | --- |
| Windows ZIP（4,286,818 字节） | `1b07e819379b336d5a137e318b36eff94624a02b722b78f6beceeeeea2361f80` |
| Ubuntu 独立网关可执行文件 | `9b870ffaead07f39804037d9e1803fdecbc1a0e61c115fd7f2c6b42983d5aaad` |

Windows 包仍未做 Authenticode 签名；Mac ad hoc 签名不代表 Developer ID 或公证发布。

## 备份与状态保留

两端先备份节点数据、安装元数据和启动定义；文件队列为空后优雅停机，确认目录、身份和任务锁释放，再取得一致停机备份。持久普通文件与目录纳入备份，运行期 IPC socket 由程序重建。Ubuntu 独立网关也在停机并实际取得 `gateway.lock` 后备份。所有备份及详细证据均保存在各自主机私有目录，未纳入 Git。

本次没有编辑节点配置：两端 `node.json`、`identity.key`、`admin.token` 与升级前备份逐字节一致；网关的 `gateway.json` 和 `identity.key` 也逐字节一致。运行 NodeId、revision、密码验证器、可信设备、授权、服务、转发及 `service_ids` 均保留，没有重新初始化。

原有任务 API 记录保持一致：Mac 1 条、Ubuntu 0 条。完成双向传输验收后，仅移除本次两个测试任务及接收文件，再核对原任务列表一致；测试源文件与校验记录留在私有证据中。

既有可执行命令、Bundle Identifier、启动项标识和数据路径保持稳定。Mac launchd Agent 保留 `--managed`；Ubuntu 继续使用原 systemd 用户服务。只更新安装版本、应用展示名、应用包位置及相应启动路径，没有生成第二套节点身份。

Windows 构建沿用原私有 profile，EXE 中的网关身份和专用访问凭证复核一致。每位使用者继续保留 `%LOCALAPPDATA%\Sub2API-Client`，不要删除或复制其他电脑的身份目录。

## 实际验证

| 检查 | 结果 |
| --- | --- |
| Mac ↔ Ubuntu 文件传输 | 两个方向各发送 1 MiB 随机文件，发送和接收任务均完成，确认字节数正确，接收文件 SHA-256 与源文件一致。 |
| 两端管理入口 | Mac `:7789`、映射 Ubuntu `:7788` 均返回新品牌 HTML 200；带令牌的状态 API 确认运行 `ec72ed4`。 |
| 本机导航 | `http://127.0.0.1:3001/` 返回 200，标题为 Veytrane · Service Navigation。 |
| 现有应用入口 | Sub2API、StreamDesk 登录页、Nikki 均返回 HTML 200；iStore 为与基线一致的 403 LuCI 密码登录页；Chromium 未认证 401、原凭据认证后 200；Samba 成功协商 SMB 3.0.2。 |
| Chromium 认证边界 | 发送凭据前，将映射入口证书指纹与通过 SSH 获取的 Ubuntu 上游证书核对一致；未声称公共 CA 信任链验收。 |
| 实际专用网关 | Mac 临时独立身份访问 Sub2API 返回 200；目录仅列 `sub2api`，管理、Samba、StreamDesk 开流均被拒绝。 |
| 专用客户端恢复 | Linux 共享客户端实现首次访问 200；网关重启后原客户端进程恢复 200；客户端重启后再次 200，身份文件不变。测试服务已停止。 |
| Windows 制品 | PE x64、ZIP CRC、文件白名单、SHA-256、BAT CRLF 和带引号路径检查通过；DLL 导入仅为 Windows 系统 DLL。 |

自动检查：13 项打包/宿主测试、8 项专用/恢复策略测试通过；另外显式执行 1 项实际网关测试，结果 1 passed、0 ignored。格式、架构图和分层检查通过，三个平台的对应发布构建完成。

这些结果覆盖本次更新后的主窗口启动、服务连接、文件传输和入口访问；未覆盖所有 GUI 操作、Windows 实机、主机物理重启/睡眠、长时间弱网或完整应用业务流程。入口响应与 SMB 协商不等于真实业务账号操作或共享文件读写验收。

## 恢复与后续更新

原二进制、安装元数据、启动定义及 Mac 旧应用仍在私有备份中。恢复前先优雅停止新进程并确认锁释放，再选择匹配当前配置与 P2PN v2 的程序及启动路径；不自动用备份覆盖新增任务、授权撤销或身份状态。Windows 更新保留身份目录，完整解压新包后运行 BAT。

当前架构见[可点击架构图](../architecture-current.html#deployment)，宿主操作见[安装与恢复说明](../../packaging/node/README.md)，专用包见[分发说明](../../packaging/sub2api-windows/README.md)。此前的[首次 v2 主节点升级](P2PN_V2_DEPLOYMENT.md)与[专用网关升级](SUB2API_P2PN_V2_DEPLOYMENT.md)保留各自历史时间、构建和制品校验值。

私有配置、NodeId、密码、令牌、私钥、完整日志、备份及带嵌入访问凭证的 ZIP 均未提交到仓库。

## 仓库与源码目录更名（2026-10-21）

GitHub 仓库更名为 [gloryhui/Veytrane](https://github.com/gloryhui/Veytrane)，仍是原仓库，仓库 ID、提交历史、Issue / PR 编号和默认分支保持不变。本地 Mac 源码目录改为 `Veytrane`，Mac 和 Ubuntu 源码工作副本的 `origin` 同步为 `https://github.com/gloryhui/Veytrane.git`。Ubuntu 源码工作副本目录和各主机安装目录维持原位置。

README、Cargo 仓库元数据以及文档中的仓库坐标和 Issue / PR 链接均使用新地址；可点击架构图仍通过相对路径跳转源码，新目录下的架构页已验证可访问。Rust 包名、命令、节点数据目录及启动项不随源码仓库更名。

此次操作没有重启服务或重新部署程序。改名前后核对：Mac Agent / Web、Ubuntu Agent / Web / 独立网关的 PID 和启动时间一致，Nginx 监听进程一致；节点身份、配置、管理令牌、任务 API 记录及网关身份 / 配置未变。两端信令 online、相互 connected，Mac 7 条映射均监听且远端就绪。9 个入口的只读检查与改名前一致（包括 iStore 的 403 登录页、Chromium 的未认证 401 和 Samba 的 SMB 3.0.2 协商）。配置指纹与进程快照仅保存在私有目录，未提交到 Git。

新目录下 Cargo 元数据、架构图来源路径、分层约束与差异格式检查通过；本次只修改仓库地址和文档，没有重建或切换当前运行的二进制。
