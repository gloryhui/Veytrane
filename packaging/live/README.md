# Veytrane Live：OBS WHIP + MediaMTX + P2P WebRTC/TCP

直播专用分支：`codex/live-distribution`。可在 Mac 开发，在 Windows 拉取相同提交后原生编译并进行 OBS、浏览器和跨网络测试。当前不包含聊天、账号、录制、回放或转码。

## 两端安装什么

| 角色 | 需要运行 | 不需要 |
| --- | --- | --- |
| 主播（Mac 或 Windows） | OBS、官方 MediaMTX 1.21.2、`live-gateway` | 修改或编译 MediaMTX |
| 观看朋友（Windows） | 分发包中的 `Live-Viewer.exe` 与相邻的 `live-viewer.json` | MediaMTX、OBS、Rust、管理员权限、登录 |
| 源码开发与测试 | Rust stable ≥ 1.90；Windows 另需 VS 2022 C++ Build Tools 和 Windows SDK | GPUI 桌面构建依赖 |

MediaMTX 使用官方单文件程序。Windows 解压 `mediamtx.exe` 即可，不要求安装为系统服务。随附 `mediamtx.yml` 控制监听、协议和录制选项，不改 MediaMTX 源码。

官方资料：[MediaMTX 下载](https://github.com/bluenviron/mediamtx/releases/tag/v1.21.2)、[OBS WHIP](https://obsproject.com/kb/whip-streaming-guide)、[MediaMTX OBS 接入](https://mediamtx.org/docs/publish/obs-studio)。

## Windows 主播从源码测试

先保留本地改动，再拉取远端直播分支：

```powershell
git fetch origin
git switch --track origin/codex/live-distribution
# 已有本地同名分支时：git switch codex/live-distribution；git pull --ff-only
powershell.exe -NoProfile -File .\packaging\live\Build-Live.ps1
```

构建脚本复用仓库的 MSVC 环境发现，构建 `live-gateway.exe` 和 `live-viewer.exe`，输出到 `target/x86_64-pc-windows-msvc/release/`。使用没有配置过环境的普通 PowerShell 也会发现已安装的 Build Tools。脚本执行策略按本机已有配置处理。

从上述官方 Release 下载 `mediamtx_v1.21.2_windows_amd64.zip`，与同一 Release 的 `checksums.sha256` 核对 SHA256，解压后只需把 `mediamtx.exe` 放到 `packaging/live/`。保留仓库随附的 `mediamtx.yml`，不要用下载包的默认配置覆盖它。

首次初始化独立直播网关（将示例信令地址换成实际可用地址）：

```powershell
.\packaging\live\Init-Live.ps1 -SignalServer 'signal.example.com:7000'
# 如需既有中继兜底，加 -RelayServer '<实际中继地址>'，格式与通用节点相同。
.\packaging\live\Start-Live-Host.ps1
```

初始化自动生成直播专用身份和观看凭证，保存在被忽略的 `live-private/gateway/`。已有目录不覆盖；升级继续使用原目录，不复制常用 Agent 或 Sub2API 网关的身份。网关使用独立进程，关闭启动窗口后不保留后台服务、不设置开机自启。

如现有信令启用了 TLS，在启动前将对应的 `signal_tls` 配置加入私有 `gateway.json` 和 `live-viewer.json`；结构与 Sub2API 专用配置相同。不能用明文模式连接 TLS-only 信令。

OBS 中填写：

```text
设置 → 直播
服务：WHIP
服务器：http://127.0.0.1:18891/live/whip
Bearer Token：留空
```

选择单层推流，视频 H.264、音频 Opus、B 帧为 0；码率、分辨率和帧率仍在 OBS 中控制。用编码器的低延迟选项，先以关键帧间隔 1 秒测试。OBS 的具体选项会随版本与编码器变化。点击“开始直播”。MediaMTX 不做转码，不会把不兼容的编码自动改为浏览器可播放的编码。

## 生成朋友的 Windows 包

在完成 Windows 构建和主播初始化后：

```powershell
.\packaging\live\Package-Live-Viewer.ps1
```

脚本在 `release/live/` 创建新目录与 ZIP，不覆盖旧包。朋友解压后双击 `Start-Live-Viewer.bat`，连接成功会自动打开 `http://127.0.0.1:18889/`；默认静音，点击“开启声音”。主播未开播或直播中断时，网页自动重试。朋友保持程序窗口运行即可，退出后本机转发关闭。

观看配置绑定主播 NodeId 和共享观看凭证，朋友首次启动自行生成设备身份。观看过程免登录、免输入验证码；底层仍自动校验节点身份与专用凭证。该文件是可转发的观看权限，不能提交到仓库；更换整批观看权限需生成新的独立直播网关并重新分发。不要删除常用节点或 Sub2API 的目录。

Windows 从源码直接测试观看端也可以：

```powershell
.\target\x86_64-pc-windows-msvc\release\live-viewer.exe --profile '<主播给的 live-viewer.json 的完整路径>'
```

朋友只观看时，无需下载 MediaMTX。

## Mac 开发和主播运行

```sh
cargo build --locked --release --features live --bin live-gateway --bin live-viewer
./target/release/live-gateway --data-dir "$PWD/live-private/gateway" --init --signal-server 'signal.example.com:7000'
MEDIAMTX_BIN='/实际路径/mediamtx' sh packaging/live/start-live-host.sh
```

Mac 下载同一官方 Release 的 `darwin_arm64` 或 `darwin_amd64` 压缩包。使用与 Windows 相同的 OBS WHIP 地址。观看端也能在 Mac 运行：

```sh
./target/release/live-viewer --profile '/实际路径/live-viewer.json'
```

## 链路和固定端口

```text
主播 OBS
  └─ WHIP / WebRTC → MediaMTX（主播回环地址）
                       ├─ HTTP/WHEP → 只读代理 → P2P live-web → 朋友浏览器
                       └─ ICE/TCP  → P2P live-media → 朋友浏览器
```

| 主播服务 | 主播监听 | P2P 服务 ID | 朋友本机监听 |
| --- | --- | --- | --- |
| 观看页面和受限 WHEP 代理 | `127.0.0.1:18890` | `live-web` | `127.0.0.1:18889` |
| MediaMTX WebRTC TCP 媒体 | `127.0.0.1:18190` | `live-media` | `127.0.0.1:18189` |
| MediaMTX WHIP/WHEP HTTP | `127.0.0.1:18891` | 不直接发布 | 无 |
| OBS 本机 WebRTC UDP | `127.0.0.1:18190/udp` | 不发布 | 无 |

网关仅导出 `live-web` 和 `live-media`。只读代理允许固定路径的 WHEP 创建、更新与清理，拒绝 WHIP、其他直播路径和管理接口；不提供文件、测速或管理服务。SDP 回答仅保留指向 `127.0.0.1:18189` 的 TCP 候选，避免浏览器尝试主播局域网地址或误走未经转发的 UDP。节点之间仍复用 QUIC 直连及已有中继逻辑。

端口被占用时程序报错，不自动换端口，以免浏览器 SDP 与映射不一致。要调整端口须同步修改代码常量、SDP 重写和 MediaMTX 配置。该专用网关默认设备上限沿用普通节点的 16 台；可用人数首先受主播上行带宽限制，每位观看者各占一份码率。

## 验证与延迟

```sh
cargo test --locked --features live --lib node::products
cargo clippy --locked --features live --lib --bin live-gateway --bin live-viewer -- -D warnings
```

可重复的 Mac 本机端到端测试（测试工具不进入朋友的包）：

```sh
cargo build --locked --features live --bin live-gateway --bin live-viewer --bin p2p_file
npm install --prefix release/live/test-tools --no-audit --no-fund playwright-core
python3 scripts/live-local-smoke.py \
  --mediamtx '/实际路径/mediamtx' \
  --playwright-module "$PWD/release/live/test-tools/node_modules/playwright-core/index.mjs" \
  --output release/live/smoke-new
```

依赖 FFmpeg 的 `whip` muxer、Chrome 和 Node。脚本在独立私有目录生成身份，启动本机临时信令与两端节点，并在退出时清理自己启动的进程。`--loopback-test` 只用于这类隔离测试，正常分发不要启用。脚本将毫秒时间戳写进源视频，再从浏览器显示出的画面像素读取，记录编码、MediaMTX、P2P 和解码播放的合计延迟；同时断言浏览器确实选中 TCP/18189，并验证停播重开后的自动恢复。它不会把 RTT 或 jitter 当作端到端延迟。

本机实测与尚未完成的 Windows/OBS/跨网络验收见[验证记录](../../docs/task-reports/LIVE_WEBRTC_TCP_VALIDATION.md)。本机结果不能代替朋友实际网络的延迟；TCP 可靠传输在丢包或带宽不足时可能积压。
