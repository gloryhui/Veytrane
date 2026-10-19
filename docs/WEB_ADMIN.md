# 本机 Web 管理页

CLI 的 `serve` / `forward` 可选启动一个**仅监听 loopback** 的单页管理界面，用浏览器
增删改持久化规则，不需要手改 JSON 或发送 `SIGHUP`。JSON 文件仍是持久化源；Web 页
只是在运行时编辑并立即应用它。

## 启动

```bash
# Ubuntu 服务端：目标授权
p2p_file serve \
  --signal 203.0.113.7:7000 \
  --config /etc/p2p_file/service.json \
  --admin-listen 127.0.0.1:7788

# macOS/Linux 客户端：本机转发规则
p2p_file forward \
  --signal 203.0.113.7:7000 \
  --config ~/.config/p2p_file/client.json \
  --admin-listen 127.0.0.1:7789
```

启动后终端会打印：

```text
Web 管理页: http://127.0.0.1:7788/
管理 token 文件: /etc/p2p_file/service.json.admin-token
```

浏览器打开该地址，输入 token 文件内容即可。token 只保存在当前标签页的
`sessionStorage`，不会写进 URL 或日志。可以用 `--admin-token-file` 指定其他路径；
默认路径是配置文件同目录的 `<配置文件名>.admin-token`。

服务端使用 Web 管理时必须提供 `--config`，否则没有可持久化的目标授权文件。

## 页面能力

服务端（`serve`）：

- 查看/编辑 `forward_targets`：ID、名称、目标 `IP:端口`、授权 NodeId、启用/停用；
- 新增、删除、保存并立即应用；
- 显示当前对端、运行中的目标和最近错误。

客户端（`forward`）：

- 查看/编辑 `rules`：ID、名称、本机监听 `IP:端口`、远端目标、对端 NodeId、启用/自动启动；
- 新增、删除、改目标（不重新绑定端口）；
- 显示已经绑定的本机监听地址和最近错误。

页面上的“从文件重载”对应重新读取 JSON 并应用；“保存并应用”会先校验、再应用
运行态、最后原子写回文件。校验或应用失败时不会覆盖原文件，并会尝试回滚运行态。
Web 保存要求至少保留一条启用规则/目标，避免产生重启后无法确定对端的配置；
需要临时撤销全部授权时仍可用 `SIGHUP` 或直接停止服务。

## 安全边界

- `--admin-listen` 只接受 `127.0.0.1` 或 `::1`；传入其他地址会直接报错。
- 远程机器请用 SSH 隧道，不要把管理端口暴露到局域网/公网：

  ```bash
  ssh -L 7788:127.0.0.1:7788 ubuntu@server
  # 然后浏览器访问 http://127.0.0.1:7788/
  ```

- 所有 `/api/v1/*` 请求都必须带 `Authorization: Bearer <token>`。
- 校验 `Host` 的主机名只能是 `127.0.0.1`、`localhost` 或 `[::1]`（允许任意本地端口，
  方便 SSH 隧道映射到不同端口）；带 `Origin` 时必须与 Host 同源，防止 DNS rebinding
  和跨站请求。
- 不返回 `Access-Control-Allow-Origin`，不支持跨域。
- 请求体上限 256 KiB；页面不显示 token、私钥或密码。
- token 文件首次创建为 `0600`（Unix）；已有文件权限过宽会打警告。
- 当前不支持多用户、TLS 或公网绑定；这些需要额外设计。

`forward` 的管理页在首次 P2P 链路建立后启动；如果对端暂时离线，先按原 CLI 方式
启动或等待重连成功后再打开页面。`serve` 的管理页在等待对端上线前就已启动。

## systemd 示例

```ini
[Service]
ExecStart=/usr/local/bin/p2p_file serve \
  --signal 203.0.113.7:7000 \
  --config /etc/p2p_file/service.json \
  --admin-listen 127.0.0.1:7788
```

服务运行用户需要对 `service.json` 和 `service.json.admin-token` 有读写权限。
修改后仍可用 `SIGHUP` 从命令行热加载；Web 页和 SIGHUP 共用同一份配置。

## HTTP API

除 `GET /` 外都需要 token：

```text
GET  /api/v1/status     # 角色、对端、配置路径、运行态监听、最近错误
GET  /api/v1/config     # role/peer/config_path + 当前 JSON 配置
PUT  /api/v1/config     # 校验、应用、写回（body 为 ServiceConfig 或 ClientConfig）
POST /api/v1/reload     # 重新读取文件并应用
```

错误格式：

```json
{
  "error": "规则应用失败，已尝试保留原规则",
  "details": ["规则 a：监听 127.0.0.1:8080 失败：address already in use"]
}
```

## 与 CLI/SIGHUP 的关系

- Web 页、JSON 文件、`SIGHUP` 三者操作同一份配置；
- Web 保存会立即写回文件，之后重启进程仍能恢复；
- 服务端换授权对端需要重启，Web 会拒绝并提示；客户端换共享对端也需要重启；
- 当前 `serve` 仍是单对端/进程，Web 不改变这一限制。

## 限制与后续

- Windows 上原子替换文件有 fallback；配置校验在替换前完成，但替换本身不是原子操作；
- 页面是单个内嵌 HTML，没有 Node/Vite 构建链；
- 暂不支持连接列表、实时日志、导入导出、多用户或远程 TLS；
- 真实浏览器交互和不同平台字体/布局仍需人工验收。
