# Windows x64 计划任务双版切换验收

**状态：待 Windows 实机执行**

Veytrane 默认保留 TCP 转发，原生文件传输由文件增强版提供。Windows Server 2022 CI 已通过两个版型的原生构建和归档验证。需要在 Windows 桌面环境实测计划任务能启动 Agent，并在同一份合成节点配置上完成标准版 → 文件增强版 → 标准版。

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

- [ ] 通过真实 Task Scheduler 注册当前测试用户的任务并启动 Agent，通过原生 IPC 确认 TCP 能力已就绪。
- [ ] 同一 SHA 依次使用 `standard`、`files`、`standard` 两种安装键。每次核对任务 Action 指向、运行中的 Agent 版型、节点身份、revision 和配置字节。
- [ ] 两个不可变安装目录和回退元数据符合预期。测试卸载后再次查询，确认本次任务已注销。脚本在退出临时目录前核对节点数据和两份版本仍存在。
- [ ] 现有 Agent、其他任务和用户数据保留。若脚本失败，记录失败步骤，并单独核对本次测试进程和任务的退出状态。

这次通过脚本主动触发任务启动。实际退出并重新登录 Windows 与文件收发各有独立验收范围。

## 回传证据

报告 `PASS`、`FAIL` 或 `BLOCKED`，附 Windows 版型、系统构建号、架构、PowerShell 版本、两份候选的 SHA-256、实际执行命令和退出码。提供脚本结果 JSON、任务前后查验结果及关键日志。若失败，写明首次失败步骤、清理结果和仍待验证的内容。证据中省略凭据、真实节点身份和私有路径。回传后更新[施工验收记录](../../architecture/tcp-default-files-optional-plan.md)。
