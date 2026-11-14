# 服务导航与管理台

导航 HTML 和 HTTP 管理 API 由 `p2p-agent` 同一 Rust 进程托管，macOS、Windows、Linux 统一监听本机回环 `http://127.0.0.1:3001/`。Python 只负责一次性准备配置、生成静态导航页或执行单次同步，不是常驻后端。

## 当前边界

- `p2p-agent` 负责导航 HTTP 服务、管理 API、配置 revision、预览保存和 P2P 核心重建。
- `scripts/install-navigation-control.py` 是兼容准备工具：保存集成配置并生成/保留私有页，不启动 Agent。
- `scripts/update-service-navigation.py` 是一次性同步器：读取节点配置后更新 HTML 数据块，随后退出。
- 旧独立 Python HTTP 导航后端、Nginx 路由和独立系统启动项不再属于新安装方案。旧 gateway 退役和本机真实迁移由部署主流程处理，准备工具不会停止、删除或改写其他服务。
- 安装工具不写 Agent 身份、不修改 Agent 主配置、不注册系统服务，也不重启 Agent。

2026-11-14 已在当前 Mac 完成内置导航迁移：`p2p-agent` 同一进程直接监听 IPv4/IPv6 回环 3001，旧 Python 导航启动项已停用，导航专用 Nginx 配置已移出加载范围，其他 Nginx 站点继续运行。迁移前后本机节点配置与身份文件逐字节一致；两端配置版本、设置内容和节点身份一致，Mac 26 条、Ubuntu 1 条转发均监听且 peer 就绪。已实读两端服务目录，并通过既有 Ubuntu 管理映射取得 HTTP 200。

Windows、Linux 共用这套 Rust 实现，目前提供隔离逻辑验证，尚未在这两个系统完成内置导航实机部署。迁移前的二进制、启动配置和私有文件保留在本机私有备份中；仓库不包含这些身份、凭据或完整私有配置。

## 配置位置

Agent 的 `installed.json` 提供实际 `data_dir`。升级后的 Rust Agent 默认读取：

```text
<data_dir 的父目录>/navigation/config.json
```

标准资产布局：

```text
<data_dir 父目录>/navigation/
  config.json
  service-navigation.html
  control/
```

也可以显式启动 Agent：

```sh
p2p-agent --data-dir <data_dir> --navigation-config <配置文件绝对路径>
```

`--no-navigation` 可关闭可选导航，即使默认配置已存在也只启动 P2P 节点。显式指定一个尚不存在的导航配置文件时，Rust 使用该路径所在目录的默认值生成私有页面，无需 Python HTTP 后端。

本文和示例使用标准默认布局。准备工具只允许 `config.json`、私有页和状态目录位于 `<data_dir 父目录>/navigation/` 内，拒绝跳出该目录的路径、符号链接和不可归属文件。

三个平台默认：

```json
{
  "http": {
    "port": 3001,
    "public_port": 3001
  }
}
```

旧 Mac 配置中的 `17790 / 3001` 在后端和公开端口同时出现时会归一为 `3001 / 3001`。新 Rust 解析器使用同一迁移规则。

## 配置格式

不要填写顶层 `platform`；运行时操作系统决定 `macos`、`windows` 或 `linux`。`navigation_root` 和 `config_file` 也是运行时字段。

```json
{
  "schema_version": 1,
  "local_name": "设备显示名",
  "installed": "/绝对路径/installed.json",
  "page": "/绝对路径/navigation/service-navigation.html",
  "state_dir": "/绝对路径/navigation/control",
  "http": {
    "port": 3001,
    "public_port": 3001
  },
  "remote": null,
  "peers": {
    "22222222222222222222222222222222": {
      "name": "Office Windows",
      "platform": "windows",
      "targets": {
        "windows-rdp": "192.0.2.10:3389"
      }
    }
  },
  "lifecycle": {
    "kind": "none",
    "service": null,
    "definition": null
  },
  "agent_web": "http://127.0.0.1:7789/"
}
```

- `installed`：本机 Agent 安装元数据。`data_dir` 决定默认导航目录。
- `page`：集成 Agent 返回的私有 HTML 路径，必须在标准导航目录内。
- `state_dir`：预览、展示设置和操作状态等私有状态目录。
- `remote`：可选可信 SSH 管理通道。为 `null` 时使用本机 Agent 的只读授权服务目录。
- `peers[完整NodeId].targets`：只属于该设备的用户确认目标，按服务 ID 配置。配置目标标记 `targetSource=user-confirmed`，但保持 `targetVerified=false`，直到通过可信管理通道实读远端配置。
- `agent_web`：另一个显式运行的 `p2p-node web` 网关，不是导航服务。三平台默认填写 `http://127.0.0.1:7789/`；需要另行运行网关，实际地址以配置为准。

示例配置：

- [`macos.json`](../examples/navigation/macos.json)
- [`windows.json`](../examples/navigation/windows.json)
- [`linux.json`](../examples/navigation/linux.json)

## 准备内置导航

macOS：

```sh
python3 scripts/install-navigation-control.py \
  --config /绝对路径/navigation.json
```

Windows PowerShell：

```powershell
py -3 .\scripts\install-navigation-control.py `
  --config C:\绝对路径\navigation.json
```

Linux：

```sh
python3 scripts/install-navigation-control.py \
  --config /绝对路径/navigation.json
```

准备工具会：

1. 读取 `installed.json`，确认当前用户的 Agent 数据目录。
2. 校验导航端口为 `3001`，配置、页面和状态路径都在 `data_dir 父目录/navigation/`。
3. 将不含运行时字段的配置私有写入标准 `config.json`。
4. 以单次子进程运行 `update-service-navigation.py` 生成私有页。
5. 同步失败但已有可用私有页时保留旧页；首次失败时生成仅包含本机节点入口的私有页。

成功输出示例：

```json
{
  "integrated": true,
  "prepared": true,
  "requires_agent_restart": true,
  "agent_started": false,
  "agent_restarted": false,
  "config": "/.../navigation/config.json",
  "page": "/.../navigation/service-navigation.html",
  "page_source": "remote-or-authorized-catalog",
  "port": 3001
}
```

`requires_agent_restart=true` 表示需要由 Agent 宿主升级/启动流程加载内置导航。准备工具只设置该状态，不执行重启。

兼容参数：

- `--replace`：保留旧调用兼容；当前不启动、不重启 Agent，也不删除旧 gateway。
- `--no-autostart`：保留旧调用兼容；当前安装器本来就不会注册或启动任何宿主。
- `--foreground`：与 `--no-autostart` 等价，只表示兼容；不创建 runtime，不运行 Python HTTP 服务。
- `--installed`、`--page`、`--state-dir`：准备工具的一次性路径覆盖；最终仍受标准导航目录归属校验。
- `--ssh-host`：覆盖非空 `remote.ssh_host`。

该 Python 工具只用于过渡。原生安装/配置 CLI 就绪后可替换脚本，输出契约仍使用 `integrated`、`requires_agent_restart`、`config` 和 `page`。

## 单次同步

映射或展示信息变化后运行：

```sh
python3 scripts/update-service-navigation.py \
  --navigation-config /绝对路径/navigation/config.json \
  --output /绝对路径/navigation/service-navigation.html
```

现有 Mac 兼容入口仍可使用：

```sh
python3 scripts/update-service-navigation.py --ssh-host ubuntu
```

同步器选项：

- `--navigation-config PATH`：跨平台配置；省略时使用平台默认配置。
- `--output PATH`：HTML 输出路径；省略时使用 `config.page`。
- `--template PATH`：无凭据 HTML 模板，默认使用仓库 `service-navigation.html`。
- `--config PATH`：旧调用兼容，覆盖本机 `node.json`。
- `--node-cli PATH`：覆盖本机 `p2p-node`。
- `--ssh-host HOST`：兼容旧调用，覆盖独立 SSH 别名。

`--output` 与 `config.page` 相同时，同步器先完成可选凭据读取，再以私有权限一次性原子替换。凭据读取失败时旧私有页保持不变。

启用 Mac 私有登录标记后，Chromium、RustFS 登录信息和当前 DSH token 只写入私有页。POSIX 权限为 `0600`，Windows 使用当前用户 ACL；仓库模板不带真实凭据。

## 管理操作

三个平台从 `http://127.0.0.1:3001/` 使用管理功能。HTML 和 API 都由 Agent 的 Rust 宿主直接提供，HTTP 只监听本机回环。

- **配置/预览**：选择已配对设备和已发布服务，填写本机回环监听。预览保留 10 分钟，并在保存前重新检查两端 revision、服务授权和端口。
- **保存**：只应用预览中明确选择的本地规则和受管理远端服务变化。远端先提交、本机后提交；本机确定失败时尝试恢复远端，结果未知时必须刷新核对。
- **单条重启**：只停止并重新启动所选转发，保留启用、自动启动、其他规则和 Agent 进程。
- **设备重连**：断开并重新认证指定 peer；该设备的全部转发可能短暂中断，完成请求不等于认证已成功，需刷新确认。
- **整 Agent 重启**：由 Rust 宿主在原进程中重建 P2P 核心，HTTP 导航保持可用；全部本机 P2P 转发和活动流会短暂中断。不会重启远端 Agent、容器或业务服务，也不需要外部 Python 辅助进程。
- **状态语义**：启用、自动启动和监听只表示配置或运行状态，不证明应用协议、登录或业务功能健康。

监控默认关闭，只在页面可见且有焦点时前台轮询；不启用后台同步或整页刷新。

## 启动与日志

导航不再拥有独立启动项。Agent 的既有系统启动方式继续负责：

- macOS：launchd LaunchAgent。
- Linux：systemd 用户服务；是否使用 linger 由现有 Agent 运维决定。
- Windows：当前用户计划任务。

准备工具不安装、替换或控制这些启动项。导航日志随 Agent 宿主输出，不再有 `p2p-file-navigation-control` Python 服务日志、`navigation-server.py` runtime 或独立 stdout/stderr 文件。

配置、私有页和状态位于标准导航目录。每次同步都通过临时文件原子替换；准备失败时不会以不带原凭据的页面覆盖已有私有页。

## 验证

Rust 导航回归与原有 Agent 进程回归：

```sh
cargo test --lib navigation
cargo test --test node_agent_process \
  agent_process_config_transaction_idempotency_restart_and_ownership
cargo build --release --bin p2p-agent --bin p2p-node
python3 scripts/navigation-native-smoke.py --bin-dir target/release
```

原生 smoke 使用独立临时身份、节点目录和随机回环端口启动真实 Rust Agent，验证同进程页面/API、单条转发幂等、预览保存，以及核心重建时 HTTP 保持可用、身份和保存配置不变。它不使用已安装 Agent、不连接真实 peer 或 SSH。Python 在这里仅作为测试驱动，运行中的网页服务始终是 Rust。

离线回归：

```sh
python3 scripts/navigation-install-tests.py
python3 scripts/navigation-platform-tests.py
python3 scripts/navigation-sync-tests.py
python3 -m py_compile scripts/install-navigation-control.py \
  scripts/navigation_platform.py \
  scripts/update-service-navigation.py
git diff --check
```

测试使用临时安装元数据、临时节点配置和模拟同步结果，不启动 Python HTTP、Nginx 或 Agent，不访问真实节点。覆盖默认端口、旧 Mac 端口归一、标准路径归属、私有写入、私有页失败保留、兼容参数和未实例化独立宿主。
