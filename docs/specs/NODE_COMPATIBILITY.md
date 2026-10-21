# 当前协议实现边界

2026-10-21：只维护当前版本，不保留历史桌面协议适配。实现与验证见 [节点协议统一记录](../task-reports/NODE_PROTOCOL_UNIFICATION.md)。

| 路径 | 当前实现 | 约束 |
| --- | --- | --- |
| Agent / 桌面附着入口 / Sub2API | 唯一 P2PN v2 协商及 File / Service / Speed 分发 | 文件与测速逐 peer 授权；TCP 只按服务 ID 和当前 ACL 开流。 |
| 独立 CLI | src/main.rs 的现有传输、隧道和服务命令使用独立线格式 | 不与 Agent 互通，身份锁仍共用。 |

LegacyDesktopV1、PeerWireMode、逐 peer 模式配置、P2PD 协商和裸目标地址隧道已移除。P2PN v1 和非节点旧帧在协商时拒绝，没有协议试错或认证失败降级。

文件/测速载荷改为 P2PF 帧，删除旧握手与隧道消息变体；当前任务库、文件回执和恢复语义不变。配置删除 peer_modes，保留未知字段拒绝：两端同步更新程序前需整理该字段，不能直接将旧配置交给新程序。2026-10-21 已完成 Mac / Ubuntu 主节点更新与服务重启，实际范围见[部署验收记录](../task-reports/P2PN_V2_DEPLOYMENT.md)；独立专用网关与朋友客户端未升级。

旧桌面数据导入和 schema 1–10 自动升级已删除。TrustSource::LegacyImport 仅描述既有授权记录的来源，并非协议执行适配；不会将其重写为密码证明。远程密码认证 P2PA 及安全域常量仍由当前协议使用，保留其安全语义。
