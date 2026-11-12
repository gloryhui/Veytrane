# StreamDesk：Windows/WSL 与 Mac 开发接入交接

核对日期：2026-11-03。此记录是当次检查快照，不代表实时在线或完整直播验收。

**后续接入更新（22:52）**：加密凭据、10GB 独立存储和节点接入已完成。Windows 已交接 WSL 回调 403/400 通过；Mac 已用专属素材完成探测、480p/720p 转码、真实录播发布/停止回调、受保护管理员 480p HLS 分片解码及执行域移除核验。Mac 主服务监听 `127.0.0.1:18096`，图片/视频处理和初始录播调度启用，自动直播及恢复调度仍关闭。旧数据保留；患者端、OBS 和生产未验。下文初期状态按操作历史保留；最新证据见 StreamDesk 仓库 `docs/mvp-v2/execution/GANCAOYUAN_WSL_MAC_20261103.md`。

## Mac 已完成

- Mac NodeId：`ae4c49f3d120f1e013155039eb3263ce`。
- Windows NodeId：`5eef2d53912e77479fa2e61ef3aa27c3`；当次已连接，三个服务均可查询。
- 配置在线提交 revision `22 → 23`，Agent epoch 和 Windows 连接 generation 保持；现有转发、身份和信任保留。
- Worker 转发复用此前已建立的规则；新建两条 SRS 转发。

| 服务 ID | Mac 监听 | Windows 发布目标（用户提供，未读远端配置） | 匿名检查 |
| --- | --- | --- | --- |
| streamdesk-wsl-media | 127.0.0.1:18098 | 127.0.0.1:18097 | /worker/v1/capabilities → 401 |
| streamdesk-wsl-srs-api | 127.0.0.1:21986 | 127.0.0.1:21985 | /api/v1/versions → 401，Basic |
| streamdesk-wsl-srs-hls | 127.0.0.1:28099 | 127.0.0.1:28098 | / → 404 |

SRS 响应头为 `SRS/6.0.191(Hang)`；版本 API 尚未做鉴权读取。三个首选 Mac 端口被旧 Ubuntu 通道占用，未覆盖。

Mac 现有发布服务 `streamdesk-v2-hooks` 的目标为 `127.0.0.1:18096`。已保留原 Ubuntu 节点授权，并追加 Windows 节点。Veytrane 不允许为相同目标重复发布启用服务，因此复用该服务 ID；没有创建第二个重复目标。

私有备份在 Mac：

`/Users/glory/Library/Application Support/p2p-file/configuration-backups/streamdesk-wsl-srs-20261103-195227`

目录 0700，配置/身份及主服务原私有 YAML 副本为 0600。恢复应通过当前 revision 做增量修改，不直接覆盖旧快照或降低 revision。

## Mac 当前主服务边界

当次没有主服务或 Vue 开发服务器监听。源码选择 `dev-local`；私有 YAML 为：

`/Volumes/1TB/Downloads/git/StreamDesk/backend/streamdesk-app/src/main/resources/application-dev-local.yml`

该文件被 Git 忽略，原值未改；它仍连接旧 Ubuntu Worker/SRS，现有处理、发布和自动直播开关开启。本次未启动主服务，未运行数据库登记、迁移或业务任务。

用户确认 Mac / Windows 共用开发 MySQL、Redis，但各自保留私有配置；只择一运行主服务。原集团留给 ubuntu-local，新集团用于 WSL。已创建“甘草园集团”和管理员账号 `v2_dev_gancaoyuan_admin`，显示名“甘草园管理员”，随机密码只保存在 Mac 0600 私有文件中。默认空间计划为 10GB，沿用本项目现有口径为 10,737,418,240 字节；WSL 容量尚待登记。

当前代码每个主服务进程仅支持一套远程 Worker/存储和一套 SRS。创建集团不会形成按集团选择不同 Worker 的路由。切到新远程存储后，旧 Ubuntu 远程卷不能被当作新的卷；旧记录和文件须保留。用户已选择**按需切换媒体配置，一次只使用一套**，本任务不开发多 Worker 路由。

用户已确认 Worker ID 统一使用 `v2_dev_`；本次约定名称为 **`v2_dev_wsl_windows_media`**，由 Windows 侧修改和验证。Mac 尚未鉴权读取该身份。当前主服务要求 `v2_native_worker_code` 与 Worker 返回的 `worker_id` 完全一致，并匹配 `v2_dev_` 前缀，不能继续使用 `v2_wsl_windows_media`。

## 可交给 Windows Codex 的操作说明

请在 Windows / WSL 的现有部署中执行以下工作，先读当地 AGENTS.md 和私有配置，保留身份、业务文件、任务回执和现有通道。不要启动第二个连接相同开发数据库的主服务。

1. 安全检查并将本 WSL Worker ID 对齐为 `v2_dev_wsl_windows_media`。先检查旧 ID 是否已有任务/登记/活动执行域；如果有，先停止新增任务并提出安全切换方案，不重命名卷、不清任务或回执。现有 Worker 密钥保持不变。确认 capabilities 和 publish/capabilities 返回相同 Worker ID；处理能力、配方身份及发布能力分别验证。发布能力应报告 app `v2_dev_streamdesk`、peer `127.0.0.1`、port `1935`，这些地址是 WSL 执行域视角。

2. 核对 Windows 节点和三个现有服务的实际目标、仅授权 Mac 节点的访问范围，以及 Windows → WSL 的实际转发。不要额外暴露匿名 Worker、SRS 管理口或 HLS 公网入口。匿名 Worker/API 应 401，凭据鉴权应分别成功。SRS versions 需实际返回 6.0.191；同时读取 streams 等受限 API，不能只检查匿名响应头。

3. 使用现有 Windows Veytrane CLI 查询 Mac 的服务目录，应能看到 **`streamdesk-v2-hooks`**。为它增加一条专用 Windows 回环转发，优先监听 **`127.0.0.1:28196`**，peer 为 Mac NodeId，service_id 为 `streamdesk-v2-hooks`。若已占用，选新空闲回环端口并反馈。按当前 revision 在线提交，先私有备份，保留旧通道。它的服务端目标已经是 **Mac `127.0.0.1:18096`**，不是 Windows 或 WSL 的 18096。

4. 核实 WSL 的实际 networkingMode。mirrored 模式也须从 WSL 实测 `127.0.0.1:28196`，不能只凭模式推断可达。NAT 模式若不可达，Windows Veytrane 继续只监听回环，另用受限 Windows 代理将已确认的 WSL 可达 Windows 虚拟网卡地址和专用端口转到该回环入口；只允许该 WSL 来源，并备份对应代理/防火墙状态。不要绑定 0.0.0.0、放宽全网防火墙或把 Mac 局域网地址当成默认可达路径。需要管理员的修改遵循当地审批。

5. 回调完整方向为：**WSL SRS → WSL 可达的 Windows 回调入口 → Windows Veytrane → Mac streamdesk-v2-hooks → Mac 127.0.0.1:18096**。Mac 主服务目前未启动，因此当前不能验证 HTTP 回调。可以先保留未启用的私有回调草稿；不要把端口监听、TCP 接通或占位回调当作完成。

6. 为新 WSL 环境生成或读取一份专属 64 位小写十六进制回调密钥，后续 Mac 私有 secret 文件与 WSL SRS on_publish/on_unpublish 必须一致。旧 ubuntu-local 回调密钥保留。现有协议路径是 `/internal/v2/srs/<64hex-secret>`；完整 URL 只写私有配置，不能打印进终端、聊天、仓库或普通访问/错误日志。回调 body 同样可能含发布凭据，禁止记录。Mac 侧保持 `server.forward-headers-strategy=none`、关闭访问日志；Veytrane 在 Mac 回环连接主服务，预期 RemoteAddr 为 127.0.0.1，不信任转发头伪造来源。

7. 将如下字段从真实 WSL 私有配置读取到一个新建的私有 JSON 文件：`worker_id`、`worker_api_key`、`srs_admin_username`、`srs_admin_password`、`srs_app`、`publisher_peer`、`publisher_port`、`srs_hook_secret`。这里没有任何真实凭据，不能用占位值代替实际读取。文件在 WSL 中为 0600、父目录 0700；如经过 Windows 文件系统，限制 ACL 到当前用户，禁止进入 Git 和普通日志。

8. 可以通过现有、已验证身份的 Veytrane 文件传输交给 Mac：Mac 对 Windows 的文件能力授权当前已存在。使用当地已部署且匹配版本的 `p2p-node send ae4c49f3d120f1e013155039eb3263ce <新私有文件绝对路径>`，路径作为单个参数且不含秘密；先核对对端完整身份与可信认证，失败不改为明文外网传输。若只能在 Windows 执行 CLI，需确认其能安全读取该文件，且对应 Windows 路径 ACL 受限。反馈仅包括文件名、任务 ID 和成功状态，不反馈内容。Mac 接收根当前是 `/Users/glory/P2PFile/Received`，完成后由 Mac 侧核对来源、文件权限并转入 Git 忽略的私有目录；不自动清理传输副本。

9. Mac 主服务配置并启动后，从 WSL 分别验证错误回调密钥返回 403、正确密钥加无效空 JSON 返回 400。用内存构造 URL/请求，禁止输出完整 URL、异常对象或 body；这两项只证明到达预期鉴权/解析入口，不证明发布授权成功。最后使用主服务生成的专属业务任务验证真实 on_publish/on_unpublish、HLS 和停止闭环。不要手工伪造有效发布或直接向未授权流推送测试视频。

## 凭据到达后的 Mac 配置计划

先备份原私有 YAML，保留 MySQL/Redis、已有加密密钥、本地 storage_root/storage_code 和旧远程配置快照。下面是计划值，尚未写入活动配置：

| 设置 | Mac 计划值 |
| --- | --- |
| v2_media_worker_url | http://127.0.0.1:18098 |
| v2_native_worker_code | v2_dev_wsl_windows_media（须先鉴权确认远端已修改） |
| v2_media_worker_storage_code | v2_dev_wsl_windows_files（拟定；须核对现有登记和新卷） |
| v2_srs_node_code | v2_dev_wsl_windows_srs（拟定；须核对现有登记） |
| v2_srs_admin_endpoint | http://127.0.0.1:21986 |
| v2_realtime_hls_origin | http://127.0.0.1:28099 |
| v2_srs_app | v2_dev_streamdesk（须核对能力和 SRS 配置） |
| v2_video_publisher_peer / port | 127.0.0.1 / 1935（WSL 执行域视角） |

Worker 和 SRS 凭据仅从私有文件读入；Worker 使用 `X-Media-Worker-Key`，SRS 使用 Basic 请求头，不写进 URL。检查 capabilities、publish/capabilities、配方散列、Worker 身份、SRS 版本、app/peer/port；随后再设计静默调度的登记/启动顺序，不能把当前全部开启的旧调度直接对准新卷。

使用项目已有 `scripts/v2_dev.sh --identity-admin` / `--media-worker-admin`，先核现有记录，再按确认的员工、集团、额度登记；不手写 SQL、启动迁移、替换旧卷 code 或清理历史记录。管理工具仍可能连接数据库、构造远端能力客户端，缺少条件时不运行。

最终业务验收须使用专属测试素材，分别记录：上传与文件身份、处理任务和成品、发布授权回调、发布状态、受保护 HLS 的实际解码、停止、执行域退出/关闭/移除、任务与业务引用收口。只处理本次测试对象，保留旧业务数据。

## 切到 Windows 运行主服务

先在原控制端停止新增调度，并通过业务流程完成活动任务/发布和租约交接；确认 Mac 主服务停止后再启动 Windows 主服务。不能因“同时只运行一个”而跳过未收口执行域核验。

Windows 自己的私有配置使用实际可达的 Windows 本机入口（通常 Worker 18097、SRS API 21985、HLS 28098，必须实测），不能复用 Mac 的 18098/21986/28099。本地 controller/storage 路径、回调 secret 文件路径改为 Windows 合法路径，旧本地卷身份不能映射到空目录。连接同一 WSL Worker 时，Worker/storage 身份应保持一致。

WSL SRS 回调改到实际可达的当前 Windows 主服务入口，不能继续经过 Mac；两端以独立私有配置保存相同业务密钥、数据库/Redis隔离参数及 WSL 服务凭据，并协调回调配置切换。注意 SRS 节点现有登记绑定 admin_endpoint，Mac 与 Windows URL 不同，需先检查现有项目管理规则和记录，不能直接改同一 node_code 的 endpoint 或绕过 fence；必要时各控制端使用经过登记的独立 SRS node_code，并核查旧发布绑定收口。

Windows/WSL 配置修改、Worker/SRS 凭据鉴权、反向回调验证、WSL 容量/Worker/SRS/发布节点登记、主服务启动及完整业务直播验收尚未执行。甘草园集团和员工已经创建，实际回执见下文。

## 甘草园集团操作补记

用户补充确认按需切换媒体配置，一次只使用一套；新建“甘草园集团”、新员工账号和默认 10GB 空间。

通过既有 `scripts/v2_dev.sh --identity-admin` 创建，管理 JVM 的 36 项相关 enabled-feature 开关单独置 false，不更改活动 YAML，不启动 Web 主服务：

- 员工账号：`v2_dev_gancaoyuan_admin`，显示名“甘草园管理员”；创建回执 `CREATED`。
- 员工 UUID：`8a2d7a4c-fb2d-49c3-80f4-58b85d9159bb`。
- 新集团 UUID：`0174682b-d24b-485f-88fd-6ba5920b7aaa`，新员工已作为首位 `GROUP_ADMIN` 开通。
- 集团开通命令 UUID：`1eaad802-3cc9-44f6-a491-3758bfdae1e9`。
- 稳定请求标识：`gancaoyuan-wsl-employee-20261103`、`gancaoyuan-wsl-group-20261103`；私有审计均有 `COMPLETED`。
- 随机初始密码仅存于 Mac `/Volumes/1TB/Downloads/git/StreamDesk/.local/mvp-v2/private/gancaoyuan-wsl-20261103/login.private.json`（0600，Git 忽略），不进入本文或聊天。
- 10GB 按现有项目口径为 10,737,418,240 字节。新 WSL 卷的物理限额和集团限额计划均为此值，但**额度尚未登记**；须先完成 Worker 凭据和真实卷身份核验，不能登记到旧 Ubuntu 卷。

管理操作前使用已有只读 inventory 工具核实开发数据库与既有活动操作员工；操作后再次读取，旧员工/集团、集团额度、物理卷、Worker/SRS 记录、未关闭执行域数量和占用中的发布绑定数量与操作前一致。活动私有 YAML 也与备份逐字节一致。没有手写 SQL、运行迁移或更改既有员工密码。后续重试须先核对私有回执及稳定请求标识，不能重新生成一组集团开通请求。
