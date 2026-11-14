# 本机服务导航

- 本工作区维护 `service-navigation.html`，作为 macOS、Windows、Linux 共用的服务入口导航。界面与管理逻辑共用，设备名称、路径、端口及独立 SSH 通道从私有导航配置读取，启动和重启通过系统适配处理。
- 修改、增加或删除端口映射后，读取两端实际配置并运行 `python3 scripts/update-service-navigation.py --navigation-config <私有配置路径>`（现有 Mac 可继续使用 `--ssh-host ubuntu`），同步导航页，再核对入口与服务端目标。
- 导航 HTML/API 由 `p2p-agent` 同一 Rust 进程托管，三平台统一为回环 `http://127.0.0.1:3001/`。Python 同步器只做单次生成，不常驻、不提供 HTTP；不得新增 Python 导航后端或 Nginx 依赖。
- 集成配置默认位于 Agent `installed.json` 的 `data_dir` 父目录下 `navigation/config.json`；也可由 `p2p-agent --navigation-config` 显式指定。准备工具只保存配置和私有页，不写 Agent 身份、不重启 Agent，也不停止旧 gateway。
- 导航集成本机管理台；网页保存映射后更新私有导航副本，其他入口修改映射后仍可运行单次同步命令。监控默认关闭，仅在用户主动开启且页面可见、有焦点时每 10 秒查询状态；隐藏、失焦或离开页面即停止，不启用后台自动同步或整页定时刷新。
- 新服务需核实访问协议和路径，在同步脚本的 `SERVICE_UI` 中补充；未知协议只提供地址复制，不能猜成 HTTP。
- 页面上的启用、自启动标签只代表配置状态，不能标成实时在线。不要把令牌、密码、身份密钥或完整私有配置写入仓库中的 HTML。用户明确要求展示的 Chromium、RustFS 账号密码及 DSH token 登录链接，仅由同步脚本写入用户私有导航文件（POSIX 0600，Windows 当前用户 ACL；Mac 默认 `~/Library/Application Support/p2p-file/navigation/service-navigation.html`）；仓库模板不含真实凭据。
- 导航布局中，远端与本机节点管理入口在顶部同排显示，远端在左、本机在右，名称来自设备配置；其他入口按“路由与网络、应用、文件共享、其他”分组。新入口在同步脚本 `SERVICE_UI` 中指定功能分类，未知协议仍只提供地址复制。
- 每张服务卡片须醒目展示完整端口链路：左侧远端服务（橙色）→ P2P → 右侧本机监听（蓝色），颜色按端点角色确定，与两端节点管理页面保持一致，目标地址标注实际远端设备名称，即使是回环地址也不能称为“本机”；本机管理入口注明本机直接访问，远端管理入口同样展示端口映射。
- 三平台均从 `http://127.0.0.1:3001/` 使用内置导航与管理 API。Rust 宿主只监听回环并直接提供页面；不再使用独立 Python HTTP 后端或 Nginx 代理。启用本机私有目录中的 `show-chromium-login`、`show-rustfs-login` 或 `show-dsh-login` 标记后，单次同步只把 Chromium、RustFS 登录信息或当前 DSH token 写入私有导航页；仓库模板不含真实凭据，CSS、JavaScript 均内联，不启用后台同步。
- 内置管理 API 只经已安装 Agent 的 revision 机制修改规则；保留无关配置、身份和授权。配置独立 SSH 管理通道时须核对完整远端节点身份；未配置该通道的设备仅选择已发布授权服务。单条转发启停、设备重连与整 Agent P2P 核心重建须明确区分影响范围；P2P 核心重建期间 HTTP 导航保持可用。准备工具不得重启 Agent、替换身份配置或管理旧 gateway。验收优先使用隔离模拟环境；真实写入仅使用专用临时映射。
