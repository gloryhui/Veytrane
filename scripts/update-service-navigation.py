#!/usr/bin/env python3
"""Refresh the standalone service navigation from installed node configurations."""
import argparse
import copy
import datetime
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent
DEFAULT_TEMPLATE = REPOSITORY_ROOT / "service-navigation.html"
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import navigation_platform


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

SUPPORTED_PROTOCOLS = {"http", "https", "tcp", "ssh", "smb", "rdp"}
NODE_ID_PATTERN = re.compile(r"[a-f0-9]{32}")


class NavigationError(ValueError):
    pass


def _node_id(value, label):
    value = str(value or "")
    if not NODE_ID_PATTERN.fullmatch(value):
        raise NavigationError(f"{label} must be a complete lowercase NodeId")
    return value


def _read_json(path, label):
    try:
        value = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise NavigationError(f"Cannot read {label}: {path}") from error
    if not isinstance(value, dict):
        raise NavigationError(f"{label} must contain a JSON object")
    return value


def _process_result(argv, *, timeout=35):
    kwargs = dict(capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout)
    if os.name == "nt":
        kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
    result = subprocess.run([str(item) for item in argv], **kwargs)
    if result.returncode:
        detail = (result.stderr or result.stdout or "no diagnostic output").strip()
        raise NavigationError(f"Command failed ({argv[0]}): {detail}")
    return result.stdout


def _response_payload(output, label):
    try:
        response = json.loads(output)
    except ValueError as error:
        raise NavigationError(f"{label} did not return valid JSON") from error
    if not isinstance(response, dict):
        raise NavigationError(f"{label} returned an invalid response")
    if response.get("error"):
        raise NavigationError(f"{label} failed: {response['error']}")
    return response.get("result", response)


def local_installation(config, node_json=None, node_cli=None):
    installed = _read_json(config["installed"], "installed.json")
    try:
        data_dir = Path(installed["data_dir"]).expanduser().resolve()
        cli = Path(node_cli) if node_cli else Path(navigation_platform.executable(config, installed, "p2p-node"))
    except (KeyError, TypeError, OSError) as error:
        raise NavigationError("installed.json does not identify the Agent binaries and data directory") from error
    node_path = Path(node_json).expanduser().resolve() if node_json else data_dir / "node.json"
    record = _read_json(node_path, "local node.json")
    settings = record.get("settings", record)
    if not isinstance(settings, dict):
        raise NavigationError("local node.json settings must be an object")
    for key in ("forwards", "services"):
        if not isinstance(settings.get(key), list):
            raise NavigationError(f"local node.json settings.{key} must be a list")
    if not isinstance(settings.get("service_ids", {}), dict):
        raise NavigationError("local node.json settings.service_ids must be an object")
    return {
        "installed": installed,
        "data_dir": data_dir,
        "node_json": node_path,
        "cli": cli,
        "settings": settings,
    }


def run_local_node(local, command, timeout=35):
    argv = [local["cli"], "--data-dir", local["data_dir"], *command]
    return _response_payload(_process_result(argv, timeout=timeout), "p2p-node " + " ".join(command))


def read_authorized_catalog(local, peer):
    result = run_local_node(local, ["services", peer], timeout=40)
    entries = result if isinstance(result, list) else result.get("services") if isinstance(result, dict) else None
    if not isinstance(entries, list):
        raise NavigationError(f"Authorized service directory for {peer} is unavailable")
    catalog = {}
    for service in entries:
        if not isinstance(service, dict) or not isinstance(service.get("id"), str) or not service["id"]:
            raise NavigationError(f"Authorized service directory for {peer} is invalid")
        catalog[service["id"]] = service.get("name") if isinstance(service.get("name"), str) else service["id"]
    return catalog


def _remote_config(config, ssh_host):
    candidate = copy.deepcopy(config)
    if candidate.get("remote") is None and ssh_host:
        candidate["remote"] = {
            "name": "Ubuntu",
            "node_id": None,
            "management_service_id": "ubuntu-admin",
            "ssh_host": ssh_host,
            "python": "python3",
            "installed": "~/.local/lib/p2p-file-node/installed.json",
        }
    elif candidate.get("remote") is not None and ssh_host:
        candidate["remote"]["ssh_host"] = ssh_host
    return candidate


def resolve_remote_node(config, settings):
    remote = config.get("remote")
    if remote is None:
        return None
    configured = remote.get("node_id")
    management_id = remote.get("management_service_id")
    mappings = {
        forward.get("peer_node_id")
        for forward in settings["forwards"]
        if management_id and settings.get("service_ids", {}).get(forward.get("id")) == management_id
    }
    if configured is not None:
        expected = _node_id(configured, "remote.node_id")
        if mappings and mappings != {expected}:
            raise NavigationError("remote.node_id does not match the management-service mapping")
        return expected
    if len(mappings) != 1:
        raise NavigationError("Cannot identify the managed remote node from remote.node_id or management_service_id")
    return _node_id(next(iter(mappings)), "managed peer node_id")


def remote_status_and_services(config, expected_node_id):
    installed = json.dumps(config["remote"]["installed"], ensure_ascii=True)
    expected = json.dumps(expected_node_id, ensure_ascii=True)
    script = f'''import json, os, subprocess, sys
from pathlib import Path
installed_path = Path(os.path.expandvars({installed})).expanduser()
installed = json.loads(installed_path.read_text(encoding="utf-8"))
cli = Path(installed["bin_dir"]) / ("p2p-node.exe" if os.name == "nt" else "p2p-node")
process = subprocess.run([str(cli), "--data-dir", installed["data_dir"], "status"],
                         capture_output=True, text=True, encoding="utf-8", timeout=30)
if process.returncode:
    print(process.stderr or process.stdout, file=sys.stderr)
    raise SystemExit(process.returncode)
response = json.loads(process.stdout)
result = response.get("result", response)
node = result.get("node") if isinstance(result, dict) else None
actual = node.get("node_id") if isinstance(node, dict) else None
actual = actual or (result.get("node_id") if isinstance(result, dict) else None)
if actual != {expected}:
    raise SystemExit("remote Agent NodeId does not match configured or management-mapped NodeId")
record = json.loads((Path(installed["data_dir"]) / "node.json").read_text(encoding="utf-8"))
settings = record.get("settings", record)
services = [dict(id=item.get("id"), target=item.get("target"), enabled=item.get("enabled"))
            for item in settings.get("services", [])]
print(json.dumps(dict(node_id=actual, services=services), ensure_ascii=False))
'''
    argv = navigation_platform.ssh_command(config, script)
    payload = _response_payload(_process_result(argv, timeout=45), "remote node status")
    actual = _node_id(payload.get("node_id"), "remote status node_id")
    if actual != expected_node_id:
        raise NavigationError("Remote Agent identity changed while reading its configuration")
    services = payload.get("services")
    if not isinstance(services, list):
        raise NavigationError("Remote node services are unavailable")
    mapped = {}
    for service in services:
        if not isinstance(service, dict) or not isinstance(service.get("id"), str) or not service["id"]:
            raise NavigationError("Remote node contains an invalid service")
        mapped[service["id"]] = service
    return actual, mapped


def run_remote_json(config, script, label, timeout=30):
    return _response_payload(
        _process_result(navigation_platform.ssh_command(config, script), timeout=timeout),
        label,
    )


def render_data(html, data):
    encoded = json.dumps(data, ensure_ascii=False, indent=2).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    html, count = re.subn(
        r'(<script id="navigation-data" type="application/json">)\s*[\s\S]*?(</script>)',
        lambda match: match.group(1) + "\n" + encoded + "\n" + match.group(2),
        html,
    )
    if count != 1:
        raise NavigationError("Template must contain exactly one navigation-data block")
    return html


def service_url(listen, protocol, path_value):
    host, port = listen.rsplit(":", 1)
    if protocol not in ("http", "https"):
        return listen
    url_host = "localhost" if protocol == "https" and host in ("127.0.0.1", "localhost", "::1", "[::1]") else host
    return f"{protocol}://{url_host}:{port}{path_value}"


def apply_presentation(services, path):
    """Reuse the local editor's display metadata; never copy credentials."""
    if not path.is_file():
        return
    presentation = _read_json(path, "presentation.json")
    for service in services:
        meta = presentation.get(service["id"], {})
        if not isinstance(meta, dict):
            raise NavigationError("presentation.json entries must be objects")
        if meta.get("peer") != service.get("peerNodeId") or meta.get("serviceId") != service.get("serviceId"):
            continue
        protocol, path_value = meta.get("protocol", "tcp"), meta.get("path", "/")
        if protocol not in SUPPORTED_PROTOCOLS:
            raise NavigationError("Unsupported local navigation protocol")
        if (not isinstance(path_value, str) or not path_value.startswith("/") or path_value.startswith("//")
                or any(character in path_value for character in ("?", "#", "\\", "\r", "\n"))):
            raise NavigationError("Invalid local navigation path")
        title, category = meta.get("title"), meta.get("category")
        if not isinstance(title, str) or not title.strip() or not isinstance(category, str) or not category.strip():
            raise NavigationError("presentation.json needs non-empty title and category")
        service.update(title=title.strip(), protocol=protocol, category=category.strip(),
                       healthPath=meta.get("healthPath", ""))
        service["url"] = service_url(service["listen"], protocol, path_value)


def _peer_info(config, peer, managed_remote):
    configured = config.get("peers", {}).get(peer, {})
    if not isinstance(configured, dict):
        raise NavigationError(f"peers.{peer} must be an object")
    remote = config.get("remote") or {}
    fallback = remote.get("name") if peer == managed_remote else None
    name = configured.get("name") or fallback or f"设备 {peer[:8]}"
    platform = str(configured.get("platform") or "unknown").lower()
    raw_targets = configured.get("targets", {})
    if not isinstance(raw_targets, dict):
        raise NavigationError(f"peers.{peer}.targets must be an object")
    targets = {}
    for service_id, target in raw_targets.items():
        if not isinstance(service_id, str) or not service_id:
            raise NavigationError(f"peers.{peer}.targets has an invalid service ID")
        if not isinstance(target, str) or not target.strip():
            raise NavigationError(f"peers.{peer}.targets.{service_id} must be a non-empty string")
        targets[service_id] = target.strip()
    return {"name": name.strip(), "platform": platform, "targets": targets}


def build_services(config, local, remote_node, remote_services):
    settings = local["settings"]
    services = []
    catalog_cache = {}
    for forward in settings["forwards"]:
        if not isinstance(forward, dict):
            raise NavigationError("local node.json contains an invalid forward")
        rule_id = forward.get("id")
        service_id = settings.get("service_ids", {}).get(rule_id)
        peer = forward.get("peer_node_id")
        listen = forward.get("listen")
        if not isinstance(rule_id, str) or not rule_id:
            raise NavigationError("local forward is missing its rule ID")
        if not isinstance(service_id, str) or not service_id:
            raise NavigationError(f"Forward {rule_id} is missing a service ID")
        peer = _node_id(peer, f"Forward {rule_id} peer_node_id")
        if not isinstance(listen, str) or ":" not in listen:
            raise NavigationError(f"Forward {rule_id} has an invalid listen address")
        peer_info = _peer_info(config, peer, remote_node)
        remote_label = "远端 " + peer_info["name"]
        remote_service = remote_services.get(service_id) if peer == remote_node else None
        if peer == remote_node:
            if remote_service is None:
                raise NavigationError(f"Managed remote service is missing: {service_id}")
            target = remote_service.get("target")
            if not isinstance(target, str) or not target:
                raise NavigationError(f"Managed remote service {service_id} has no target")
            target_source = None
            authorized_verified = None
            target_verified = True
            remote_verified = True
            enabled = bool(forward.get("enabled") and remote_service.get("enabled"))
        else:
            if peer not in catalog_cache:
                catalog_cache[peer] = read_authorized_catalog(local, peer)
            catalog = catalog_cache[peer]
            if service_id not in catalog:
                raise NavigationError(f"Service {service_id} is not in the authorized directory for {peer}")
            target = peer_info["targets"].get(service_id)
            target_source = "user-confirmed" if target else "authorized-directory"
            authorized_verified = True
            target_verified = False
            remote_verified = False
            enabled = bool(forward.get("enabled"))
        presentation = SERVICE_UI.get(service_id)
        if presentation:
            title, description, category, protocol, path_value = presentation
        else:
            title = catalog.get(service_id, forward.get("name", service_id)) if peer != remote_node else forward.get("name", service_id)
            description = (
                "已通过可信管理通道读取服务目标；协议未登记，因此仅提供 TCP 地址复制。"
                if peer == remote_node else
                "授权目录已确认服务 ID；目录不公开远端目标，且协议未登记，因此仅提供 TCP 地址复制。"
            )
            category, protocol, path_value = "其他", "tcp", ""
        services.append(dict(
            id=rule_id,
            serviceId=service_id,
            title=title,
            name=forward.get("name", service_id),
            description=description,
            category=category,
            protocol=protocol,
            url=service_url(listen, protocol, path_value),
            listen=listen,
            target=target or "远端目标未公开",
            remoteLabel=remote_label,
            peerName=peer_info["name"],
            peerPlatform=peer_info["platform"],
            peerNodeId=peer,
            targetSource=target_source,
            authorizedDirectoryVerified=authorized_verified,
            targetVerified=target_verified,
            remoteConfigVerified=remote_verified,
            enabled=enabled,
            autoStart=bool(forward.get("auto_start")),
            remote=True,
        ))
    return services


def local_admin_service(config_or_host, host=None):
    if host is None:
        host = config_or_host
    url = host["adminUrl"]
    parsed = urlsplit(url)
    listen = parsed.netloc + (parsed.path if parsed.path not in ("", "/") else "")
    platform_name = {"macos": "Mac", "windows": "Windows", "linux": "Linux"}.get(
        host["platform"], host["platform"])
    service = dict(
        id=host["localAdminId"],
        serviceId=host["localAdminId"],
        title=f"本机 {host['name']} · 节点管理",
        name=f"{host['name']} Veytrane 管理",
        description=f"配置这台 {platform_name} 的回环监听，接收其他设备提供的服务。",
        category="节点管理",
        protocol="http",
        url=url,
        listen=listen,
        target=f"本机 {host['name']} Web 管理",
        enabled=True,
        autoStart=None,
        remote=False,
        localPlatform=host["platform"],
        localName=host["name"],
    )
    return service


def read_dsh_login_token(config, target):
    installed = json.dumps(config["remote"]["installed"], ensure_ascii=True)
    target_json = json.dumps(target, ensure_ascii=True)
    script = f'''import json, os, re, subprocess
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
subprocess.run(["systemctl", "--user", "is-active", "--quiet", "dsh.service"], check=True)
invocation = subprocess.check_output(["systemctl", "--user", "show", "dsh.service", "-p", "InvocationID", "--value"], text=True).strip()
if not invocation:
    raise SystemExit("DSH current invocation is unavailable")
installed = json.loads(Path(os.path.expandvars({installed})).expanduser().read_text(encoding="utf-8"))
settings = json.loads((Path(installed["data_dir"]) / "node.json").read_text(encoding="utf-8"))["settings"]
target = {target_json}
port = int(target.rsplit(":", 1)[1])
logs = subprocess.check_output(["journalctl", "--user", "-u", "dsh.service", "_SYSTEMD_INVOCATION_ID=" + invocation, "--no-pager", "-o", "cat"], text=True)
logs = re.sub(r"\\x1b\\[[0-?]*[ -/]*[@-~]", "", logs)
for candidate in reversed(re.findall(r"https?://[^\\s]+", logs)):
    url = urlsplit(candidate)
    if url.scheme == "http" and url.hostname in ("127.0.0.1", "localhost") and url.port == port:
        token = parse_qs(url.query).get("token", [""])[0]
        if token:
            print(json.dumps({{"token": token}}))
            break
else:
    raise SystemExit("DSH current login token was not found")
'''
    value = run_remote_json(config, script, "DSH login lookup")
    if not isinstance(value.get("token"), str) or not value["token"]:
        raise NavigationError("DSH login token is unavailable")
    return value["token"]


def _read_remote_secret(config, script, label):
    value = run_remote_json(config, script, label)
    if not all(isinstance(value.get(key), str) and value[key] for key in ("username", "password")):
        raise NavigationError(f"{label} did not return username and password")
    return {"username": value["username"], "password": value["password"]}


def atomic_write(path, text, private=False):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    existing_mode = (path.stat().st_mode & 0o777) if path.is_file() else None
    fd, temporary = tempfile.mkstemp(prefix=".navigation-", dir=path.parent)
    os.close(fd)
    try:
        if private:
            navigation_platform.secure_private_file(temporary)
        with Path(temporary).open("w", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        if not private and os.name != "nt":
            temporary_path = Path(temporary)
            temporary_path.chmod(existing_mode or 0o644)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def write_private_navigation(config, html, data, *, force=False):
    private_root = Path(config["page"]).expanduser().parent
    markers = {
        "chromium": private_root / "show-chromium-login",
        "dsh": private_root / "show-dsh-login",
        "rustfs": private_root / "show-rustfs-login",
    }
    credentials_enabled = config["platform"] == "macos" and config.get("remote") is not None
    active_markers = markers if credentials_enabled else {}
    if not force and not any(path.is_file() for path in active_markers.values()):
        return False
    try:
        private_root.resolve().relative_to(REPOSITORY_ROOT)
    except ValueError:
        pass
    else:
        raise NavigationError("Private navigation must remain outside the repository")
    private = copy.deepcopy(data)
    by_id = {service.get("serviceId"): service for service in private["services"]}
    if active_markers.get("chromium", Path()).is_file() and "chromium-web" in by_id:
        script = '''from pathlib import Path
import json
path = Path.home() / "app/docker-deployments/chromium/login.credentials"
pairs = dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)
print(json.dumps({"username": pairs["USERNAME"], "password": pairs["PASSWORD"]}))
'''
        by_id["chromium-web"]["login"] = _read_remote_secret(config, script, "Chromium login lookup")
    if active_markers.get("rustfs", Path()).is_file() and "rustfs-console" in by_id:
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
        by_id["rustfs-console"]["login"] = _read_remote_secret(config, script, "RustFS login lookup")
    if active_markers.get("dsh", Path()).is_file() and "dsh" in by_id:
        service = by_id["dsh"]
        url = urlsplit(service["url"])
        query = [(key, value) for key, value in parse_qsl(url.query) if key != "token"]
        query.append(("token", read_dsh_login_token(config, service["target"])))
        service["loginUrl"] = urlunsplit((url.scheme, url.netloc, url.path, urlencode(query), url.fragment))
        service["description"] = f"直接访问远端 {service['peerName']} 的 DSH；点击打开页面即可使用本机私有 token 登录。"
    atomic_write(config["page"], render_data(html, private), private=True)
    return True


def navigation_host(config, local=None):
    host = dict(navigation_platform.host_info(config))
    if host.get("platform") == "macos":
        try:
            if local is None:
                installed = _read_json(config["installed"], "installed.json")
                data_dir = Path(installed["data_dir"]).expanduser().resolve()
            else:
                data_dir = Path(local["data_dir"]).expanduser().resolve()
            token_path = data_dir / "admin.token"
            host["tokenCommand"] = "pbcopy < " + shlex.quote(str(token_path))
        except (NavigationError, KeyError, OSError, TypeError):
            pass
    return host


def generate(config, *, output=None, node_json=None, node_cli=None, template=DEFAULT_TEMPLATE):
    local = local_installation(config, node_json=node_json, node_cli=node_cli)
    remote_node = resolve_remote_node(config, local["settings"])
    remote_services = {}
    if remote_node and any(
            forward.get("peer_node_id") == remote_node for forward in local["settings"]["forwards"]):
        verified_node, remote_services = remote_status_and_services(config, remote_node)
        remote_node = verified_node
    host = navigation_host(config, local)
    services = build_services(config, local, remote_node, remote_services)
    services.append(local_admin_service(host))
    apply_presentation(services[:-1], Path(config["state_dir"]).expanduser() / "presentation.json")
    data = {
        "updatedAt": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "host": host,
        "services": services,
    }
    template_path = Path(template)
    if not template_path.is_file():
        raise NavigationError(f"Navigation template is missing: {template_path}")
    rendered = render_data(template_path.read_text(encoding="utf-8"), data)
    output_path = Path(output or config["page"]).expanduser()
    private_path = Path(config["page"]).expanduser()
    same_path = output_path.resolve(strict=False) == private_path.resolve(strict=False)
    if same_path:
        write_private_navigation(config, rendered, data, force=True)
        print(f"Updated private navigation {output_path}: {len(services)} entries; private permissions applied")
    else:
        atomic_write(output_path, rendered)
        print(f"Updated {output_path}: {len(services)} entries; no credentials included")
        if write_private_navigation(config, rendered, data):
            print("Updated private local navigation; private permissions applied")
    return data


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh-host", help="Deprecated override for the configured independent SSH alias")
    parser.add_argument("--config", type=Path, help="Legacy local node.json path override")
    parser.add_argument("--navigation-config", type=Path, help="Cross-platform navigation JSON configuration")
    parser.add_argument("--output", type=Path, help="HTML output; defaults to config.page, or the repository template for legacy Mac use")
    parser.add_argument("--mac-web", help="Deprecated override for the local Agent web URL")
    parser.add_argument("--node-cli", type=Path, help="Installed p2p-node CLI override")
    parser.add_argument("--template", type=Path, default=DEFAULT_TEMPLATE,
                        help="Credential-free HTML template")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    try:
        config = navigation_platform.load_config(args.navigation_config)
        config = _remote_config(config, args.ssh_host)
        if args.mac_web:
            config["agent_web"] = args.mac_web
        output = args.output or (config["page"] if args.navigation_config else DEFAULT_TEMPLATE)
        generate(config, output=output, node_json=args.config, node_cli=args.node_cli, template=args.template)
    except (NavigationError, navigation_platform.PlatformError, OSError, subprocess.SubprocessError) as error:
        print(f"navigation sync failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
