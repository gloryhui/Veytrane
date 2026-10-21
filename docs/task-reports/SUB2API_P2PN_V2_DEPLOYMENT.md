# Sub2API 专用网关与 Windows 分发包升级到 P2PN v2

验收时间：2026-10-21 19:16（Asia/Shanghai）。构建源码为干净提交 `2bb445707435e02b72dc5d479861f69765b9e6b0`，沿用已统一的 P2PN v2 协议。本记录补充[主节点部署验收](P2PN_V2_DEPLOYMENT.md)，不改变当时的验收范围。

当前状态：后续已将专用网关和 Windows 包更新为 Veytrane 品牌，见[最新部署验收](VEYTRANE_DEPLOYMENT.md)。本文保留此前分发包名称、校验值和原始验收范围。

## 已部署与已交付范围

| 组件 | 本次结果 |
| --- | --- |
| Ubuntu 独立 `p2p-sub2api-gateway` | 已替换为 P2PN v2 构建，systemd 用户服务 enabled / active，信令 online；实际进程的可执行路径及文件 SHA-256 与安装记录一致。 |
| Windows x64 专用客户端 | 已重新交叉构建并生成 `Sub2API-Windows-x64.zip`，沿用原专用凭证与网关身份；未连接朋友的 Windows 主机，尚无该主机的安装或实机运行结果。 |
| Mac / Ubuntu 通用主节点 | 保持原有 `50a167f` 安装；本次未重启主 Agent 或管理 Web，既有入口复核通过。 |

Windows 包来自官方 Rust `x86_64-pc-windows-gnullvm` 标准库及既有 LLVM-MinGW 工具链，仍未做 Authenticode 签名。客户端与网关使用相同 P2PN v2 核心；Linux 共享客户端实现的实际接入证据列在下文，不替代 Windows 实机结果。

| 制品 | SHA-256 |
| --- | --- |
| Ubuntu 网关可执行文件 | `00cf91f0a936f579ead72ed04dcc6cab10104da97f783d06d7adb90883983e00` |
| Windows ZIP（4,286,624 字节） | `ca62c7c6656c9f21f2195b96d7918e55c6662b1050ebed396bfacb035d1943a8` |

安装路径、完整构建 manifest 和分发 ZIP 保存在发行者私有目录，不随文档进入 Git。客户端未暴露版本查询接口，Windows 源码来源由干净构建记录、包外 manifest 的源码/文件哈希与 ZIP 校验值关联，不宣称已从运行中的 Windows 进程读取构建版本。

## 身份、授权与部署过程

升级前备份专用网关数据、安装元数据及 systemd 服务定义，同时记录通用主节点状态。新二进制先完成构建与校验，随后用服务原有的 SIGINT 优雅停机，确认 PID 为 0 并实际取得 `gateway.lock`，再保存停机备份和切换可执行路径。旧二进制与服务定义继续保留。

没有重新运行 `--init`，没有改写 `gateway.json` 或 `identity.key`；两者与停机前备份逐字节一致，运行 NodeId 仍与原安装记录一致。专用网关使用自己的 `GatewayProfile`，没有通用 Agent 的 `settings.peer_modes` 字段需要删除。

Windows 构建继续使用同一份私有 profile，程序中嵌入的网关身份与专用凭证复核一致。每台电脑仍从当前用户的 `%LOCALAPPDATA%\Sub2API-Client\identity.key` 读取独立身份，新包不包含预生成身份。没有接触朋友电脑的这份文件，后续替换程序时应保留整个数据目录。

授权范围仍是：当前通过专用密码认证的设备仅获得 `sub2api → Ubuntu 回环 :8080`；文件、测速和通用管理入口禁用。网关只维护会话期间的内存 ACL，重启后由客户端重新认证恢复，不将临时 ACL 当作持久授权列表。

## 验证结果

- 8 项专用模式/恢复策略测试通过，含真实 QUIC 下两个独立身份共用凭证、错误密码拒绝、非 Sub2API 服务拒绝、File/Speed 拒绝及初始化不覆盖旧身份。
- 显式执行原本需要部署环境的验收测试：Mac 临时独立身份连接实际 Ubuntu 专用网关，Sub2API HTTP 200；目录仅返回 `sub2api`，`ubuntu-admin`、`samba`、`streamdesk` 开流均被拒绝。该测试单独执行后为 1 passed、0 ignored。
- Ubuntu 隔离目录运行与 Windows 共用 `run_client` 的 Linux 测试工具：首次 HTTP 200；重启网关后，同一客户端进程恢复 HTTP 200；重启客户端后再次 200，身份文件逐字节一致。测试用户服务已停止，测试身份及详细日志留在私有证据目录。
- Windows 包检查通过：PE x64、ZIP CRC、四文件白名单、SHA-256、BAT CRLF 及带空格路径调用；DLL 导入仅为 Windows 系统 DLL。ZIP 不含 profile JSON、网关状态、预置身份或通用管理令牌；专用共享凭证按原设计嵌入 EXE，整个 ZIP 仍须按受控分发材料保管。
- Ubuntu 通用主节点的配置、身份、任务库、管理令牌哈希及 Agent / Web PID、启动时间与升级前一致。Mac 侧导航、两端节点管理、Sub2API、StreamDesk、Nikki 返回 200；iStore 返回与原基线一致的 403 登录页，Chromium 未认证返回 401；Samba 协商为 SMB 3.0.2。已有端口映射未修改。

Windows 构建仅产生一条工具链的 `-no-pie` 未使用参数提示，构建成功；没有把该提示当作 Windows 运行证据。未验收朋友 Windows 上的 BAT、默认浏览器、物理休眠/断网或长期运行，也未操作 Sub2API 的真实业务账号。

## Windows 后续替换与恢复

关闭旧程序，将新版 ZIP 完整解压后使用新的 `Start-Sub2API.bat`。保留 `%LOCALAPPDATA%\Sub2API-Client`，不要复制其他电脑的身份，不要删除原身份来处理连接问题。新网关采用 P2PN v2，旧协议客户端不能继续连接，需使用这次新包。

网关故障恢复先停止服务并确认锁释放，再按备份服务定义恢复旧可执行路径或安装修复构建。程序与协议需成对选择；不能只回退一端。恢复启动路径不等于恢复旧身份或校验值，不自动覆盖运行后的状态。

架构位置见[部署关系](../architecture-current.html#deployment)，首次配置及构建入口见[专用分发说明](../../packaging/sub2api-windows/README.md)。私有 profile、密码校验值、NodeId、身份文件、备份、完整日志和带嵌入凭证的 ZIP 均未提交。
