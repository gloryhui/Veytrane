# Windows x64 计划任务双版切换验收

**状态** `PASS`（2026-11-21，Windows x64 实机）

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

这次通过脚本主动触发任务启动。实际退出并重新登录 Windows 与文件收发各有独立验收范围。

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
