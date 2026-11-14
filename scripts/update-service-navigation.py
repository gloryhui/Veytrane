#!/usr/bin/env python3
"""Refresh the standalone local navigation from the deployed JSON configurations."""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

SERVICE_UI = {
    "ubuntu-admin": ("远程 Ubuntu · 节点管理", "配置 Ubuntu 服务器提供的服务、目标地址和允许访问的设备。", "节点管理", "http", "/"),
    "ubuntu-redis": ("Ubuntu · Redis", "在 Redis 客户端中连接下方 TCP 地址；导航不提供连接凭据。", "应用", "tcp", ""),
    "ubuntu-mysql": ("Ubuntu · MySQL", "在 MySQL 客户端中以远端 Ubuntu 地址和映射端口建立 TCP 连接；导航不提供连接凭据。", "应用", "tcp", ""),
    "ubuntu-postgresql": ("Ubuntu · PostgreSQL", "在 PostgreSQL 客户端中以远端 Ubuntu 地址和映射端口建立 TCP 连接；导航不提供连接凭据。", "应用", "tcp", ""),
    "sub2api": ("使用 Sub2API", "打开 Sub2API 控制台，管理账号与 API 服务。", "应用", "http", "/"),
    "chromium-web": ("Chromium · 远程浏览器", "在 Mac 浏览器中操作远端 Ubuntu 的 Chromium，使用原有账号登录。", "应用", "https", "/"),
    "streamdesk-srs-api": ("StreamDesk V2 · SRS API", "主后端使用的独立 SRS 管理接口，需管理鉴权，不是前端控制台。", "应用", "http", "/api/v1/versions"),
    "streamdesk-srs-hls": ("StreamDesk V2 · HLS 源站", "主后端授权代理使用的内网 HLS 源站；普通观看应从 StreamDesk 页面进入。", "应用", "http", "/"),
    "streamdesk-srs-rtmp": ("StreamDesk V2 · SRS RTMP", "OBS 使用的 RTMP TCP 地址，通过 P2P 到远端 Ubuntu 的 SRS；推流地址和密钥从 StreamDesk 获取。", "应用", "tcp", ""),
    "streamdesk-media": ("StreamDesk · 媒体服务 API", "主 Spring Boot 的文件、检测与录播执行接口；需服务密钥鉴权，无网页登录控制台。", "应用", "http", "/worker/v1/capabilities"),
    "streamdesk": ("打开 StreamDesk", "访问部署在 Ubuntu 内网的 StreamDesk。", "应用", "https", "/login"),
    "istore-web": ("iStore · 路由器管理", "管理 iStore 路由器的网络、设备与系统设置。", "路由与网络", "http", "/cgi-bin/luci/"),
    "istore-nikki": ("iStore · Nikki 代理面板", "打开 Nikki 的 zashboard 面板，查看连接、代理与规则。", "路由与网络", "http", "/ui/"),
    "samba": ("Samba 文件共享", "在 Finder 中按 ⌘K，将共享地址粘贴到「连接服务器」。", "文件", "smb", ""),
    "ssh": ("Ubuntu · SSH 终端", "在终端执行 ssh ubuntu-local，通过本机端口登录远端 Ubuntu。", "路由与网络", "ssh", ""),
    "nginx-share": ("Nginx · 文件共享", "访问远端 Ubuntu 的 Nginx 文件共享服务。", "文件", "http", "/"),
    "rustfs-console": ("RustFS · 对象存储管理", "打开远端 Ubuntu 的 RustFS Web 控制台，使用原有账号登录并管理对象存储。", "应用", "http", "/rustfs/console/"),
    "netdata": ("Netdata · 服务器监控", "查看远端 Ubuntu 上的 Netdata 监控面板。", "应用", "http", "/"),
    "codex-web": ("Codex Web · AI 工作台", "直接访问远端 Ubuntu 的 Codex Web 原始端口 8214。", "应用", "http", "/"),
    "dsh": ("DSH · DeepSeek Harness", "直接访问远端 Ubuntu 的 DSH 原始端口 3080；首次访问需使用 DSH 启动时生成的 token 链接登录。", "应用", "http", "/"),
    "rustdesk-win": ("Windows · RustDesk", "在 Mac RustDesk 中填写本机连接地址。Windows 的服务发布和授权尚待配置核对。", "应用", "tcp", ""),
    "windows-rdp": ("Windows · 远程桌面", "在 Mac 的 Windows App 中添加电脑，电脑名称填写下方连接地址；登录时使用 Windows 账户。", "应用", "rdp", ""),
    "wsl-llama": ("Windows · WSL llama.cpp", "OpenAI 兼容 API，可使用 /v1 作为 base URL。", "应用", "http", "/v1/models"),
    "streamdesk-wsl-media": ("Windows · StreamDesk WSL Media Worker", "媒体 Worker 能力接口，需服务密钥；Windows 发布目标由用户提供，未读取 Windows 私有配置。", "应用", "http", "/worker/v1/capabilities"),
    "streamdesk-wsl-srs-api": ("Windows · WSL SRS 管理 API", "主服务使用的 SRS 管理接口，需独立鉴权；Windows 发布目标由用户提供。", "应用", "http", "/api/v1/versions"),
    "streamdesk-wsl-srs-hls": ("Windows · WSL SRS HLS 源站", "主服务代理使用的 HLS 源站；空目录可返回 404，观看应经过 StreamDesk 保护。", "应用", "http", "/"),
}


def read_windows_catalog(args, peer):
    # The service directory exposes IDs/names only, never Worker credentials or targets.
    cli = args.node_cli
    if cli is None:
        installed = json.loads((args.config.parent.parent / "bin/installed.json").read_text())
        cli = Path(installed["bin_dir"]) / "p2p-node"
    result = subprocess.run([str(cli), "--data-dir", str(args.config.parent), "services", peer],
                            check=True, capture_output=True, text=True, timeout=40)
    response = json.loads(result.stdout)
    if response.get("error") or not isinstance(response.get("result"), list):
        raise ValueError("Windows authorized service directory is unavailable")
    return {service["id"] for service in response["result"]}


def render_data(html, data):
    encoded = json.dumps(data, ensure_ascii=False, indent=2).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    html, count = re.subn(r'(<script id="navigation-data" type="application/json">)\s*[\s\S]*?(</script>)', lambda m: m[1] + "\n" + encoded + "\n" + m[2], html)
    if count != 1:
        raise ValueError("Expected exactly one navigation-data block")
    return html


def apply_presentation(services, path):
    """Reuse the local web editor's display metadata; never copy credentials."""
    if not path.is_file():
        return
    presentation = json.loads(path.read_text())
    for service in services:
        meta = presentation.get(service["id"], {})
        if meta.get("peer") != service.get("peerNodeId") or meta.get("serviceId") != service.get("serviceId"):
            continue
        protocol, path_value = meta.get("protocol", "tcp"), meta.get("path", "/")
        if protocol not in ("http", "https", "tcp", "ssh", "smb", "rdp"):
            raise ValueError("Unsupported local navigation protocol")
        if not path_value.startswith("/") or path_value.startswith("//") or any(c in path_value for c in ("?", "#", "\\", "\r", "\n")):
            raise ValueError("Invalid local navigation path")
        service.update(title=meta["title"], protocol=protocol, category=meta["category"], healthPath=meta.get("healthPath", ""))
        service["url"] = f"{protocol}://{service['listen']}{path_value if protocol in ('http', 'https') else ''}" if protocol != "tcp" else service["listen"]


def read_dsh_login_token(args):
    # Only inspect the currently running service invocation, never an older token.
    script = '''import json, re, subprocess
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
subprocess.run(["systemctl", "--user", "is-active", "--quiet", "dsh.service"], check=True)
invocation = subprocess.check_output(["systemctl", "--user", "show", "dsh.service", "-p", "InvocationID", "--value"], text=True).strip()
if not invocation:
    raise SystemExit("DSH current invocation is unavailable")
settings = json.loads((Path.home() / ".local/share/p2p-file/node/node.json").read_text())["settings"]
target = next(service["target"] for service in settings["services"] if service["id"] == "dsh")
port = int(target.rsplit(":", 1)[1])
logs = subprocess.check_output(["journalctl", "--user", "-u", "dsh.service", "_SYSTEMD_INVOCATION_ID=" + invocation, "--no-pager", "-o", "cat"], text=True)
logs = re.sub(r"\\x1b\\[[0-?]*[ -/]*[@-~]", "", logs)
for candidate in reversed(re.findall(r"https?://[^\\s]+", logs)):
    url = urlsplit(candidate)
    if url.scheme == "http" and url.hostname in ("127.0.0.1", "localhost") and url.port == port:
        token = parse_qs(url.query).get("token", [""])[0]
        if token:
            print(json.dumps({"token": token}))
            break
else:
    raise SystemExit("DSH current login token was not found")
'''
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.ssh_host, "python3 -"],
                            input=script, check=True, capture_output=True, text=True, timeout=30)
    return json.loads(result.stdout)["token"]


def write_private_navigation(args, html, data):
    # Opt-in belongs to this Mac, never to the shared repository or its HTML.
    private_root = Path.home() / "Library/Application Support/p2p-file/navigation"
    show_chromium = (private_root / "show-chromium-login").is_file()
    show_dsh = (private_root / "show-dsh-login").is_file()
    show_rustfs = (private_root / "show-rustfs-login").is_file()
    if not (show_chromium or show_dsh or show_rustfs):
        return
    private = dict(data, services=[dict(service) for service in data["services"]])
    chromium = next((service for service in private["services"] if service.get("serviceId") == "chromium-web"), None)
    if show_chromium and chromium is not None:
        script = '''from pathlib import Path
import json
path = Path.home() / "app/docker-deployments/chromium/login.credentials"
pairs = dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)
print(json.dumps({"username": pairs["USERNAME"], "password": pairs["PASSWORD"]}))
'''
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.ssh_host, "python3 -"],
                                input=script, check=True, capture_output=True, text=True, timeout=30)
        chromium["login"] = json.loads(result.stdout)
    rustfs = next((service for service in private["services"] if service.get("serviceId") == "rustfs-console"), None)
    if show_rustfs and rustfs is not None:
        script = '''import json, subprocess
container = json.loads(subprocess.check_output(["docker", "inspect", "rustfs"]))[0]
if not container["State"]["Running"]:
    raise SystemExit("RustFS container is not running")
env = dict(item.split("=", 1) for item in container["Config"].get("Env", []) if "=" in item)
username = env.get("RUSTFS_ACCESS_KEY")
password = env.get("RUSTFS_SECRET_KEY")
if not username or not password:
    raise SystemExit("RustFS login configuration is unavailable")
print(json.dumps({"username": username, "password": password}))
'''
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.ssh_host, "python3 -"],
                                input=script, check=True, capture_output=True, text=True, timeout=30)
        rustfs["login"] = json.loads(result.stdout)
    dsh = next((service for service in private["services"] if service.get("serviceId") == "dsh"), None)
    if show_dsh and dsh is not None:
        url = urlsplit(dsh["url"])
        query = [(key, value) for key, value in parse_qsl(url.query) if key != "token"]
        query.append(("token", read_dsh_login_token(args)))
        dsh["loginUrl"] = urlunsplit((url.scheme, url.netloc, url.path, urlencode(query), url.fragment))
        dsh["description"] = "直接访问远端 Ubuntu 的 DSH 原始端口 3080；点击打开页面即可使用本机私有 token 登录。"
    rendered = render_data(html, private)
    # The private copy is outside the repository and is replaced with mode 0600.
    fd, temporary = tempfile.mkstemp(prefix=".navigation-", dir=private_root)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(rendered)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, private_root / "service-navigation.html")
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    print("Updated private local navigation; credentials are not written to the repository")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh-host", required=True, help="Ubuntu SSH alias, e.g. ubuntu")
    parser.add_argument("--config", type=Path, default=Path.home() / "Library/Application Support/p2p-file/node/node.json")
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[1] / "service-navigation.html")
    parser.add_argument("--mac-web", default="http://127.0.0.1:7789/")
    parser.add_argument("--node-cli", type=Path, help="Installed p2p-node CLI; defaults to this Mac's installation metadata")
    args = parser.parse_args()
    local = json.loads(args.config.read_text())["settings"]
    # The remote command emits only public routing fields, never authentication data.
    command = "python3 -c " + "'import json; from pathlib import Path; s=json.loads((Path.home()/\".local/share/p2p-file/node/node.json\").read_text())[\"settings\"]; print(json.dumps([{k:x.get(k) for k in (\"id\",\"target\",\"enabled\")} for x in s[\"services\"]]))'"
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.ssh_host, command], check=True, capture_output=True, text=True, timeout=30)
    remote = {s["id"]: s for s in json.loads(result.stdout)}
    ubuntu_peers = {f["peer_node_id"] for f in local["forwards"]
                    if local.get("service_ids", {}).get(f["id"]) == "ubuntu-admin"}
    if len(ubuntu_peers) != 1:
        raise ValueError("Cannot identify the Ubuntu peer from its management mapping")
    windows_peer = "5eef2d53912e77479fa2e61ef3aa27c3"
    windows_media_ids = {"streamdesk-wsl-media", "streamdesk-wsl-srs-api", "streamdesk-wsl-srs-hls"}
    required_windows_ids = {local.get("service_ids", {}).get(f["id"]) for f in local["forwards"]
                            if f["peer_node_id"] == windows_peer
                            and local.get("service_ids", {}).get(f["id"]) in (windows_media_ids | {"windows-rdp"})}
    if required_windows_ids and not required_windows_ids <= read_windows_catalog(args, windows_peer):
        raise ValueError("Windows services are missing from the authorized directory; navigation was not changed")
    services = []
    for forward in local["forwards"]:
        service_id = local.get("service_ids", {}).get(forward["id"])
        ubuntu = forward["peer_node_id"] in ubuntu_peers
        # Never look up another node's service ID in Ubuntu's service catalog.
        pending_windows = (forward["peer_node_id"] == windows_peer
                           and service_id in ({"rustdesk-win", "wsl-llama", "windows-rdp"} | windows_media_ids))
        if not ubuntu and not pending_windows:
            raise ValueError(f"No verified service source for peer mapping: {forward['id']}")
        service = remote.get(service_id) if ubuntu else None
        if ubuntu and service_id and service is None:
            raise ValueError(f"Remote service missing: {service_id}; navigation was not changed")
        title, description, category, protocol, path = SERVICE_UI.get(service_id, (forward["name"], "复制本机监听地址，使用对应客户端连接。", "其他", "tcp", ""))
        listen = forward["listen"]
        host, port = listen.rsplit(":", 1)
        # Existing HTTPS services use certificates that include localhost.
        url_host = "localhost" if protocol == "https" and host in ("127.0.0.1", "localhost") else host
        url = f"{protocol}://{url_host}:{port}{path}" if protocol not in {"tcp", "rdp"} else listen
        windows_target = {
            "rustdesk-win": "127.0.0.1:21118（计划目标，待 Windows 核对）",
            # Confirmed by the Windows owner; the authenticated catalog exposes
            # only ID/name, so do not claim its target or full ACL was read here.
            "windows-rdp": "127.0.0.1:3389",
            "wsl-llama": "wsl-llama（Windows 目标地址未公开）",
            "streamdesk-wsl-media": "127.0.0.1:18097（Windows 端提供，未读取配置）",
            "streamdesk-wsl-srs-api": "127.0.0.1:21985（Windows 端提供，未读取配置）",
            "streamdesk-wsl-srs-hls": "127.0.0.1:28098（Windows 端提供，未读取配置）",
        }.get(service_id)
        services.append(dict(id=forward["id"], serviceId=service_id, title=title, name=forward["name"], description=description,
                             category=category, protocol=protocol, url=url, listen=listen,
                             target=service["target"] if service else windows_target,
                             remoteLabel="远端 Ubuntu" if ubuntu else "远端 Windows",
                             peerNodeId=forward["peer_node_id"],
                             targetSource="user-confirmed" if service_id == "windows-rdp" and pending_windows else None,
                             authorizedDirectoryVerified=service_id in required_windows_ids if pending_windows else None,
                             targetVerified=not pending_windows,
                             enabled=bool(forward["enabled"] and (service is None or service["enabled"])),
                             autoStart=bool(forward["auto_start"]), remote=True))
    services.append(dict(id="mac-admin", serviceId="mac-admin", title="本机 Mac · 节点管理", name="Mac Veytrane 管理", description="配置这台 Mac 的监听端口，接收 Ubuntu 等远端设备提供的服务。", category="节点管理", protocol="http", url=args.mac_web, listen=args.mac_web.split("://", 1)[1].rstrip("/"), target="本机 Veytrane Web 管理", enabled=True, autoStart=None, remote=False))
    apply_presentation(services, Path.home() / "Library/Application Support/p2p-file/navigation/control/presentation.json")
    data = dict(updatedAt=datetime.datetime.now().astimezone().isoformat(timespec="seconds"), services=services)
    html = render_data(args.output.read_text(), data)
    write_private_navigation(args, html, data)
    args.output.write_text(html)
    print(f"Updated {args.output}: {len(services)} entries; no credentials included")


if __name__ == "__main__":
    main()
