# 直播专用版 WebRTC/TCP 验证记录

日期：2026-10-23（Asia/Shanghai）。分支：`codex/live-distribution`，基于 `6b084199088cd578d3668f16657de291ecbdf18c`。

后续已完成的 Windows 原生构建、真实 OBS 与朋友包同机验收见 [Windows OBS 验证记录](LIVE_WINDOWS_OBS_VALIDATION.md)。本文保留先前 Mac / FFmpeg 实测及当时待办的历史状态；跨网络验收仍未完成。

## 实现范围

- 新增 `live-gateway` 和 `live-viewer`，由 `live` Cargo feature 启用。主播与观看端使用独立于通用 Agent / Sub2API 的身份目录。
- 官方 MediaMTX 1.21.2 原样运行，接受本机 OBS WHIP；关闭录制、回放、API、RTSP、RTMP、HLS、SRT、MoQ。
- 复用既有 ForwardManager / 节点服务 ID / QUIC 通道，仅导出观看 HTTP 与 ICE/TCP 两个服务。WHEP 代理将 TCP 媒体候选重写到观看者回环端口，过滤 UDP 和其他地址。
- 观看端无交互登录，底层使用自动生成并分发的专用节点凭证。浏览器默认静音，连接就绪自动打开，开播和断流时自动重试。
- 附 Windows 原生构建、首次初始化、主播启动与观看包脚本，以及 Mac 主播启动脚本。未修改 MediaMTX 源码。

## 环境与方法

- macOS / Apple Silicon，Rust 1.98.1，官方 MediaMTX 1.21.2，Chrome headless，FFmpeg WHIP。
- 官方 macOS 压缩包 SHA256：`7d0d0c5c11527cbd10bde1c3d728864eb0334a9b653566cd73c0259750cbf093`，已与同一官方 Release 的 `checksums.sha256` 核对。
- 源视频：640×360、30 fps、H.264 baseline、无 B 帧、约 2.5 Mbps CBR；音频为 Opus 96 kbps。移动噪声提供实际编码负载，画面顶部携带毫秒时间戳及校验码。
- 路径：源视频 → FFmpeg WHIP → 官方 MediaMTX → 主播端节点 → 真实 QUIC 直连 → 观看端 TCP 监听 → Chrome WebRTC 解码与播放。
- 两端节点、源视频与浏览器同在一台 Mac，使用同一系统时钟；未注入网络延迟或丢包。浏览器逐帧读取已显示画面中的时间戳，计算源帧生成到浏览器呈现回调的延迟。结果涵盖编码、路由、隧道与播放，但不包含物理摄像头采集或显示器扫描延迟。
- 通过 Chrome `getStats()` 确认选中的远端 ICE 候选是 `tcp / 127.0.0.1:18189 / passive`；MediaMTX 日志同时确认读取端为 TCP、推流端为本机 UDP；节点日志确认 P2P 直连。因此没有绕过隧道从本机 UDP 直接播放。

## 两轮实测

| 指标 | 首轮 | 第二轮（增加停播恢复验收） |
| --- | --- | --- |
| 有效逐帧样本 | 591 | 598 |
| 时间戳校验拒绝 | 0 | 0 |
| 最低延迟 | 139 ms | 13 ms |
| 中位延迟 | 213 ms | 85 ms |
| P95 延迟 | 294 ms | 104 ms |
| 最高延迟 | 300 ms | 107 ms |
| 浏览器报告视频丢帧 | 0 | 0 |
| 播放页 JavaScript 异常 | 0 | 0 |

两轮均测量 20 秒稳态窗口，之前等待 3 秒预热；数字存在调度、编码和播放缓冲差异，不挑选较低的一轮作为网络性能保证。第二轮在测量后关闭发布者、等待 3 秒再启动，原观看页面约 **8212 ms** 恢复到新的 WebRTC 会话并出现画面，没有刷新页面。该时间包含人为停播的 3 秒。

被忽略的本机原始结果：`release/live/smoke-01/latency.json`、`release/live/smoke-02/latency.json`。这些目录含隔离测试身份，不进入仓库或朋友分发包；本记录仅保留无凭证的汇总。

## 已通过检查

- `cargo test --locked --offline --features live --lib node::products -- --nocapture`：12 通过，1 个既有 Sub2API 部署测试按要求忽略。
- 覆盖直播专用服务范围、拒绝远端 WHIP / 管理接口、SDP 过滤与端口重写、初始化不覆盖身份，并回归既有 Sub2API 双身份 / 错误密码隔离测试。
- `cargo build --locked --offline --features live --bin live-gateway --bin live-viewer --bin p2p_file`。
- `cargo clippy --locked --offline --features live --lib --bin live-gateway --bin live-viewer -- -D warnings`。
- `cargo check --locked --offline --no-default-features --lib`：未启用直播功能的库构建通过。
- Python / JavaScript / shell 语法检查、`git diff --check`。
- 两轮真实媒体测试；第二轮另经过 P2P HTTP 入口验证远端 WHIP 和管理请求均返回 404。
- 测试结束已停止本轮启动的 MediaMTX、信令、网关、观看端、FFmpeg、测试源与 headless Chrome；未改既有 Mac / Ubuntu 服务或持久化映射。

## 待 Windows 与实际朋友网络验收

以下尚未由本轮执行，不能用 Mac 编译或本机媒体结果代替：

1. Windows 原生 MSVC 构建，运行 PowerShell 构建 / 启动 / 打包脚本，检查路径含空格、控制台编码与退出时 MediaMTX 清理。
2. Windows OBS 真正通过 WHIP 推送 H.264 / Opus；确认所用编码器无 B 帧、画面和音频正常。本轮可重复推流源为 FFmpeg，并未操作 OBS 录制或推流配置。
3. 另一台 Windows 解压朋友包，无管理员权限启动，确认默认浏览器自动打开、开启声音、停播重开、休眠与网络切换后的恢复。
4. 两台不同网络的机器验证直连和必要时的中继；记录各自网络环境、码率与长时间播放表现。
5. 实际 OBS 端到端延迟：在 OBS 画面中加入毫秒计时器，将原始计时器与朋友端播放画面放到同一镜头拍摄并比较时间差；跨机器仅比较各自系统时钟会引入时钟偏差。分别记录稳定窗口的中位数与 P95，注明直连或中继。

操作步骤见 [直播专用版说明](../../packaging/live/README.md)。
