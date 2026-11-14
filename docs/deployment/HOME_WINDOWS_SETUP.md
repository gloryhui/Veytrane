# 家用 Windows 安装 Veytrane 与 Ubuntu 服务映射交接

核对日期：2026-10-23（Asia/Shanghai）。本文交给家用 Windows 上的 Codex 执行。

本文件可单独带走。文中的相对源码链接以仓库内 `docs/deployment/` 为基准；独立副本的相对链接若不可用，请在克隆后的 Veytrane 仓库中按所列路径查阅。

本次还提供配套的 [`windows-service-navigation.template.html`](windows-service-navigation.template.html)，与本文一起带到 Windows；它复用现有 Mac 导航的全部 CSS，已改为 Windows 文案和操作方式，不能重新设计成另一种样式。页面当前显示首选端口并标注“待部署 · 模板端口”，须在安装完成后按实际配置生成正式页面。

## 可直接交给 Windows Codex 的任务

> 请阅读本文，实际安装 Windows x64 版 Veytrane 后台 Agent，配置本文全部 12 条 Ubuntu 服务映射，并设置系统开机自动启动。优先沿用 Ubuntu 服务目标的端口，遇到真实占用才使用备用端口。主方案要求开机后未登录 Windows 也能运行；仓库自带的登录启动方案仅作为受限条件下的替代，必须说明区别。
>
> 请同时使用随附的 `windows-service-navigation.template.html` 生成 Windows 本机服务汇总 HTML，保留其视觉样式、搜索筛选、分组、端点颜色和卡片布局，只按实际安装结果更新数据及必要的 Windows 操作说明。完成后保存到 Windows 用户私有导航目录，创建桌面“Veytrane 服务导航”快捷方式并打开验收，不能只在报告里列出网址。
>
> 请先检查本机系统、已有安装、数据目录、端口和源码，再执行安装。新 Windows 节点自行生成身份，不能复制 Mac / Ubuntu 的 identity.key、node.json 或管理 token。连接现有 Ubuntu 主节点，按本文追加 Windows NodeId 的信任和 12 项服务授权，保留 Mac 授权及其他配置。无需安装 Sub2API 专用客户端，也无需重新部署 Ubuntu 应用。完成后实际验证连接、映射、应用协议、后台驻留和自启动，生成本机入口导航与验收报告。
>
> 常规安装、构建、配置和可逆修复直接执行；只有缺少真实凭据、管理员权限或必须由用户输入系统密码时再请求必要协助。先完成所有不依赖该协助的工作。不要把“配置已保存”“任务已注册”“端口在监听”当成端到端验收成功；未执行重启验证时明确标记。不得为验证自启动擅自重启正在使用的电脑。

## 1. 已核对的真实环境

本次只读读取了两端实际配置，未改动任何映射、信任、运行服务或启动项：

- Mac：`~/Library/Application Support/p2p-file/node/node.json`，revision `14`，12 条映射均配置 `enabled=true`、`auto_start=true`。
- Ubuntu：`~/.local/share/p2p-file/node/node.json`，revision `15`，对应 12 项服务均配置启用。以上是配置状态，不表示应用此刻在线。
- Ubuntu 主节点完整 NodeId：`cbd82de8be77d5f813fc3ff92663b2ac`。
- Mac NodeId：`ae4c49f3d120f1e013155039eb3263ce`，目前这 12 项服务的 `allowed_peers` 都只包含该 Mac。**Windows 不会自动继承这些权限。**
- 信令：`47.101.183.194:8900`；当前两端 `signal_tls=null`、`relay_server=null`。新节点先匹配现有配置，不擅自单端启用 TLS 或填造 Relay。
- Mac 的 SSH 别名 `ubuntu` 当前解析为 `glory@47.101.183.194:8913`。这是可尝试的初次管理入口，Windows 是否有路由和合法 SSH 凭据须重新验证；别名和私钥不会随本文迁移。
- 源码：<https://github.com/gloryhui/Veytrane.git>，本文核对基线 `6b084199088cd578d3668f16657de291ecbdf18c`，包含 DSH 大消息吞吐与丢包链路修复。Windows 至少包含此提交或已核验的后续版本，不使用早期无此修复的旧包。

本文包含路由和设备标识，不包含账户密码、私钥、管理 token、DSH 登录 token 或完整私有配置。携带本文即可开始工作；SSH 授权与各应用登录凭据需要在 Windows 本机另行提供。

## 2. 目标端口与访问方式

所有 Windows 监听均绑定 `127.0.0.1`。表中“Ubuntu 目标”由 Ubuntu 访问；其中的回环地址属于远端 Ubuntu，不是 Windows。路由器与 StreamDesk 内网地址也只由 Ubuntu 连接，Windows 无需直达其内网。

| 服务 ID | Ubuntu 目标 | 现有 Mac 端口 | Windows 首选端口 | 冲突备用端口 | Windows 入口或客户端 |
| --- | --- | ---: | ---: | ---: | --- |
| `sub2api` | `127.0.0.1:8080` | 18081 | 8080 | 18081 | `http://127.0.0.1:8080/` |
| `samba` | `127.0.0.1:445` | 1445 | 445 | 1445 | `\\127.0.0.1\实际共享名`；备用端口见第 7 节 |
| `streamdesk` | `192.168.3.192:18443` | 18443 | 18443 | 28443 | `https://localhost:18443/login` |
| `ubuntu-admin` | `127.0.0.1:8787` | 7788 | 8787 | 7788 | `http://127.0.0.1:8787/`，需要 Ubuntu 管理 token |
| `istore-web` | `192.168.3.1:80` | 18082 | 80 | 18082 | `http://127.0.0.1/cgi-bin/luci/` |
| `istore-nikki` | `192.168.3.1:9090` | 9090 | 9090 | 19090 | `http://127.0.0.1:9090/ui/` |
| `chromium-web` | `127.0.0.1:3001` | 13001 | 3001 | 13001 | `https://localhost:3001/`，需要原 Chromium 账号 |
| `ssh` | `127.0.0.1:61132` | 2222 | 61132 | 2222 | `ssh -p 61132 glory@127.0.0.1` |
| `nginx-share` | `127.0.0.1:3003` | 3003 | 3003 | 13003 | `http://127.0.0.1:3003/` |
| `netdata` | `127.0.0.1:19999` | 19999 | 19999 | 29999 | `http://127.0.0.1:19999/` |
| `codex-web` | `127.0.0.1:8214` | 8214 | 8214 | 18214 | `http://127.0.0.1:8214/` |
| `dsh` | `127.0.0.1:3080` | 3080 | 3080 | 13080 | `http://127.0.0.1:3080/`，首次需当前实例 token 链接 |

备用端口也必须先检查。其他端口不可用时选择空闲端口并记录，不抢占现有应用。Windows 本地 Veytrane Web 管理建议用 `127.0.0.1:7789`，避免与映射来的 Ubuntu `8787` 混淆。HTTPS 保留 `localhost` 主机名，按真实证书处理信任，不关闭系统证书验证。

## 3. 检查、获取与安装 Windows 程序

支持范围按仓库 Windows 文档：Windows 10 22H2 / Windows 11，x64。先检查 `%LOCALAPPDATA%\p2p-file\node`、`%LOCALAPPDATA%\p2p-file\bin\installed.json`、现有 Agent 进程、计划任务及已有 Veytrane 安装。已有身份必须保留，停止后备份再升级，不重新初始化。

先检查端口，必要时进一步确认占用者和系统保留范围：

```powershell
$WantedPorts = @(8080,445,18443,8787,80,9090,3001,61132,3003,19999,8214,3080,7789)
Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
    Where-Object { $_.LocalPort -in $WantedPorts } |
    Select-Object LocalAddress,LocalPort,OwningProcess
netsh interface ipv4 show excludedportrange protocol=tcp
```

配置前实际尝试绑定目标回环端口；检查 IPv6 / 通配监听也可能产生的冲突。占用进程属于旧 Veytrane 时按升级流程处理；属于其他应用时使用备用端口。不要为了 `445` 自动停止 Windows 文件共享服务，也不要为了 `80` 关闭 IIS / HTTP.sys。

优先使用可信来源、包含上述修复的 Windows x64 完整构建，核对来源、SHA256、构建信息和依赖。没有可验证制品则从源码原生构建 Agent + CLI；后台映射不依赖 GUI，不能只安装 `p2p-desktop.exe`，也不能把 `sub2api-client.exe` 当通用节点。

工具链按 [Windows 构建说明](../../packaging/windows-x64/README.md)：Git、Python 3.11+、Rust stable（至少 1.90）、Visual Studio 2022 C++ Build Tools / MSVC v143 与 Windows SDK。示例路径可按实际调整：

```powershell
# 已有仓库时先检查分支与本地改动，不覆盖用户工作。
git clone https://github.com/gloryhui/Veytrane.git C:\src\Veytrane
Set-Location C:\src\Veytrane
git merge-base --is-ancestor 6b084199088cd578d3668f16657de291ecbdf18c HEAD
if ($LASTEXITCODE -ne 0) { throw '当前源码不包含要求的修复基线' }

powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\packaging\windows-x64\doctor.ps1
if ($LASTEXITCODE -ne 0) { throw '先解决 doctor 报告的问题' }

# 导入仓库已有 MSVC 环境，不依赖 shell 事先配置。
. .\packaging\windows-x64\desktop-build-env.ps1
$VsWhere = Find-P2PVsWhere
Initialize-P2PVisualStudioEnvironment -VsWherePath $VsWhere | Out-Null
rustup target add x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Windows target 安装失败' }
cargo build --locked --release --target x86_64-pc-windows-msvc --bin p2p-agent --bin p2p-node
if ($LASTEXITCODE -ne 0) { throw '构建失败' }

$BinSource = (Resolve-Path .\target\x86_64-pc-windows-msvc\release).Path
$DataDir = Join-Path $env:LOCALAPPDATA 'p2p-file\node'
$HostTool = (Resolve-Path .\scripts\node-host.py).Path
```

使用干净源码构建；不能用 `--allow-dirty` 掩盖未知改动。安装前检查 Agent 与 CLI 的 `--build-info` 来自同一构建、目标为 `x86_64-pc-windows-msvc`。`node-host.py` 安装时会再次核验，使用不可覆盖的提交 SHA 版本目录。

仅全新节点执行以下操作；每个原生命令后检查退出码：

```powershell
& (Join-Path $BinSource 'p2p-node.exe') --data-dir $DataDir init
if ($LASTEXITCODE -ne 0) { throw '初始化失败；不要删除已有节点来重试' }
python $HostTool install --bin-dir $BinSource --data-dir $DataDir
if ($LASTEXITCODE -ne 0) { throw '安装失败' }
$Install = Get-Content (Join-Path $env:LOCALAPPDATA 'p2p-file\bin\installed.json') -Raw | ConvertFrom-Json
$Node = Join-Path $Install.bin_dir 'p2p-node.exe'
$Agent = Join-Path $Install.bin_dir 'p2p-agent.exe'
& $Node --data-dir $DataDir ownership-status
```

记录初始化返回的 Windows 完整 NodeId。`install` 不启用自启动、不运行 Agent。已有宿主安装使用 `upgrade`，但升级工具会重建仓库默认登录任务；如果已经采用第 6 节的开机任务，需要在同一维护窗口同步其路径并移除重复登录任务，始终只有一个 Agent 所有者。

## 4. Windows 配置草稿

以下代码只适用于上述全新节点，且 Agent 尚未运行。先按第 3 节检查端口，再调整 `$Port` 中有冲突的项目。`$DataDir`、`$Node` 沿用上文。旧节点应从 `p2p-node config` 的 `result.settings` 增量修改，不能用此全新模板覆盖。

```powershell
$UbuntuId = 'cbd82de8be77d5f813fc3ff92663b2ac'
$Port = [ordered]@{
    'sub2api'=8080; 'samba'=445; 'streamdesk'=18443; 'ubuntu-admin'=8787
    'istore-web'=80; 'istore-nikki'=9090; 'chromium-web'=3001; 'ssh'=61132
    'nginx-share'=3003; 'netdata'=19999; 'codex-web'=8214; 'dsh'=3080
}
# 例如确认 445 / 80 已占用且备用端口可绑定后：
# $Port['samba'] = 1445
# $Port['istore-web'] = 18082

$Record = Get-Content (Join-Path $DataDir 'node.json') -Raw | ConvertFrom-Json
if ($Record.revision -ne 0) { throw '此模板仅供 revision=0 的全新节点' }
$Draft = $Record.settings
$Draft.PSObject.Properties.Remove('remote_auth')
$Draft.signal_server = '47.101.183.194:8900'
$Draft.signal_tls = $null
$Draft.relay_server = $null
$Draft.enable_tcp = $true
# 本次只要求服务映射，不额外授予文件 / 测速权限。
$Draft.enable_files = $false
$Draft.enable_speed = $false
$Now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$Draft.trusted_devices = @(@{
    node_id=$UbuntuId; display_name='Ubuntu'; last_short_id=$null
    trusted_at=$Now; updated_at=$Now
})
$Sources = @{}; $Sources[$UbuntuId] = 'administrator'
$Draft.trust_sources = $Sources
$Draft.resources = @{ peers=@() }
$Draft.services = @()
$Ids = @{}
$Draft.forwards = @(foreach ($ServiceId in $Port.Keys) {
    $RuleId = "ubuntu-$ServiceId"
    $Ids[$RuleId] = $ServiceId
    @{
        id=$RuleId; name=$ServiceId; peer_node_id=$UbuntuId
        listen="127.0.0.1:$($Port[$ServiceId])"; target='127.0.0.1:1'
        enabled=$true; auto_start=$true
    }
})
$Draft.service_ids = $Ids
$DraftPath = Join-Path $DataDir 'windows-mapping.draft.json'
$Json = $Draft | ConvertTo-Json -Depth 30
# Rust JSON 读取不接受 BOM，兼容 Windows PowerShell 5.1。
[IO.File]::WriteAllText($DraftPath, $Json, (New-Object Text.UTF8Encoding($false)))
& $Node --data-dir $DataDir offline-apply $DraftPath --revision $Record.revision
if ($LASTEXITCODE -ne 0) { throw '离线配置未提交成功' }
```

`target='127.0.0.1:1'` 是现有通用节点采用的占位写法。真正远端目标由 Ubuntu 服务发布配置决定，必须通过 `service_ids` 关联；不能改成 Windows 自己去连接 Ubuntu 内网地址。草稿是 `settings` 对象本身，不是带 `schema_version/revision/settings` 的整份持久文件。

这里管理员显式预置 Ubuntu 的信任；Ubuntu 端也必须独立信任 Windows。这样自动重连使用持久设备信任，不依赖把密码保存到脚本。不得把管理员信任标成 `password_verified`，也无需复制 / 重设 Ubuntu 已有密码验证器。

## 5. Ubuntu 追加 Windows 授权：必须完成

Windows 只有监听而 Ubuntu 未授权时，无法访问这些服务。先用合法凭据验证初次管理通道，例如：

```powershell
ssh -p 8913 glory@47.101.183.194
```

验证远端 SSH 主机指纹。此通道不可用时，先完成 Windows 安装、配置与本地启动，再输出 Windows NodeId 和待追加的 12 个服务 ID，请用户通过现有 Mac 管理通道完成授权；没有凭据时不能宣称部署完成，不能尝试复制 Mac 私钥。映射后的 `61132` 不能作为尚未获授权时的初次管理通道。

在 Ubuntu 以既有 Agent 所属用户 `glory` 进行以下增量操作，管理 CLI 路径先从运行服务定义核实：

1. 用管理 CLI 的 `config` 读取最新 `result.revision` 和 `result.settings`，保存私有备份与修改前快照。不要假定部署时仍为 revision `15`，也不要把完整配置输出到聊天或仓库。
2. 在 `trusted_devices` 中按 Windows 完整 NodeId 去重追加，名称如“家用 Windows”；`last_short_id=null`，时间为当前 Unix 秒数。`trust_sources[WINDOWS_NODE_ID]='administrator'`。存在旧记录时保留真实来源，不无故改写。
3. 在下列每个 `services[].allowed_peers` 中去重追加 Windows NodeId：`sub2api`、`samba`、`streamdesk`、`ubuntu-admin`、`istore-web`、`istore-nikki`、`chromium-web`、`ssh`、`nginx-share`、`netdata`、`codex-web`、`dsh`。
4. 保留全部原有 `allowed_peers`（特别是 Mac）、服务 ID、目标地址、启用状态、资源策略、身份和其他设置。无需增加文件 / 测速授权。任何服务缺失或目标已变化时，先报告并按实际配置修正本次交接内容，不重建旧服务。
5. 用导出的完整 `result.settings` 作为草稿，只改上述授权，保持不含 `remote_auth`。通过 `p2p-node --data-dir <实际数据目录> apply <私有草稿.json> --revision <刚读取的revision>` 在线提交。正常授权变更不需要 `--restart`，不重启 Ubuntu Agent 或应用；revision 冲突时重新读取、重新合并，不能强行覆盖。
6. 再读回配置，确认新增授权和原 Mac 授权均存在。Windows 启动后执行 `connect` / `services`，实际检查全部 12 项服务目录。

该授权允许 Windows 访问 Ubuntu 管理页，但不替代管理 token；也不替代 SSH、Samba、Chromium、DSH 和各应用自身登录。

## 6. 后台运行与真正的开机自动启动

### 首选：开机触发、未登录也运行的计划任务

当前 `p2p-agent.exe` 是后台普通程序，未实现 Windows SCM 服务入口，不要直接 `sc.exe create ... p2p-agent.exe`。本方案由 Windows 任务计划程序托管，达到“开机即启动后台节点”的目标，不能在报告中冒称原生 SCM 服务。

仓库 `node-host.py enable` 当前只创建 `AtLogOn + Interactive + Limited` 任务，**不满足登录前启动**。主方案在 `node-host.py install` 后自行创建一个专用开机任务，暂不执行默认 `enable`。

由 Windows Codex 在本机生成任务定义并准备好一切；需要系统凭据时，让用户在任务计划程序的 Windows 系统对话框输入，不要求用户在聊天中提供密码：

| 项目 | 设置 |
| --- | --- |
| 名称 | `Veytrane-Agent-Boot`；已存在时先核对所有者与用途，不盲目覆盖 |
| 账户 | 初始化该节点的同一 Windows 用户 / SID，不能换成 SYSTEM 或另一管理员 |
| 常规 | “不管用户是否登录都要运行”，使用账户密码登录类型；通常不勾选最高权限 |
| 触发器 | 系统启动时（`AtStartup` / `BootTrigger`），延迟 30 秒 |
| 操作程序 | `$Install.bin_dir` 下 `p2p-agent.exe` 的实际绝对路径 |
| 参数 | `--data-dir "<DataDir绝对路径>"`，不带 `--initialize` |
| 起始目录 | `$Install.bin_dir` 的实际绝对路径 |
| 条件 | 允许电池供电时运行；不要因切换电池而停止；不要求空闲 |
| 设置 | 不设运行时间上限；错过启动后尽快运行；失败后每分钟重试，最多 10 次；已有实例时不启动新实例 |

管理员权限只用于注册开机任务等必要操作，运行账户仍是原节点账户。不要勾选“不要存储密码 / 仅访问本地计算机资源”的受限选项作为未经验证的网络节点替代方案。Windows Hello PIN 不等于任务所需的账户密码；无可用账户密码或策略阻止时报告实际限制，先完成其余工作，不偷偷改为 SYSTEM。

保存前确保手动测试 Agent 已优雅停止，锁已释放；创建后手动运行任务，检查 `p2p-node status`。如果用户暂时无法完成密码步骤，可以先按下节启用登录启动，但报告必须写“登录后自动启动，登录前启动待完成”。该开机任务方案是根据 Windows 官方任务机制和本仓库单用户 IPC / 身份实现制定，尚需 Windows 实机验收。

开机触发和账户凭据要求见 [Microsoft：schtasks create](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-create) 与 [BootTrigger](https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-boottrigger-triggergroup-element)。更改 Windows 账户密码后须更新任务凭据并重新验证。

### 替代：仓库自带的当前用户登录启动

只有接受“登录后启动”时采用：

```powershell
python $HostTool enable
if ($LASTEXITCODE -ne 0) { throw '注册登录任务失败' }
python $HostTool start
if ($LASTEXITCODE -ne 0) { throw '任务启动失败' }
python $HostTool status
```

不同时保留两个启用的 Agent 启动任务。切换方式前用 `$Node --data-dir $DataDir stop` 优雅停机，再用 `ownership-status` 证明释放，不通过删除锁文件、强杀进程或任务计划程序“结束任务”来处理正常停机。启用开机主方案后，管理使用对应任务名称和 `p2p-node`；`node-host.py start/disable` 等命令指向其默认登录任务，不会管理自建开机任务。

### 可选本地管理窗口

Agent 稳定运行后，按需运行：

```powershell
& $Node --data-dir $DataDir web --listen 127.0.0.1:7789
```

访问 `http://127.0.0.1:7789/`，从 Windows 数据目录 `admin.token` 获取本机管理 token。关闭此 Web 进程或终端不应关闭 Agent；Web 不属于 12 条远端映射的必需条件。无需为 Web 管理额外开公网监听。

## 7. Windows 的 Samba 特殊处理

优先尝试 `127.0.0.1:445`，但 Windows 系统文件共享可能已占用，不能自动关闭该服务来迁就映射。改用 `1445` 后，不能让用户在资源管理器输入 `\\127.0.0.1:1445\share` 并声称可用。

Windows 11 24H2 起支持 SMB 客户端指定替代 TCP 端口，需核对系统版本、管理员权限、策略和实际 cmdlet 参数。官方说明见 [Microsoft：SMB 替代端口](https://learn.microsoft.com/en-us/windows-server/storage/file-server/smb-ports)。先获取真实共享名并确认盘符空闲，然后可在支持的系统按实际值执行：

```powershell
# 检查是否支持 TcpPort，不能只凭 PowerShell 版本判断。
(Get-Command New-SmbMapping).Parameters.ContainsKey('TcpPort')
# 将实际共享名替换正确后执行；登录凭据通过系统方式另行提供。
New-SmbMapping -LocalPath 'Z:' -RemotePath '\\127.0.0.1\实际共享名' -TcpPort 1445
```

若在提升权限窗口操作，另行验证普通用户资源管理器能否访问，不能仅凭管理员会话中的盘符判成功。Windows 10 / 不支持替代端口的系统，在 `445` 不可用时应明确保留此限制，可使用支持自定义 SMB 端口的客户端，或用已映射 SSH 的 SFTP / Nginx 文件共享完成相应文件访问。不能以“1445 已监听”替代原生 SMB 使用验收，不自动降低 SMB 安全设置。

## 8. 连接、验收与本机入口文档

1. **节点**：运行 `& $Node --data-dir $DataDir status`，核对 Windows 独立 NodeId、正确构建、运行用户和数据目录。再运行：

   ```powershell
   & $Node --data-dir $DataDir connect cbd82de8be77d5f813fc3ff92663b2ac
   & $Node --data-dir $DataDir services cbd82de8be77d5f813fc3ff92663b2ac
   ```

   连接是异步动作，命令返回接纳不等于已连接；等待实际状态，并核对目录中完整 12 项。显式手动 `disconnect` 会阻止当前宿主自动重连，不能用它测试普通断网恢复后又忘记执行 `connect`。
2. **端口**：逐项核对实际 `forwards.listen`、监听所属 PID 和对应 `service_ids`，确认均为回环。HTTP/HTTPS、SSH、SMB 分别按自身协议验证；单纯 TCP 成功仅记“TCP 可达”。应用 401/403、登录页和证书错误要分别记录，不混称全业务通过。
3. **DSH**：首次登录链接必须取 Ubuntu 当前运行实例的 token，再换成 Windows 实际本地端口，私下使用。不能将旧 token、带 token 的链接或真实会话内容写进仓库导航 / 报告。验证真实会话大消息与 WebSocket 至少持续 5 分钟，同时保持一条 SSH 会话，记录断开或重连。
4. **应用离线**：配置启用不代表远端应用运行。StreamDesk 等入口失败时先区分 Ubuntu 目标不可达、授权、P2P、认证和本地占用；本任务不授权恢复已停用的旧应用或更改其部署。
5. **驻留与恢复**：关闭安装终端和所有管理界面，确认 Agent 仍运行、映射可用；按用户可接受的方式验证网络恢复。经用户同意实际重启后，核对任务运行时间早于登录时间、Windows NodeId 不变、自动连接和映射恢复。未做重启或登录前验证的项目明确标记待验。
6. **导航与交付**：必须按下节从随附 HTML 模板生成 Windows 正式服务导航，创建桌面快捷方式并打开验收，同时生成验收报告。

### 8.1 必须生成的 Windows 服务汇总 HTML

模板文件：[`windows-service-navigation.template.html`](windows-service-navigation.template.html)。使用原有导航的字体、配色、卡片、图标、响应式布局和内联 CSS / JavaScript；不要另起样式。无需运行 Mac 的 Nginx 或照搬 Mac 专用同步脚本。

交付位置为 `%LOCALAPPDATA%\p2p-file\navigation\service-navigation.html`，并在当前用户桌面创建指向它的“Veytrane 服务导航”快捷方式。可在 Edge / Chrome 中直接打开，关闭后台 Agent 之外的安装终端后仍能打开页面。HTML 无外部 CDN、无后台同步、无网页定时刷新。

生成正式页面时，由 Windows Codex 读取最终 Windows 配置的 `forwards`、`service_ids`，并通过授权管理通道核对 Ubuntu 服务的 `id/target/enabled`。以实际端口、目标和保存状态替换模板 `<script id="navigation-data" type="application/json">` 数据；核对后将 `isTemplate` 设为 `false`，将 `updatedAt` 改为实际同步时间（带时区）。端口与映射规则 ID 均使用实值，不得保留模板中的首选端口假装已经部署。无法核对某项时保留模板 / 待核对说明，不标为完成。替换 JSON 时正确转义 `<`、`>`、`&`，避免内容破坏 script 标签。

- 顶部同排左侧“远程 Ubuntu · 节点管理”，右侧“本机 Windows · 节点管理”。前者按实际映射端口打开，后者建议 `7789`，注明需要本机 Web 网关运行；本机管理项 ID 使用 `windows-admin`。
- 其余按“路由与网络、应用服务、文件共享、其他”分组，保留名称 / 服务 ID / 端口搜索、分类筛选、地址复制和已知 Web 入口打开按钮。
- 每张远端服务卡片醒目显示“远端 Ubuntu 目标（橙色）→ P2P → Windows 本机监听（蓝色）”。回环目标同样标为远端 Ubuntu；本机 Windows 管理注明直接访问。
- HTTP / HTTPS 的主机、路径、协议按第 2 节和实际核验结果生成，HTTPS 使用 `localhost`。Samba 使用实际共享名和第 7 节适用的 Windows 连接方法；若使用 `1445`，页面提供适用的命令 / 说明，不能生成带端口的伪 UNC 路径。SSH 按实际映射端口提供 PowerShell 可用的命令。
- 保留模板的 Windows token 获取命令提示，以最终 SSH 端口和实际数据目录更新；不要残留 `pbcopy`、Finder、`⌘K/⌘V` 等 Mac 操作说明。命令只有经核实的用户名、路径和端口，不包含 token 本身。
- “已启用 / 自动启动”只表示保存配置，不标为实时在线。静态页面注明同步时间，按需手动重新生成；另外保留可手动执行的 Windows 更新脚本或准确更新步骤，不建立后台更新任务。
- 分发模板与默认生成页面不含密码、私钥、Chromium 登录信息或 DSH token。日后用户另行明确要求展示私密登录信息时，仅放在受用户 ACL 保护的本机私有副本，不写入仓库或分发包。

验收时在 Windows 浏览器实际检查：13 个入口（12 个远端映射 + 1 个本机管理）、两张管理卡顺序、完整端口链路、分类搜索、复制按钮、最终 URL、窄窗口布局，以及不存在 Mac 专用操作提示。应用凭据不足时如实记录，不能据此省略导航生成。

交付报告应包含：源码提交 / 制品哈希、运行账户、安装目录、数据目录、Windows NodeId、实际 12 条映射与备用原因、Ubuntu 新 revision 与授权读回结果、启动任务类型和触发条件、各服务验收状态、尚需用户提供的认证 / 未完成事项，以及关闭自启动和回退步骤。导航和报告不含任何密码或 token。

## 9. 停用与回退

- 开机主方案：先在本机禁用专用 `Veytrane-Agent-Boot` 任务，再通过 `p2p-node ... stop` 优雅停机，确认 `ownership-status` 成功。删除任务不会删除身份和数据；需重新运行时启用并启动该任务。
- 仓库登录方案：`python node-host.py stop`、`disable`；只有明确需要卸载宿主时执行 `uninstall`。数据目录和版本目录按仓库行为保留。
- 升级失败保留原制品与一致性备份，停止新 Agent、证明锁释放后恢复旧启动路径；不要用旧 `node.json` 覆盖部署后的新授权或任务，也不要重新生成身份。
- 取消 Ubuntu 授权时，只移除本次 Windows NodeId 对应的服务 ACL 与信任，不删除原 Mac 和其他节点。对有活动会话的撤权应按项目现有强撤销语义执行。

## 10. 本文核验依据与边界

- 两端真实配置的路由字段与服务 ACL（2026-10-23 只读核对）。
- [节点安装与恢复](../../packaging/node/README.md)、[Windows x64 构建](../../packaging/windows-x64/README.md)。
- [`scripts/node-host.py`](../../scripts/node-host.py)：Windows 登录任务、同一构建安装与所有者释放检查。
- [`src/node/adapters/cli.rs`](../../src/node/adapters/cli.rs)、[`src/node/core/settings.rs`](../../src/node/core/settings.rs)：CLI、配置草稿与 revision 校验。
- [`src/node/local/paths.rs`](../../src/node/local/paths.rs)、[`src/storage/permissions.rs`](../../src/storage/permissions.rs)：Windows IPC 按用户 SID 和数据路径隔离。
- [`scripts/update-service-navigation.py`](../../scripts/update-service-navigation.py)：现有各入口协议和路径。该脚本是 Mac 导航同步器，不在 Windows 上照搬执行。
- [DSH 吞吐修复验收](../task-reports/DSH_MAPPING_THROUGHPUT_DEPLOYMENT.md)。

本文为 Windows 部署执行交接，尚未在家用 Windows 上安装或实机验收。此次未修改 Mac / Ubuntu 服务，因此无需运行 Mac 导航同步命令；以后若在 Mac / Ubuntu 真正改动映射，仍须按项目要求读取两端配置并运行 `python3 scripts/update-service-navigation.py --ssh-host ubuntu`。
