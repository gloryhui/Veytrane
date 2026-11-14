# 本机服务导航

- 本工作区维护 `service-navigation.html`，作为当前 Mac 的服务入口导航。
- 修改、增加或删除端口映射后，读取两端实际配置并运行 `python3 scripts/update-service-navigation.py --ssh-host ubuntu`，同步导航页，再核对入口与服务端目标。
- 导航集成本机管理台；网页保存映射后更新私有导航副本，其他入口修改映射后仍可运行单次同步命令。监控默认关闭，仅在用户主动开启且页面可见、有焦点时每 10 秒查询状态；隐藏、失焦或离开页面即停止，不启用后台自动同步或整页定时刷新。
- 新服务需核实访问协议和路径，在同步脚本的 `SERVICE_UI` 中补充；未知协议只提供地址复制，不能猜成 HTTP。
- 页面上的启用、自启动标签只代表配置状态，不能标成实时在线。不要把令牌、密码、身份密钥或完整私有配置写入仓库中的 HTML。用户明确要求展示的 Chromium、RustFS 账号密码及 DSH token 登录链接，仅由同步脚本写入用户私有目录 `~/Library/Application Support/p2p-file/navigation/service-navigation.html`（0600）；仓库模板不含真实凭据。
- 导航布局中，Ubuntu 与 Mac 节点管理入口在顶部同排显示，远端 Ubuntu 在左，本机 Mac 在右；其他入口按“路由与网络、应用、文件共享、其他”分组。新入口在同步脚本 `SERVICE_UI` 中指定功能分类，未知协议仍只提供地址复制。
- 每张服务卡片须醒目展示完整端口链路：左侧远端 Ubuntu 服务（橙色）→ P2P → 右侧 Mac 本机监听（蓝色），颜色按端点角色确定，与两端节点管理页面保持一致，目标地址统一标注“远端 Ubuntu”，即使是回环地址也不能称为“本机”；Mac 管理入口注明本机直接访问，Ubuntu 管理入口同样展示端口映射。
- 本机导航可通过现有 Nginx 的 `http://127.0.0.1:3001/` 或直接在 Chrome 打开；配置、监控和重启需从 HTTP 入口使用。Nginx 精确映射用户私有导航副本，并将 `/api/navigation/` 代理到独立的 `127.0.0.1:17790` 管理后端，不暴露仓库或私有状态目录。启用本机私有目录中的 `show-chromium-login`、`show-rustfs-login` 或 `show-dsh-login` 标记后，正常单次同步命令会同时更新仓库模板数据和私有副本，分别添加 Chromium、RustFS 登录信息或当前 DSH 服务实例的 token 登录链接；CSS、JavaScript 均内联，不启用后台同步。
- 管理后端只经已安装 Agent 的 revision API 修改规则；保留无关配置、身份和授权。Ubuntu 管理使用独立 SSH 并核对完整节点身份，Windows 第一版仅选择已发布授权服务。单条转发启停、设备重连与整台 Mac Agent 重启须明确区分影响范围；安装管理台不得重启 Agent，验收使用专用临时映射。
