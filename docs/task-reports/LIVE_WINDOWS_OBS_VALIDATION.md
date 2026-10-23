# Windows 原生部署与真实 OBS 验收

日期：2026-10-23（Asia/Shanghai）。分支：`codex/live-distribution`。

## 结论与范围

已在 Windows 完成 MSVC 构建、官方 MediaMTX 校验及部署、独立直播身份初始化、真实 OBS WHIP 推流、浏览器 TCP 隧道播放、开启声音、停播重开自动恢复及朋友包解压启动验收。

以下两轮延迟测量中，主播、网关、观看程序与浏览器均在同一台 Windows 上，节点通过现有公网信令配对后建立 IPv4 QUIC 直连。**延迟数据不能用于宣称跨网络、移动网络或中继性能通过。** 后续一次真实朋友跨网络 Edge 播放已由用户确认，见本文末尾的排障补充。

## 代码、工具及已有工作保护

- 复用 `F:\git\p2p_file`；开始时在干净的 `main`，没有未提交修改。拉取并切换到 `origin/codex/live-distribution`，完整实现提交为 `b3df38a8a3e10af8c9e81fbd65c3ee6ea7958d9c`，`git merge-base --is-ancestor b3df38a HEAD` 成功。
- 阅读根目录 `AGENTS.md`、`packaging/live/README.md` 及历史 Mac 验证记录；没有覆盖其他工作树。
- Windows 10 专业版 10.0.19045；Rust 1.98.1，`x86_64-pc-windows-msvc`；Visual Studio 2022 17.14.37806.7、MSVC 14.44.35207、Windows SDK 10.0.26100.0。使用已有官方工具链，没有另装工具链。
- 已有 `p2p-agent`（PID 28868）保持运行；没有停止、重配其他 Veytrane / Sub2API 服务。已有节点 `identity.key` 的前后 SHA256 相同。本轮期间通用节点配置的整体哈希发生变化，不能声称整份 `node.json` 未变；本轮只读该配置，未向其写入内容。
- 已安装 OBS 32.2.2 支持 WHIP。复制其运行文件到被忽略的 `release/live/obs-portable/`，采用独立便携配置、场景集合及合成源。原 OBS 用户目录核对 136 个原有文件，均未改变。
- 本机实际节点配置提供信令地址，TLS 配置为空、中继配置为空；沿用该已存在且实际可连接的组合，没有猜测信令、中继或 TLS 参数。完整私有配置及凭证不进入报告。
- 首次创建 `live-private/gateway/`；直播身份与原节点独立，之后没有重新初始化。朋友端自行生成其独立身份。

## 构建与 MediaMTX

实际运行 `packaging/live/Build-Live.ps1`，原生 release 构建成功。依赖检查发现初版观看程序导入 `VCRUNTIME140.dll`；为满足朋友无需另装运行库，修改构建脚本，对所有目标依赖应用 `-C target-feature=+crt-static`，保留并恢复调用方环境变量，再执行原生构建。

运行中的初版网关二进制仅重命名保留于 `target/`，以便构建新文件；该操作没有中断直播服务或替换其身份。新的发布二进制位于 `target/x86_64-pc-windows-msvc/release/`。最终媒体检查后，只停止本轮旧网关以启用新构建；父启动脚本的 `finally` 成功清理其自己启动的 MediaMTX，再由同一启动脚本运行最终静态 CRT 网关。

MediaMTX 来自 [官方 v1.21.2 Release](https://github.com/bluenviron/mediamtx/releases/tag/v1.21.2)，下载 `mediamtx_v1.21.2_windows_amd64.zip` 及同一 Release 的 `checksums.sha256`，实际 SHA256 核对成功：

```text
a84224f5a490855684e62ba6fb71e80ea2a5e16aed61883bb58380290e93e467
```

只提取 `mediamtx.exe` 到 `packaging/live/`，`--version` 返回 `v1.21.2`；仓库 `mediamtx.yml` 与 Git 原版一致，没有覆盖成下载包配置，也没有修改 MediaMTX 源码。实际运行 `Start-Live-Host.ps1`。为保存验收日志，仅在本轮启动进程的环境中指定文件日志目标。

## 真实 OBS 推流与编码

OBS 的测试配置与场景均为 `Veytrane Live Test`。用户在真实 OBS 窗口点击开始、停止、重开，并由日志和浏览器交叉确认；没有使用 FFmpeg 发布程序代替 OBS。

| 项目 | 最终实际参数 |
| --- | --- |
| 服务及服务器 | WHIP；`http://127.0.0.1:18891/live/whip` |
| Bearer Token | 空 |
| 画面 | 1280×720，30 fps |
| 视频 | OBS x264，H.264 baseline，CBR 3000 kbps，veryfast，zerolatency |
| B 帧及关键帧 | `bframes=0`，关键帧间隔 1 秒；`scenecut=0` |
| 音频 | OBS 内部 libopus 编码器，48 kHz 双声道，96 kbps |
| 测试源 | 浏览器源中的合成运动画面、十进制毫秒计时器和带校验的像素时间码；独立 440 Hz 合成 WAV 循环源 |

初轮生成的私有配置错误地使用冒号拼接 x264 参数，OBS 日志提示解析失败。初轮已使用 baseline/zerolatency；保留其测量记录，但不将错误参数视为成功应用。关闭这个便携 OBS 后，将私有配置修正为 **`bframes=0 scenecut=0`**，重新启动 OBS 并完整复测。最终日志分别显示 `bframes = 0`、`scenecut = 0`，没有该解析警告。

没有桌面、麦克风或私人窗口采集源；没有开始录制、启用回放或转码。WAV 仅是合成测试输入，不是录音。

## 观看与实际媒体路径

- 原生 `live-viewer` 完成节点凭证验证后自动打开默认 Chrome 的 `http://127.0.0.1:18889/`。实际页面显示 1280×720、约 30 fps 和约 3 Mbps。
- 另用有窗口的 Chrome 执行逐帧验收，没有添加绕过自动播放限制的参数。首次载入 `muted=true`、`paused=false`；实际点击“开启声音”后 `muted=false`、`paused=false`，按钮变为“关闭声音”。默认浏览器中的声音按钮也实际点击验证。
- WebRTC `getStats()` 在两轮均选择远端候选 `tcp / 127.0.0.1:18189 / passive`；视频 codec 为 `video/H264`，音频为 `audio/opus`。最终轮视频统计解码 997 帧、丢帧 0；两轮页面 JavaScript 异常均为 0。
- 解码后的音频流分析主峰 439.45 Hz，最终 RMS 约 0.05390，确认测试音频实际到达浏览器。该检查验证接收及解码，没有以软件分析冒充人耳对物理扬声器的检查。
- MediaMTX 日志明确区分 OBS 发布端的 `host/udp/127.0.0.1/18190` 与观看端的 `host/tcp/127.0.0.1/18190`；网关和观看程序日志均显示 IPv4 直连与凭证认证完成。
- 路径为：OBS → 本机 MediaMTX → 网关 → Veytrane QUIC → 观看端 TCP/18189 → Chrome WebRTC。浏览器没有选择主播端 UDP 直连。OBS 到本机 MediaMTX 使用 UDP 是预期的本机输入路径。
- 经朋友侧 18889 HTTP 入口检查：`GET /healthz` 为 200，`POST /live/whip`、`GET /v3/paths/list`、`POST /other/whep` 均为 404。

## 实测端到端延迟

OBS 浏览器源每次绘图将同机 `Date.now()` 的低 32 位编码成黑白像素时间码，附校验和，并显示完整十进制毫秒值。观看页在 `requestVideoFrameCallback` 中从已解码画面读取该时间码，与同机当前毫秒时间比较，模 2³² 处理回绕，拒绝校验失败及重复时间戳。每轮先预热约 3 秒，再测量 30 秒。

这测量合成源绘制、OBS 取帧与编码、WHIP、MediaMTX、节点隧道、浏览器解码及呈现回调的合计延迟。没有使用 ping、RTT 或 jitter 充当端到端延迟；也不包含物理摄像头、显示器扫描或扬声器延迟。30 fps 本身具有约 33.3 ms 的帧间隔，毫秒显示不代表物理测量精度达到 1 ms。

| 同机 OBS 实测 | 首轮 | 修正参数后的最终轮 |
| --- | ---: | ---: |
| 有效逐帧样本 | 899 | 899 |
| 校验拒绝 | 0 | 0 |
| 最低 | 142 ms | 174 ms |
| 中位数 | 206 ms | 223 ms |
| P95 | 220 ms | 238 ms |
| 最高 | 222 ms | 241 ms |
| 浏览器视频丢帧 | 0 | 0 |

按排序后的第 `floor(N×q)` 个零基样本取分位数。保留两轮结果，不挑选较低一轮作为朋友实际网络的性能保证。

## 停播重开与退出

用户于 22:35:33 停止 OBS，MediaMTX 关闭发布及读取会话，原网页显示“等待主播开播，正在重试…”。用户于 22:36:01 重开，原页面自动建立新会话并恢复画面与开启的声音；验收浏览器顶层导航计数始终为 1，没有刷新。

测试脚本的整段恢复观察耗时 71609 ms，包括提示后等待用户操作、停播时间及恢复后额外 3 秒稳定观察；**不能将其当成程序纯重连耗时**。本轮确认自动恢复功能，没有给出严格的恢复时延指标。最终恢复候选仍为 TCP/18189。

朋友包在非管理员会话、路径含空格的 `release/live/Friend Package Acceptance/` 内实际解压启动，自动生成 `%LOCALAPPDATA%/Veytrane-Live-Viewer/identity.key`，完成免登录观看。Ctrl+C 后 18889 与 18189 均释放；批处理终止提示后完成退出。随后再次启动成功。

## 端口及其他服务

按本轮进程 PID 核对，TCP 服务仅监听以下回环地址：

| 进程 | TCP 监听 |
| --- | --- |
| MediaMTX | `127.0.0.1:18891`、`127.0.0.1:18190` |
| live-gateway | `127.0.0.1:18890` |
| Live-Viewer | `127.0.0.1:18889`、`127.0.0.1:18189` |

MediaMTX 的固定 UDP/18190 也仅监听 127.0.0.1。另有 Veytrane QUIC 与 OBS WebRTC 所需的动态 UDP socket，部分绑定 `0.0.0.0` / `::`；因此只能确认上述**服务 TCP 入口为回环地址**，不能声称所有网络 socket 都只绑定回环。没有因此关闭防火墙或修改系统安全设置，没有意外开启 MediaMTX 管理 API、RTSP、RTMP、HLS、SRT、MoQ 或回放服务。

系统还有已有代理、浏览器、WSL 等进程的监听；它们不属于本轮创建的服务，未停止或重配。没有修改原 Windows / Ubuntu 持久化端口映射。

遵照工作区要求尝试执行 `python scripts/update-service-navigation.py --ssh-host ubuntu`（本机 `python3` 是商店占位入口，使用实际 Python 3.14）。该脚本默认读取 Mac 的 `~/Library/Application Support/p2p-file/node/node.json`，Windows 不存在此文件，读取阶段即失败，导航未被改写。没有把 Windows 直播链路误写成远端 Ubuntu / Mac 的导航映射。

## 验证与证据

- 原生 `Build-Live.ps1` release 构建；静态 CRT 构建及最终二进制依赖检查、包启动验证见下节。
- `cargo test --locked --features live --lib node::products`：12 通过、0 失败，1 个既有部署测试忽略。
- `cargo clippy --locked --features live --lib --bin live-gateway --bin live-viewer -- -D warnings` 通过。
- PowerShell AST 语法检查及 `git diff --check` 通过。
- 原始证据保存在被忽略的 `release/live/`：`mediamtx.log`、`host-run.log`、`friend-viewer.log`、`cargo-test.log`、`cargo-clippy.log`、`build-static-crt.log`、`viewer-dependencies.txt`、`obs-validation/latency.json`、`obs-validation/recovery.json`、`obs-validation-final/latency.json`、浏览器 PNG 及便携 OBS 自身日志。
- 原始配置、观看凭证、身份和带 ICE 会话字段的完整统计仅保留本机，不加入 Git。朋友 ZIP 中只有观看程序、专用观看配置、启动脚本、说明及许可证，没有主播身份或开发工具。

## 尚未验证

1. 另一台干净 Windows 上解压最终包；本机普通用户测试不能替代干净系统测试。
2. 不同网络、真实朋友端的端到端延迟、多个网络的直连成功率及中继兜底；一次朋友 Edge 出画见末尾补充。
3. 休眠、切换网络、长时间持续播放及多人带宽表现。
4. 物理显示器 / 摄像头 / 扬声器的端到端效果；本轮仅测合成源至浏览器呈现回调。

## 最终朋友包

实际运行 `packaging/live/Package-Live-Viewer.ps1`，最终交付：

```text
F:\git\p2p_file\release\live\viewer-20261023-224420.zip
SHA256: 63c75f768a2baef56a68bb7c98c659bdac5d0b902ea643bed62dfaf5f667ab67
```

旧的 `viewer-20261023-223427.zip` 保留作验收历史，不作为最终分发包。

最终 ZIP 实际检查为五个文件：`Live-Viewer.exe`、`live-viewer.json`、`Start-Live-Viewer.bat`、`使用说明.txt`、`LICENSE.txt`。配置与当前独立直播网关匹配，不含主播身份密钥。程序大小 14,323,712 字节。

对最终 `live-viewer.exe` 和 `live-gateway.exe` 运行 `dumpbin /DEPENDENTS`，均不再导入 VCRUNTIME、MSVCP、动态 UCRT 或 `api-ms-win-crt-*`；仅有 Windows 系统 DLL。构建成功后，`CARGO_ENCODED_RUSTFLAGS` 已恢复到调用前状态。

最终 ZIP 再次实际解压到含空格路径 `F:\git\p2p_file\release\live\Final Friend Package\viewer-20261023-224420\`，用普通用户运行其中的程序，完成直连认证、自动打开默认浏览器、1280×720/30 fps 播放及实际点击开启声音。最终截图为 `release/live/final-package-playing.png`。不需要安装 OBS、MediaMTX、Rust 或 Visual C++ Redistributable，也不需要交互登录；干净的第二台 Windows 仍属待验收。

用户在最终包播放检查完成后可关闭测试 OBS。便携 OBS 配置、合成测试源和启动脚本保留本机；直播网关及 MediaMTX 不设置 Windows 服务或开机自启。私有观看包和配置没有加入 Git。

## 后续真实朋友跨网络排障

2026-10-23 晚，日常运行副本已独立部署到 `F:\program\Veytrane-Live`，不依赖仓库分支。用户反馈另一网络的 Windows 朋友端连接失败，至少一端使用过 VPN / 代理。主播日志显示 IPv4 打洞超时、朋友 IPv6 候选连接失败；两端均未配置 Relay。朋友退出 VPN 后仍失败。

主播网络为 Windows 公用网络，默认阻止入站；原有程序放行规则不覆盖新部署路径。当前 Codex 会话无管理员权限，准备了针对单一程序的脚本，由用户于 **23:24:47** 以管理员身份执行：只允许 `F:\program\Veytrane-Live\bin\live-gateway.exe` 的 UDP 入站，适用 Public / Private，不关闭防火墙、不开放 TCP、不改其他应用规则。网关随后出现朋友端 IPv4 直连建立、服务协议协商和专用凭证认证完成的日志。

用户于 23:29:51 开始真实 OBS 推流。实际参数为 NVENC H.264 high、B 帧 0、3440×1440 / 60 fps、CBR 10000 kbps，音频 Opus 160 kbps；这是日志中的实际输出，并非此前讨论的 3440×2160 参数。

在用户继续推流时，启动独立本机观看端做对照：Chrome 经 TCP/18189 正常播放这一路源，18 秒内解码 1086 帧、丢帧 0、页面异常 0，WHEP 返回 201。这一检查没有记录视频或抓取私人桌面图像。

用户进一步确认：**朋友使用 Firefox 时一直显示连接中，改用 Edge 后能够实际播放。** 因而已确认一次真实朋友跨网络的 IPv4 直连、凭证认证及 Edge 出画，不能继续将其全部标为未执行。Firefox 的具体版本和失败机制没有进一步定位，不宣称所有 Firefox 都不支持；跨网络延迟、弱网、长时间播放、中继及其他浏览器仍未验收。本机对照观看端在检查后退出，保留主播服务和朋友连接。

23:34 后，MediaMTX 另记录 OBS 发布会话的 RTP 丢包和 H.264 FU-A 分片警告。这发生在上述 18 秒本机对照窗口之后，未在本次继续定位，因此本记录不将初次出画扩展为长期稳定性或零丢包保证。

现场证据保存在独立运行目录的 `logs/`、`runtime/gateway-firewall-result.json` 和 `runtime/playback-inspection.json`；不包含在 Git 或朋友包中。
