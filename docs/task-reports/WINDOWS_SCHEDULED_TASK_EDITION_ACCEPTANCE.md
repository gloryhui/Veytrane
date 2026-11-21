# Windows x64 计划任务双版切换验收

**状态** `PASS`（2026-11-21，Windows x64 双版换版、重新登录触发和双向文件收发）

Veytrane 默认保留 TCP 转发，原生文件传输由文件增强版提供。Windows Server 2022 CI 已通过两个版型的原生构建和归档验证。Windows 桌面实测已通过计划任务启动 Agent，并在同一份合成节点配置上完成标准版 → 文件增强版 → 标准版。

## 实测环境

- 使用已登录的专用测试账户或一次性虚拟机，系统为 Windows 10 22H2 x64 或 Windows 11 x64。使用普通交互会话，确认 `pwsh` 为 PowerShell 7 或更新版本，`python` 为 Python 3.11 或更新版本。
- 预检当前测试账户没有 `node-host.py` 为其 SID 生成的 `P2PFile-Agent-*` 任务。若已有同名任务，记录阻碍并保留任务。
- 候选、日志和脚本使用独立测试目录。验收脚本另建临时 `LOCALAPPDATA` 与节点数据。保留现有 Agent、登录任务、信任、映射、配置、身份和用户的未提交改动。

## 候选与脚本

使用构建提交 `de9af87d516efea8f0cf2dab0c235ea59815e1b2`。从 [CI #44880833831](https://github.com/gloryhui/Veytrane/actions/runs/44880833831) 下载两份 Windows 候选。下表是产物内发行 ZIP 的 SHA-256。也可使用 Mac 已校验并保存在外置盘的同批原归档。

| 版型 | GitHub Actions 产物 ID | 发行 ZIP 的 SHA-256 |
| --- | --- | --- |
| 标准版 | `14699490790` | `bfed8d06901ced7caf124451f3dcbd782bf4049d8adfa3dca9f60b41bf663170` |
| 文件增强版 | `14699490793` | `85314ff88adfc42026fee1e6626b98c3161cf027e46f9bdcce29dd94a1f8d7bf` |

在具备仓库读取权限的环境可使用 `gh run download 44880833831 --repo gloryhui/Veytrane --pattern '*x86_64-pc-windows-msvc*' --dir <测试目录>`，每份产物会进入各自目录。校验 `candidate.json` 的 `build_sha`、`target`、`edition` 与 `source_state`，并用同提交的 `scripts/verify-desktop-package.py` 验证归档。解包后，两个候选目录都应直接包含 `p2p-agent.exe` 和 `p2p-node.exe`。若产物已过期或无法下载，先取用外置盘原归档并核对哈希。

使用同一构建提交下的 `scripts/node-host-tests.py` 和同目录的 `node-host.py`。已有工作区包含改动时，可用 `git archive` 将这两个脚本及归档验证器导出到专用目录。

## 验收步骤

实际调用仓库的 [Windows 双版型隔离用例](../../packaging/node/README.md#windows-双版型隔离验收)。在两个脚本所在目录执行以下命令，将参数换成两个解包后实际包含可执行文件的目录。保留标准输出、标准错误及退出码。

```powershell
python -X utf8 .\node-host-tests.py --windows-upgrade "<标准版二进制目录>" "<文件增强版二进制目录>"
```

- [x] 通过真实 Task Scheduler 注册当前测试用户的任务并启动 Agent，通过原生 IPC 确认 TCP 能力已就绪。
- [x] 同一 SHA 依次使用 `standard`、`files`、`standard` 两种安装键。每次核对任务 Action 指向、运行中的 Agent 版型、节点身份、revision 和配置字节。
- [x] 两个不可变安装目录和回退元数据符合预期。测试卸载后再次查询，确认本次任务已注销。脚本在退出临时目录前核对节点数据和两份版本仍存在。
- [x] 现有 Agent 进程持续运行。测试操作限于本次任务、合成节点数据和临时安装，退出后无测试 Agent 进程或临时节点目录残留。

上述换版用例通过脚本触发任务启动。实际登录触发和双向文件收发另有实机证据，见[独立验收](#独立验收)。

## 回传证据

2026-11-21 在 Windows 10 专业版 22H2 x64（构建 `19045.7846`）、PowerShell `7.6.10` 和 Python `3.14.0` 上，经用户授权使用当前普通交互账户和临时节点数据。两份 `de9af87d516efea8f0cf2dab0c235ea59815e1b2` 发行 ZIP 的 SHA-256 均与上表一致。`--windows-upgrade` 用例自身退出码为 `0`，`script-result.json` 为 `PASS`，标准错误为空。

| 时间（UTC+08:00） | Task Scheduler 观测 |
| --- | --- |
| 17:16:57 | 执行前查询同一 SID 对应任务，匹配数为 `0` |
| 17:17:04 | 任务为 `Running`，Action 指向 `standard` 安装 |
| 17:17:08 | 任务为 `Running`，Action 指向 `files` 安装 |
| 17:17:13 | 任务为 `Running`，Action 指回 `standard` 安装 |
| 17:17:17–17:17:20 | 任务先变为 `Disabled`，随后注销，卸载后独立复查的匹配数为 `0` |

三个阶段均通过原生 IPC 核对运行版型和 TCP 就绪，节点身份、revision 和 `node.json` 字节一致。两个换版方向的回退元数据均通过核对。卸载保留节点数据和两个不可变安装目录，随后测试夹具清理完毕，原有 Agent 进程仍在运行。

回传包 `veytrane-windows-x64-44880833831-PASS.zip` 的 SHA-256 为 `0418eeeb3f68d1c693b34c1e70beabe4dfc6f62b5d1fd83b532ea5909b04856c`。包内 `command.txt` 记录实际命令，`exit-code.json` 记录用例退出码，`task-before.json`、`task-observations.json` 和 `task-after.json` 记录任务查验。归档验证、用例原始输出和清理证据一并回传。[施工验收记录](../../architecture/tcp-default-files-optional-plan.md)

## 独立验收

2026-11-21 在同一 Windows 10 专业版 22H2 x64（构建 `19045.7846`）上，经用户授权使用当前普通交互账户和合成节点，复用上表中 CI #44880833831 的同 SHA 双候选。

### 实际重新登录触发计划任务

**结果** `PASS`。注册前当前 SID 对应任务匹配数为 `0`。测试任务的触发器为当前 SID 的 `AtLogOn`，Principal 为 `Interactive`、`Limited`。Action 指向隔离安装的标准版 `p2p-agent.exe` 和测试数据目录。采证前未手动启动任务或测试 Agent。

| 时间（UTC+08:00） | 观测 |
| --- | --- |
| 16:37:28 | 原 Windows 会话登录，事件 `21` |
| 18:09:58 | 任务已注册且为 `Ready`，尚无运行记录，测试 Agent 进程数为 `0` |
| 18:12:14 | 原会话注销，事件 `23` |
| 18:40:39 | 同一账户登录新会话，事件 `21` |
| 18:40:40 | 任务 `LastRunTime` 晚于本次登录，采证时任务为 `Running` |
| 18:45:39 | `node-host.py uninstall` 完成后复查，同名任务和测试 Agent 进程数均为 `0` |

卸载前确认 Agent 可执行文件和数据目录与 Action 一致，进程属于新会话。`p2p-node status` 经原生 IPC 确认标准版核心运行、TCP 就绪。卸载复查确认原有 Agent 和其他计划任务仍在。

### 文件增强版双向收发

**结果** `PASS`。两个文件增强版 Agent 使用独立身份、节点和接收目录，信令关闭，两端经回环直连。每个方向均用 `p2p-node send` 发起，并用 `p2p-node tasks` 核对发送端和接收端任务。`Get-FileHash -Algorithm SHA256` 确认源文件和实际落盘文件一致。

| 方向 | 文件 | 字节 | 发送任务 | 接收任务 | 源文件与落盘文件的 SHA-256 |
| --- | --- | ---: | --- | --- | --- |
| A → B | `a-to-b.bin` | 256,123 | `completed` | `completed` | `11db6950efde4f30330c8fad1bdc5a146b919d13f91588ffade70cbf1b11ad26` |
| B → A | `b-to-a.bin` | 413,981 | `completed` | `completed` | `04fd644576ba1b33608db605d02830234781baa89cfd2b28ea9d8374122e9dac` |

接收文件均落在各自的专用接收目录。两台测试 Agent 经 IPC 停止后均以退出码 `0` 退出，所有权锁释放。

独立验收回传包 `veytrane-windows-independent-acceptance-PASS.zip` 的 SHA-256 为 `04e38ad869ef2ba4463eff54b08edf45d17a496c952dd43a7b1f519d34081eda`。包内 `login-before.json`、`login-task-definition.xml` 和 `login-result.json` 记录任务定义、登录时间线、IPC 与注销复查。`bidirectional-files.json` 记录两边的任务状态和各自的源文件与落盘文件哈希。[施工验收记录](../../architecture/tcp-default-files-optional-plan.md#独立验收)

## Windows 生产换版

**结果** `PASS`（2026-11-21 20:58:31，UTC+08:00）。Windows x64 生产 Agent 和配套 `p2p-node` 从 `c94d501adcc94043dbf02dd0bc3fcad9c4d732e8` 切换到 `de9af87d516efea8f0cf2dab0c235ea59815e1b2` 标准版。复用上表中 CI #44880833831 的标准版归档（产物 ID `14699490790`）。

| 核对项 | 实测结果 |
| --- | --- |
| 配置及退出码 | 成功重试的配置 revision 为 `15 → 16`。标准候选 `--check-config` 和 `node-host.py upgrade` 退出码均为 `0`。按 JSON 结构复核仅 `enable_files`、`revision` 和 `last_request` 变化，节点身份与任务库校验值一致，旧版安装保留。 |
| 生产计划任务 | 原任务 `\Veytrane-Agent-Boot` 启动新版后处于 `Running`，Agent PID `12348 → 17352`。仅匹配 Action 的可执行路径和原先指向旧版目录的工作目录切换到新安装。参数、主体、登录方式、触发器、设置、其他 Actions 和任务 DACL 均经核对。 |
| 原生 IPC 与转发 | 新 `p2p-node` 经 IPC v1 确认 `standard`、TCP 就绪、测速启用、文件关闭。信令在线，Mac 和 Ubuntu 两个可信对端已连接，17 条转发均监听且对端就绪。 |
| 协议探测 | Windows → Ubuntu SSH 转发返回 `SSH-2.0`。Windows → Mac RustDesk 转发及 Windows 本机 RustDesk 均返回 `Message.hash`，本机 RDP 完成 X.224 协商。换版前没有运行中的 WSL 发行版。 |
| 管理入口基线 | `http://127.0.0.1:3001/` 换版前后均返回 HTTP `400`。这项核对只确认响应与原有基线一致，管理页面功能未验收。 |

首轮在升级脚本执行前，JSON 序列化文本比较误判了配置差异，当时恢复到旧版 revision `15`。重试前用备份离线验证了键重排可以通过、无关 TCP 变更会被拒绝，随后用结构比较复核生产配置。脱敏结果保存为 `production-retry-result.json`，换版前管理入口状态记录在 `production-preflight.json`。私有备份和原始配置留存于 Windows 本机。[施工验收记录](../../architecture/tcp-default-files-optional-plan.md#windows-生产换版验收)
