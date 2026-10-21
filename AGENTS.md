# 本机服务导航

- 本工作区维护 `service-navigation.html`，作为当前 Mac 的服务入口导航。
- 修改、增加或删除端口映射后，读取两端实际配置并运行 `python3 scripts/update-service-navigation.py --ssh-host ubuntu`，同步导航页，再核对入口与服务端目标。
- 新服务需核实访问协议和路径，在同步脚本的 `SERVICE_UI` 中补充；未知协议只提供地址复制，不能猜成 HTTP。
- 页面上的启用、自启动标签只代表配置状态，不能标成实时在线。不要把令牌、密码、身份密钥或完整私有配置写入 HTML。
- 本机导航不需要启动额外服务。保持 HTML 可直接在 Chrome 打开，CSS、JavaScript 均内联。
