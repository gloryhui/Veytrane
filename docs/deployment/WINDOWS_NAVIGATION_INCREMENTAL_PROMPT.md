# 增量任务：补充 Windows 服务汇总 HTML

你正在执行之前那份 Windows Veytrane 安装与映射交接文档。请继续原任务，保留已完成的安装、身份、授权、映射和自启动设置；本次仅增加以下导航页面交付，不重新执行部署。

随本 Prompt 提供 `windows-service-navigation.template.html`。请以这份 HTML 为基础，保留现有字体、配色、卡片布局、图标、搜索筛选、功能分组、端点颜色及响应式样式，不另行设计页面。

1. 在映射配置确定后，从 Windows 实际配置读取 `forwards`、`service_ids`，通过已有合法管理通道核对 Ubuntu 服务的 `id/target/enabled`。使用本次安装最终采用的端口，不能照搬模板里的首选端口，也不要为迁就模板更改已经配置好的映射。未完成的部署继续按原任务推进。
2. 更新 HTML 内 `navigation-data` JSON 的所有服务信息和带时区的 `updatedAt`。核对完毕后将 `isTemplate` 设为 `false`；未核对的项目保留明确的待核对提示。JSON 内的 `<`、`>`、`&` 必须转义，避免破坏 script 标签。
3. 导航包含最终配置的 12 条远端映射，以及 1 个 Windows 本机管理入口。顶部同排左侧为远程 Ubuntu 管理，右侧为本机 Windows 管理；本机管理项使用 `windows-admin`，按实际 Web 网关端口生成并注明需先启动本机 Web 网关。其他按路由与网络、应用服务、文件共享、其他分组。
4. 每张远端卡片保留“远端 Ubuntu 目标（橙色）→ P2P → Windows 本机监听（蓝色）”的完整链路。Ubuntu 回环地址也必须标为远端 Ubuntu；本机 Windows 管理注明直接访问。已启用 / 自动启动只表示配置状态，不标为实时在线。
5. 保留已核实的协议与路径，HTTPS 使用适当的 `localhost` 主机名。SSH 命令、Ubuntu token 获取提示均使用最终 SSH 映射端口与实际用户名；Windows 本机 token 获取提示使用实际数据目录，不能机械保留默认路径。Samba 填入真实共享名；若采用 1445 等非标准端口，按当前 Windows 版本提供适用的连接命令或限制说明，不能生成 `\\127.0.0.1:1445\share` 这样的伪 UNC 路径。未知协议仅提供地址复制。
6. 保存正式页面到 `%LOCALAPPDATA%\p2p-file\navigation\service-navigation.html`，在当前用户桌面创建“Veytrane 服务导航”快捷方式，并在 Edge / Chrome 中打开检查。无需安装 Nginx，CSS / JavaScript 全部内联，不使用外部 CDN、不增加后台同步或网页定时刷新。
7. 页面和交付文件不包含密码、私钥、管理 token、Chromium 登录凭据或 DSH token 链接。管理 token 只提供复制命令，不把真实内容嵌入 HTML。不能保留 Finder、pbcopy、⌘K / ⌘V 等 Mac 操作说明。
8. 在 Windows 浏览器实际核对管理卡顺序、13 个入口、最终端口与 URL、搜索筛选、复制按钮和窄窗口布局。另外留下可手动执行的导航更新脚本或准确更新步骤，之后映射变更再按需更新页面。将页面路径、快捷方式和检查结果追加到原部署报告。

无需重装 Agent、重新初始化身份、重做授权或修改自启动来完成这个增量任务。若缺少 Ubuntu 管理凭据，先完成不依赖该凭据的页面工作，对未核验目标作出标记，并继续原部署中其余可执行事项。
