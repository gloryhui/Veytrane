# 当前入口与历史实现处置

状态：当前源码维护与部署边界。当前只维护用户正在使用的版本，不为历史版本保留无调用方的兼容层。最新清理证据见[节点协议统一记录](../task-reports/NODE_PROTOCOL_UNIFICATION.md)；2026-10-21 Mac / Ubuntu 主节点的实机结果见[P2PN v2 部署验收](../task-reports/P2PN_V2_DEPLOYMENT.md)，不扩展为其他平台或完整生产验收。

| 入口 / 模块 | 当前决定 |
| --- | --- |
| p2p-desktop → Agent | 唯一桌面入口，通过 IPC 共享后台身份、任务和监听。旧内嵌桌面已删除。 |
| p2p-node / Web → Agent | 当前管理接口；保留初始化、所有权检查、离线配置和在线操作。 |
| 旧桌面数据导入 | 退役 CLI 三个迁移命令、UI 预览/导入、备份恢复适配器和旧导入规格。 |
| node 根模块别名 | 删除；调用方直接引用 core / local / api / host / adapters / products。 |
| 旧偏好及 schema 1–10 升级 | 删除；不再把历史字段自动转换成当前授权或偏好。 |
| LegacyCliV3 占位模式 | 未实现且配置校验从未允许使用，删除枚举和不可达分支。 |
| 独立 CLI 的 transport / transfer / tunnel / admin | 仍由 src/main.rs 的现有命令使用，保留。 |
| 旧桌面线协议与 PeerWireMode | 删除；当前文件/测速载荷与 Service ID 开流统一走 P2PN v2，核心和进程测试同步切换。 |
| 信令 / Relay | Agent、专用版和独立 CLI 共用，保留。 |
| 四设备配置示例、打包资源、历史验收记录 | 现有执行卡、构建或证据索引仍引用，保留。 |

当前 Agent 配置删除 peer_modes 字段，转发必须具有 service_ids 映射；任务库与身份存储保持。节点协议使用 P2PN v2 / P2PF 文件载荷，两端需同步更新。Mac / Ubuntu 主节点及独立 Sub2API 网关现已更新为 Veytrane `ec72ed4`，保留身份、授权、任务和 7 条映射，对应 Windows 新包已生成；朋友 Windows 主机尚未安装验收，见[当前部署记录](../task-reports/VEYTRANE_DEPLOYMENT.md)。独立 CLI 仍有实际命令调用，不属于本次旧桌面适配清理范围。
