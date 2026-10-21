# Mac 与 Ubuntu 的 P2PN v2 部署验收

验收时间：2026-10-21 19:02（Asia/Shanghai）。部署源码：`50a167f9a623448f9a1d8440cdcd2c5e02a8039e`。本文记录该时间点的升级结果，不是实时监控；后续文档提交不会改变已安装二进制的构建 SHA。

后续状态：同日 19:16 已完成独立 Sub2API 网关的 P2PN v2 部署及 Windows 新包构建，详见[专用分发部署验收](SUB2API_P2PN_V2_DEPLOYMENT.md)。下文保留 19:02 主节点验收时专用路径尚未升级的原始范围。

## 部署范围

| 环境 | 已更新组件 | 宿主与验证 |
| --- | --- | --- |
| Mac / aarch64-apple-darwin | Agent、管理 CLI / Web、桌面应用包 | launchd 恢复运行，Agent 保留 `--managed`；桌面包构建元数据及 ad hoc 签名验证通过。 |
| Ubuntu / x86_64-unknown-linux-gnu | 主 Agent、管理 CLI / Web | systemd 用户服务恢复运行，Agent 与 Web 均为 active、enabled。 |

两端主 Agent 均报告上述构建 SHA，信令 online、对端 connected，双向信任有效。Mac 的 7 条转发均监听且 `peer_ready=true`；通过认证的服务目录返回全部 7 个授权服务 ID。

同机独立 Sub2API 专用网关未升级，检查时仍为 active；朋友的 Windows 客户端未升级、未验收。不能将主节点的 P2PN v2 验收套用到这条独立路径。信令服务器、Relay 与后端应用也未更新。

## 备份与数据保留

升级前分别备份两端节点数据、安装元数据及服务定义；优雅停止 Web / Agent、确认所有权锁释放后，再取得一致的停机备份。Mac 另保留旧桌面应用包。备份与详细验收证据保存在各自主机的私有数据目录外层，未纳入 Git。

以停机备份为基准，解析后的完整 `node.json` 唯一变动是移除 `settings.peer_modes`。身份密钥与管理令牌逐字节一致；密码验证器、可信设备、资源授权、revision、服务、转发及 `service_ids` 均保留。没有重新初始化身份或重设凭据。

原有任务记录完整保留：Mac 1 条、Ubuntu 0 条。验收结束后仅移除本次生成的测试任务和接收文件，再次确认原任务记录与升级前一致；测试源文件及详细校验记录留在私有备份中。两端旧二进制版本与恢复元数据保留，未自动回滚数据。

## 实际传输与入口验证

Mac → Ubuntu、Ubuntu → Mac 分别传输一个 1 MiB 随机文件。两个方向的发送与接收任务均完成，确认字节数等于文件大小，接收文件 SHA-256 与源文件一致。

下表按“远端服务 → Mac 本地入口”描述映射。远端内网设备仅列角色与端口，不公开实际私有地址。7 条映射及另外 2 个本地入口在升级前后返回一致的结果。

| 服务 ID / 入口 | 远端 Ubuntu 可达目标 | Mac 访问地址 | 验证结果 |
| --- | --- | --- | --- |
| ubuntu-admin | Ubuntu 回环 `:8787` | `http://127.0.0.1:7788/` | HTML 200；带令牌的状态 API 成功并确认新构建。 |
| sub2api | Ubuntu 回环 `:8080` | `http://127.0.0.1:18081/` | HTML 200。 |
| streamdesk | 内网 StreamDesk 主机 `:18443` | `https://localhost:18443/login` | 登录页 HTML 200。 |
| samba | Ubuntu 回环 `:445` | `smb://127.0.0.1:1445` | SMB2 NEGOTIATE 成功，选中 SMB 3.0.2（`0x302`）。 |
| istore-web | 内网路由器 `:80` | `http://127.0.0.1:18082/cgi-bin/luci/` | 403，正文为 LuCI 密码登录页，与升级前一致。 |
| istore-nikki | 内网路由器 `:9090` | `http://127.0.0.1:9090/ui/` | HTML 200。 |
| chromium-web | Ubuntu 回环 `:3001` | `https://localhost:13001/` | 未认证 401；使用现有私有凭据认证后 200。 |
| Mac 节点管理（本地） | 不经过 P2P | `http://127.0.0.1:7789/` | HTML 200；带令牌的状态 API 成功并确认新构建。 |
| 服务导航（本地 Nginx） | 不经过 P2P | `http://127.0.0.1:3001/` | HTML 200。 |

Mac 导航的 3001 与 Ubuntu Chromium 的 3001 分属不同主机。iStore 的 403 与 Chromium 的未认证 401 是入口认证行为，本次通过正文及认证后响应区分了登录入口与转发故障。

Chromium 发送凭据前，将映射入口的证书指纹与通过 SSH 读取的 Ubuntu 上游证书核对一致。HTTPS 探测不代表公共 CA 信任链验收；所有凭据仅在本机私有上下文中读取，未写入本文或仓库 HTML。

## 验收边界与恢复

本次验证覆盖服务重启后的节点连接、双向文件传输、授权服务目录及上述入口。没有验收主机物理重启 / 睡眠唤醒、长时间弱网运行、Windows、Linux GUI 或桌面 GUI 交互；Mac 桌面包的 ad hoc 签名验证不代表公证发布。应用入口检查也不等于完整业务操作验收，未操作路由器设置、Chromium 内部会话或已有 SMB 共享文件。

源码回归结果见[节点协议统一记录](NODE_PROTOCOL_UNIFICATION.md)，与本文的实机部署证据分别记录。架构及部署关系见[可点击架构图](../architecture-current.html#deployment)，升级顺序与失败恢复见[节点宿主安装说明](../../packaging/node/README.md)。

出现升级故障时，先停新 Agent 并确认锁释放，核对当前配置 schema、revision、授权和任务状态，再决定修复构建或恢复旧启动路径。不得将停机备份直接覆盖运行中的节点，也不得用旧备份覆盖升级后新增任务或撤销记录。

本文仅保留构建、端口及脱敏结果；私有配置、备份、NodeId、任务 ID、密码、令牌、私钥与完整运行日志不随文档提交。
