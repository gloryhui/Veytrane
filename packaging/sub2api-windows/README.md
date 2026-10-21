# Sub2API 专用分发包

该模式与通用桌面客户端及现有 JSON Agent 分离。ZIP 内仅有 `Sub2API.exe`、`Start-Sub2API.bat`、使用说明及许可证；不包含通用 Agent、管理 CLI、Web 管理端、配置 JSON 或任何预生成身份。

每台 Windows 的当前用户首次启动生成独立 Ed25519 身份，保存在 `%LOCALAPPDATA%\Sub2API-Client`。所有收件人可使用同一 ZIP，凭证在构建时嵌入，但身份从不嵌入。客户端不接受参数，不读取运行时服务器/端口配置，不接受环境变量覆盖，仅监听 `127.0.0.1:18081`，仅请求 `sub2api` 服务。BAT 路径有引号，使用 CRLF，不需要管理员权限或额外解释器。

## 服务端边界

运行独立 `p2p-sub2api-gateway` 进程，使用独立身份和独立密码校验值，不能复用管理员节点的身份、密码、信任列表或令牌。网关只将当前已通过专用密码认证的设备加入内存 ACL，且仅发布 `sub2api → 127.0.0.1:8080`。认证断开后收回 ACL；文件和测速能力禁用；不创建管理 HTTP/IPC 或文件任务服务。不存在“隐藏了客户端配置就获得安全”的假设。

`sub2api-gateway` 构建特性将这个独立网关的设备会话上限设置为 128；普通 Agent/桌面构建仍为 16。握手并发及 TCP 总流量配额仍有界。这是容量上限，不是经过 128 台压测的吞吐承诺。

访问凭证为共享 bearer 权限：可从收件人的程序中提取，也可转发。持有者可以生成新设备身份，因此无法靠单独禁用某个旧 NodeId 来可靠地封禁一个持有凭证的人。撤销整个分发批次需更换网关校验值并重新打包。密码不会赋予现有管理员节点权限。Sub2API 自身仍按它的账号权限控制；不要分发该应用的管理员账号。

## 构建及部署

1. 构建服务器：`cargo build --locked --release --features sub2api-gateway --bin p2p-sub2api-gateway`。
2. 为网关选择全新私有目录，执行 `p2p-sub2api-gateway --data-dir <目录> --init --signal-server <HOST:PORT>`，从 stdin 输入独立随机 12 位字母数字密码。仅返回新网关 NodeId。已存在的身份/配置会被拒绝覆盖。
3. 网关 `gateway.json` 仅保存密码校验值。用该目录运行服务器并由 systemd 管理；不要改变现有 Agent 的部署。
4. 在仓库外或被忽略且权限为 0700 的私有目录准备客户端构建 JSON：`signal_server`、`server_node_id`、`password`；可选 `signal_tls`、`relay_server`。它是发行者的私有构建输入，不能提交或加入 ZIP。
5. 构建时设置 `P2P_SUB2API_PROFILE_FILE=<绝对路径>`，然后 `cargo build --locked --release --features sub2api-client --bin sub2api-client --target x86_64-pc-windows-msvc`。也可使用 LLVM-MinGW 配合官方 `x86_64-pc-windows-gnullvm` 标准库交叉构建，静态链接 CRT，并检查 DLL 导入。默认未提供 profile 的构建会在运行时拒绝连接。
6. `python3 scripts/package-sub2api-windows.py --binary <sub2api-client.exe> --output <新输出目录>`。包外的 manifest 记录实际父提交、未提交状态、源文件与程序的 SHA256；它不声称未提交代码已经发布到 Git。

首次启动持锁，避免重复实例。核心保持断线重连；检测到休眠/接口变化后清理会话并重建网络。连通探测后打开默认浏览器，端口占用时直接报错，不猜测另一个端口。关闭窗口不保留后台任务或开机自启。

## 自动恢复

- 在 STUN 探测过的同一个 IPv4 QUIC socket 上，每 15 秒发送一次 Binding Indication 维持映射。接收仍只由 Quinn 所有，不启动第二个 UDP 接收循环。
- 网关保持单一进程与目录锁。接口变化、长时间停顿或网络核心退出时，在进程内停止旧核心并重建网络；身份与校验值不重建、不回滚。systemd 继续作为进程异常退出的兜底。
- 无已连接用户时，实际闲置五分钟会刷新地址；连续三次传输失败、连接阶段无进展、信令长期未确认也会触发有时间间隔的重建。密码认证失败不计入传输失败；存在已连接用户时不触发空闲/失败刷新。
- 客户端一分钟没有设备通道，或已有通道连续三次服务目录探测无响应，会自动重建本机网络。休眠和接口变化沿用宿主状态监测。健康通道不重复请求信令配对。
- 网关的服务目录仍可访问时，HTTP 暂不可用会明确显示等待 Sub2API 后端，不因后端服务故障反复拆除正常通道。网络恢复并允许原有通信后会继续连接；凭证撤销或持续阻断 UDP 不会被自动恢复绕过。
- 网关私有目录中的 `runtime.json` 仅记录网络代次、恢复原因和连接状态，不是管理 API，不含访问凭证。用户配置、身份文件和授权范围均保持原状。

`examples/sub2api-client-harness.rs` 仅供发行者在隔离目录进行故障测试，不会进入 ZIP。实际测试工具通过独立测试服务的 cgroup 阻断 IPv4/IPv6 网络，不应给正常业务服务设置测试防火墙规则。

验证需分别记录：Rust/跨平台编译、真实 QUIC 协议权限测试、Ubuntu 实际接入、Windows BAT/浏览器/休眠真实设备验收。交叉构建成功不能代替 Windows 实机验收。
