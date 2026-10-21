# 当前入口与历史实现处置

状态：源码维护记录，不代表 Windows / Linux 实机或生产部署验收。当前只维护用户正在使用的版本，不为历史版本保留无调用方的兼容层。最新清理证据见[遗留清理记录](../task-reports/LEGACY_CLEANUP.md)。

| 入口 / 模块 | 当前决定 |
| --- | --- |
| p2p-desktop → Agent | 唯一桌面入口，通过 IPC 共享后台身份、任务和监听。旧内嵌桌面已删除。 |
| p2p-node / Web → Agent | 当前管理接口；保留初始化、所有权检查、离线配置和在线操作。 |
| 旧桌面数据导入 | 退役 CLI 三个迁移命令、UI 预览/导入、备份恢复适配器和旧导入规格。 |
| node 根模块别名 | 删除；调用方直接引用 core / local / api / host / adapters / products。 |
| 旧偏好及 schema 1–10 升级 | 删除；不再把历史字段自动转换成当前授权或偏好。 |
| LegacyCliV3 占位模式 | 未实现且配置校验从未允许使用，删除枚举和不可达分支。 |
| 独立 CLI 的 transport / transfer / tunnel / admin | 仍由 src/main.rs 的现有命令使用，保留。 |
| 文件协议、可选能力和 PeerWireMode 分支 | core/session、transfer、tunnel 及其真实进程回归仍有直接依赖，本次不把它们当成孤立废文件删除。 |
| 信令 / Relay | Agent、专用版和独立 CLI 共用，保留。 |
| 四设备配置示例、打包资源、历史验收记录 | 现有执行卡、构建或证据索引仍引用，保留。 |

当前 Agent 的 node.json 与任务存储格式未改动，未更新已安装程序或重启服务。默认 Agent 和专用版使用 P2PN v1；独立 CLI 的协议不能因同仓库而声称与之互通。进一步移除协议分支必须同时重整流分发、授权和恢复测试，不能直接删除共用 codec 或以跳过测试代替验证。
