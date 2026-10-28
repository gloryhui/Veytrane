# Sub2API Windows 专用包更新与分发验收

后续状态：本文所记 ZIP 未配置中继，已被同日的[固定中继版](SUB2API_FIXED_RELAY_20261028.md)替代。以下保留较早构建的原始校验值与验收范围。

验收日期：2026-10-28（Asia/Shanghai）。专用分支原为 `e6e8612`，本次快进到主线 `6b084199088cd578d3668f16657de291ecbdf18c`，包含落后的四个提交。Windows 客户端和 Ubuntu 专用网关均来自该干净提交；本文随后补充，不属于二进制的构建输入。

## 更新原因与使用方式

主线已包含自动重连、Docker 网桥及接口变化处理，以及弱网丢包时的 QUIC BBR 吞吐修复。原专用网关和此前交付 Windows 包来自 2026-10-21 的 `ec72ed4`，本次同时更新两端，以覆盖两个发送方向。

朋友使用 Windows 10 / 11 x64，完整解压 ZIP 后双击 `Start-Sub2API.bat`。现有客户端实现会自动读取嵌入的专用连接凭据、生成或复用当前用户独立身份、认证专用网关、监听 `127.0.0.1:18081`，就绪后打开浏览器。无需填写服务器地址、端口规则或安装开发环境。

浏览器入口为 `http://127.0.0.1:18081/`；OpenAI 兼容 API Base URL 为 `http://127.0.0.1:18081/v1`，仍需使用分配给朋友的 Sub2API 账号或 API Key。保持命令窗口运行，关闭窗口即停止转发；不安装开机自启。端口占用时明确报错，不自动改变端口。

## 制品与部署

| 项目 | 结果 |
| --- | --- |
| Windows 构建 | Rust 1.98.1，官方 `x86_64-pc-windows-gnullvm` 标准库，既有 LLVM-MinGW 工具链；release 构建成功。 |
| 分发 ZIP | 4,732,023 字节；包内仅有 `Sub2API.exe`、`Start-Sub2API.bat`、`使用说明.txt`、`LICENSE.txt`。 |
| ZIP SHA-256 | `3b352170c6656ec185352167a53643f22486e74f5c22af3af4944c44e2dbe8e8` |
| Windows EXE SHA-256 | `1a0cff219a0e55add8dfc40a91a5d08cf66f5e773fa8193ddf3868b9ee43ad82` |
| Ubuntu 网关 SHA-256 | `e3e63d79e8adabb9844235272d60cb9798bc4d0e174a60d6aff130f729f1d9ff` |
| 专用网关 | 原生 Linux release 构建；升级后 systemd 用户服务 active、enabled，运行路径与新程序一致。 |

Ubuntu 网关按原服务的 SIGINT 优雅停止，确认 PID 为 0 并取得 `gateway.lock` 后保存私有停机备份，再切换到新哈希版本目录。`gateway.json`、`identity.key` 与原安装记录在切换后先核对逐字节一致，再将安装记录更新到新二进制和构建提交；网关身份、专用凭证及授权范围不变。旧程序、旧 unit 和原安装记录保留用于回退。

本次仅重启独立专用网关，没有编辑通用 Agent 的配置、端口映射或服务导航，也没有重启 Sub2API 应用服务。

## 验证与边界

- 专用产品与恢复策略：8 passed；实际部署测试先按默认规则 ignored，随后显式单独执行，1 passed、0 ignored。
- 传输回归：26 passed，包括每六个下行 UDP 包丢一个的 2 MiB 传输及反向流量校验。
- 核心网络恢复策略：6 passed。
- 新构建的共享核心以临时独立身份连接实际 Ubuntu 专用网关，HTTP 200；服务目录只有 `sub2api`，访问 `ubuntu-admin`、`samba`、`streamdesk` 被拒绝。
- Windows 制品：PE x64、ZIP CRC、四文件白名单、BAT CRLF 和路径引号通过；DLL 导入仅为 Windows 系统组件。EXE 中的 profile 与原私有构建输入匹配，包内无单独 profile、预置身份、管理员令牌或配置文件。
- Ubuntu Sub2API `/health` 返回 200，独立网关保持 active、enabled。

未连接朋友 Windows 电脑，以上不代替 Windows 上双击 BAT、默认浏览器、休眠恢复和实际网络环境验收。包未做 Authenticode 签名；交叉链接仅报告既有的 `-no-pie` 未使用参数提示，构建成功。

ZIP 包含嵌入的共享访问凭证，只作为私下分发材料，不提交仓库或上传公开 Release。朋友仍使用独立 Sub2API 业务权限。更新朋友电脑时保留 `%LOCALAPPDATA%\Sub2API-Client`，不要复制他人身份文件。

构建、manifest、制品检查和测试日志保存于发行者私有构建目录 `~/p2p-file-builds/sub2api-refresh-20261028/`；Ubuntu 停机备份位于 `~/.local/share/p2p-file/configuration-backups/sub2api-refresh-20261028/`。完整凭据与日志不进入仓库。
