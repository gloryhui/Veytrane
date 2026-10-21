# 节点协议统一与旧桌面适配清理

日期：2026-10-21。范围：当前 Agent、桌面附着入口、Sub2API 与核心业务测试；不部署或重启运行中的 Mac / Ubuntu 节点。

## 实现

- 删除 LegacyDesktopV1 / PeerWireMode、wire_mode / peer_modes 及兼容构造器。所有 NodeSessionConfig 均使用当前协议。
- 删除 P2PD 握手、旧 Hello/Ready 与 TunnelOpen/TunnelReady/TunnelError 消息，删除 core/tunnel.rs。唯一握手位于 wire.rs，唯一业务入站接纳者为 Dispatcher。
- File / Service / Speed 始终带类型前缀。文件与测速使用 P2PF 载荷；TCP 始终请求 service ID，由远端按当前 ACL 解析目标，再复用通用 TCP 字节搬运。
- 删除缺少可信设备能力时的一次性密码认证分支。持续授权通道是必需能力；设备信任强撤销关闭旧连接、失效所有当前授权，重新显式认证后才允许继续。
- 删除连接模式集合与已失去读取方的资源绑定缓存。资源检查直接读取当前 FileInlets / Service 策略。
- 核心、文件、测速、隧道和真实进程夹具使用当前协议与显式资源授权。旧隧道测试移到 services/tunnel_tests.rs，保留数据回环、目标/设备拒绝和延迟开流期间撤销的检查。
- 双向文件撤销回归验证两端任务中断、不发布未完成文件；新密码认证后只恢复仍获授权的方向。未通过新增 skip / ignore 保留旧测试。

## 版本与数据边界

删除消息变体会改变 postcard 编码，因此 NodeFrame.version 升到 2。P2PN\x01 是封装标记，不能将末尾 01 当作当前业务版本。旧 P2PD 与 P2PN v1 在协商阶段明确拒绝；P2PF 不接受旧文件 magic。

NodeSettings 删除 peer_modes，未知字段仍拒绝；每条转发必须有 service_ids。两端升级前需停机备份并移除该旧字段、核对映射。此变更不自动迁移已安装配置，不生成新身份，不改密码验证器、任务库或持久回执。

独立 CLI 的现有线格式与 TCP 搬运仍有真实命令调用，保留；它不是 Agent 的降级入口。TrustSource::LegacyImport 仅是记录来源，P2PA 与认证安全域常量仍参与当前认证，不属于被删除的旧协议分支。

## 文档

可点击架构图保留 22 个模块与 4 个视图，更新会话、文件帧、服务开流和源码链接；同步协议、授权、配置恢复、发布边界、四设备示例及安装说明。历史清理报告加上后续状态链接，历史验收证据不改写。

## 验证

- `scripts/node-core-e2e.py --output /tmp/p2p-protocol-v2-e2e`：283 项通过、零失败、零忽略；13 个场景、21 条进程证明完整。
- `cargo clippy --locked --all-features --all-targets -- -D warnings`：通过。依赖包仍有 Cargo 自身的 future-incompat 提示，不是项目 Clippy 错误。
- `cargo check --locked --no-default-features --all-targets`：通过。
- `scripts/package-desktop-tests.py` 8 项、`scripts/node-host-tests.py` 5 项：通过。
- 架构依赖守卫、22 模块 / 4 视图数据同步与源码路径检查：通过；浏览器点选核心视图与会话详情验证成功。
- `cargo test --locked --all-features --all-targets --no-fail-fast -- --test-threads=4`：675 项库测试及 8 项集成测试通过，零失败。原有 1 项 `deployed_gateway_accepts_an_independent_identity_and_only_sub2api` 需显式 `SUB2API_LIVE_PROFILE` 与隔离部署，保持原有忽略；没有新增跳过。
- Service 流补充断言回归：2 项通过，验证拒绝响应请求号和 UnknownService 错误码，以及远端目标没有接入。
- `cargo fmt --all -- --check`、`git diff --check`：通过。

证据保存在本机 `/tmp/p2p-protocol-v2-*` 日志及矩阵目录，不作为仓库运行数据提交。本轮验证为 macOS Apple Silicon 的本地单元、真实环回网络与进程测试；不等同于更新已部署节点、Windows/Linux 实机或公网 NAT 组合验收。
