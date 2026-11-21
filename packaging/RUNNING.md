# Veytrane 桌面候选包

标准版包含设备连接、TCP 转发、测速和本机管理。文件增强版增加原生文件工作台和 Web 文件面板。两个版型都需要同一提交的桌面、Agent 和 CLI。桌面通过 IPC 附着已运行 Agent，退出桌面后 Agent 继续工作。

三平台构建脚本默认产出标准候选。Shell 和 BAT 使用 `--with-files`，PowerShell 使用 `-WithFiles` 产出文件增强候选。增强归档带 `-files` 后缀，CI 资产名以 `veytrane-files-` 开头。对于已有节点，按[宿主升级说明](node/README.md)保留身份、配置及任务数据。

这是 GPUI MVP 的候选产物。版本、完整源码 commit、目标、实际二进制/归档大小、SHA256、签名状态在 `build-info.json` / `candidate.json`。`SHA256SUMS` 校验解包后的文件；旁置 `.sha256` 校验归档。不要把 PR 的 CI 成功当作最低系统、物理设备或双 NAT 验收。最终状态以 [Issue #24](https://github.com/gloryhui/Veytrane/issues/24) 为准；未完成 mandatory 外部验证之前不创建正式 Release。

## 安装与运行

Linux 包针对 Ubuntu 24.04 x86_64（glibc 2.39）；不是所有 Linux 发行版的通用静态包。解压 tar.gz，运行包内 `./p2p-desktop`。原生窗口同时支持 X11/Wayland 路径；目前 Linux 原生 smoke 使用 X11/Xvfb/Mesa，不能代替真实 Wayland/硬件测试。Ubuntu 运行依赖：

```sh
sudo apt-get install libxcb1 libxkbcommon0 libxkbcommon-x11-0 libfontconfig1 libfreetype6 libwayland-client0 libwayland-cursor0 libwayland-egl1 libvulkan1 mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gtk fonts-noto-cjk
./p2p-desktop --version
./p2p-desktop
```

请安装适合 GPU 的 Vulkan 驱动；Mesa 软件渲染只是可用的验证路径。桌面 portal 服务提供原生文件/目录选择器。实际 ELF 动态依赖和符号版本保存在 `native-inspection.json`；Wayland/Vulkan/portal 的动态加载和服务依赖不会全部出现在 ldd 中。

Windows：x64，产品最低目标 Win10 22H2，同时验证目标 Win11。解压 zip 到普通用户可读目录，双击 `p2p-desktop.exe`。不用 MSYS/Cygwin；不产出 Windows ARM/32 位包。程序为 GUI subsystem，查看 metadata 用 PowerShell 的显式等待与输出捕获：

```powershell
Start-Process -FilePath .\p2p-desktop.exe -ArgumentList '--build-info' -Wait -RedirectStandardOutput "$env:TEMP\p2p-desktop-build-info.json"
Get-Content "$env:TEMP\p2p-desktop-build-info.json"
Get-FileHash .\p2p-desktop.exe -Algorithm SHA256
Get-AuthenticodeSignature .\p2p-desktop.exe
```

实际导入的系统/运行库列表位于 native-inspection.json。若目标系统提示缺少 VCRUNTIME/MSVCP 等 v14 运行库，按 [Microsoft 官方下载说明](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist) 安装对应 x64 Visual C++ Redistributable；运行库版本须不低于构建工具所需版本，不从第三方站点复制 DLL。

候选未用开发者证书签名，Windows 安全提示应以操作系统实际显示为准。CI windows-2022/windows-all 是 hosted runner，不证明 Win10/Win11 真机运行。PE x64 架构、导入依赖和实际 Authenticode 状态随包记录。

macOS：产品目标 macOS 13+，分别提供 Apple Silicon arm64 与 Intel x86_64 包。
Apple Silicon 选择 `aarch64-apple-darwin`，Intel Mac 选择 `x86_64-apple-darwin`。
CI 下载中 `veytrane-app-only-*` 是轻量应用 ZIP，`veytrane-*` 是含依赖来源材料的
完整候选。解压 ZIP，将 `Veytrane.app` 放在 Applications 或用户可读目录，通过 Finder 打开。
两种包的 Info.plist 都保持 `LSMinimumSystemVersion=13.0`；CI 检查 Mach-O 最低部署版本，
并在各自架构的 macOS runner 原生执行回归，不能代替 macOS13 实机交互验收。

```sh
"Veytrane.app/Contents/MacOS/p2p-desktop" --build-info
codesign -dv --verbose=4 "Veytrane.app"
```

只需本机可运行的轻量 `.app` 时，打包命令加 `--app-only`。该模式只输出 `Veytrane.app`，保留依赖许可证/声明文本，不复制完整 `.crate` 源码归档，也不生成候选 ZIP、`candidate.json` 或 SHA256 旁置文件；因此它不是完整审计候选包：

```sh
./packaging/macos-arm64/build.sh --app-only --output "$P2P_PACKAGE_DIR"
open "$P2P_PACKAGE_DIR/Veytrane.app"
```

Intel 本机构建使用 [macos-x86_64 入口](macos-x86_64/README.md)：

```sh
./packaging/macos-x86_64/doctor.sh
./packaging/macos-x86_64/build.sh --app-only --output "$P2P_PACKAGE_DIR"
```

Apple Silicon 本机使用 [macos-arm64 入口](macos-arm64/README.md)。构建时要求匹配架构的
原生终端和硬件；Rosetta 不作为 Intel 本机验证。不提供 Universal 包。

没有 Developer ID/公证凭证；包只有实际验证的 ad-hoc 签名（没有开发者证书），未公证。不要把 ad-hoc 叫正式签名，也不删除系统 quarantine 以制造通过截图。允许打开候选的操作应由设备持有人按系统提示完成。

## 应用图标

macOS 应用包自带 ICNS 图标；Windows 构建会将 ICO 嵌入可执行文件和窗口。
更新图标后需重新构建并打包，旧应用包不会自动变化。

Linux 包内提供 `p2p-file.png` 和 `p2p-file.desktop`。如需加入桌面应用菜单，
在解压后的包目录执行（需将 `~/.local/bin` 加入 PATH）：

```sh
install -Dm755 p2p-desktop "$HOME/.local/bin/p2p-desktop"
install -Dm644 p2p-file.png "$HOME/.local/share/icons/hicolor/1024x1024/apps/p2p-file.png"
install -Dm644 p2p-file.desktop "$HOME/.local/share/applications/p2p-file.desktop"
```

## 配置、自建信令与可信设备边界

首次启动保留空信令配置，附近设备或已知地址可直接连接。需要跨网络发现时，在设置中加入自己的信令地址，通过“保存网络并停止节点”提交，再启动后台。使用独立 CLI 启动信令服务。

```sh
cargo build --locked --release --no-default-features --features standalone --bin p2p_file
./target/release/p2p_file signal-server --help
./target/release/p2p_file signal-server --listen 0.0.0.0:8900
```

信令服务使用可达的 TCP 端口，设备间允许实际 UDP 打洞和 QUIC 流量。标准首页按设备、服务 ID 和本机监听管理端口转发。需要传输原生文件时，在两端增强版的设置中启用文件传输并重新启动 Agent，确认对端授权和本机接收目录。

复制完整本机 ID；对端输入该 ID 并连接。接收方无需预设发送方，收到任务会自动显示。发送并发仅支持 1/2/3；修改后保存才应用。测速使用同一已认证连接、30秒或1–10分钟，与该 peer 的文件活动互斥，接收结果的 elapsed 决定平均速率。取消后速率归零，允许随后文件或新的测速。

## 配置与恢复

配置/身份由本机系统应用目录保存，不进产物、不传给对端。Linux 通常是 `~/.config/p2p_file/settings.json` 和 `~/.local/share/p2p_file/data`；Windows 使用 Roaming 配置与 Local 数据目录下 `p2p_file`；macOS 是 `~/Library/Application Support/p2p_file/settings.json` 和其 `data` 子目录。位置由 dirs 的系统目录解析决定。备份身份密钥、配置、tasks.json、任务 staging/journal/bitmap；它们可能含私有本机路径/密钥，不应上传到公开 Issue。

崩溃或硬杀后，Interrupted 记录不会自动发送。两端恢复原应用数据与接收目录，显式连接原 peer，再选择原 TaskID 的“继续”。不能换身份/复制新的空 TaskStore 来冒充断点恢复。源已改变/消失或权限不足会显示明确失败；修复原因后手动继续。Completed 以落盘回执为准，暂停/失败/离线速率归零。

重名文件保留原内容到固定时间戳备份，新内容发布到原名；碰撞时用固定后缀，不删除用户旧文件。不要手工删任务 staging、journal、旧备份或回执来处理错误。崩溃验证不等于整机断电/介质损坏；发布要求同一卷支持安全硬链接/不可覆盖提交，Windows 目录 fsync 限制已记录。

## 来源、许可证和证据

`LICENSE` 是本仓库 MIT，GPUI 适配代码及 Apache 条款在 `THIRD_PARTY_NOTICES.md`。完整候选的 `dependencies.json` 记录目标平台、桌面与 Agent 的实际构建组合、普通及构建依赖、上游许可证和已锁定源码 SHA256。macOS 将资料置于 `.app` 的 `Contents/Resources`。随包提供 `licenses/` 和原始 `.crate` 归档，轻量 `--app-only` 输出许可证文本。系统动态库由系统包管理器提供其许可材料。

包体积包含这些来源材料，启动耗时需由实际 GUI 窗口测量；metadata 子进程耗时只称 metadata probe。真实 600 秒、进程强杀/恢复、目录重名、中文/DPI 的证据和未执行项见仓库 T010/T011/T012 文档与 Issue #24。缺少物理 Win10/Win11、macOS13 Apple Silicon、真实中文 IME/Wayland、Windows↔Linux 双 NAT 两方向文件/测速及 Apple Silicon 互传时，最终状态保持 `FINAL_BLOCKED_EXTERNAL_VALIDATION`。
