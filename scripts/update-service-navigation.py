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

SERVICE_UI = {
    "ubuntu-admin": ("远程 Ubuntu · 节点管理", "配置 Ubuntu 服务器提供的服务、目标地址和允许访问的设备。", "节点管理", "http", "/"),
    "sub2api": ("使用 Sub2API", "打开 Sub2API 控制台，管理账号与 API 服务。", "应用", "http", "/"),
    "chromium-web": ("Chromium · 远程浏览器", "在 Mac 浏览器中操作远端 Ubuntu 的 Chromium，使用原有账号登录。", "应用", "https", "/"),
    "streamdesk": ("打开 StreamDesk", "访问部署在 Ubuntu 内网的 StreamDesk。", "应用", "https", "/login"),
    "istore-web": ("iStore · 路由器管理", "管理 iStore 路由器的网络、设备与系统设置。", "路由与网络", "http", "/cgi-bin/luci/"),
    "istore-nikki": ("iStore · Nikki 代理面板", "打开 Nikki 的 zashboard 面板，查看连接、代理与规则。", "路由与网络", "http", "/ui/"),
    "samba": ("Samba 文件共享", "在 Finder 中按 ⌘K，将共享地址粘贴到「连接服务器」。", "文件", "smb", ""),
}


def render_data(html, data):
    encoded = json.dumps(data, ensure_ascii=False, indent=2).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    html, count = re.subn(r'(<script id="navigation-data" type="application/json">)\s*[\s\S]*?(</script>)', lambda m: m[1] + "\n" + encoded + "\n" + m[2], html)
    if count != 1:
        raise ValueError("Expected exactly one navigation-data block")
    return html


def write_private_navigation(args, html, data):
    # Opt-in belongs to this Mac, never to the shared repository or its HTML.
    private_root = Path.home() / "Library/Application Support/p2p-file/navigation"
    if not (private_root / "show-chromium-login").is_file():
        return
    private = dict(data, services=[dict(service) for service in data["services"]])
    chromium = next((service for service in private["services"] if service.get("serviceId") == "chromium-web"), None)
    if chromium is not None:
        script = '''from pathlib import Path
import json
path = Path.home() / "app/docker-deployments/chromium/login.credentials"
pairs = dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)
print(json.dumps({"username": pairs["USERNAME"], "password": pairs["PASSWORD"]}))
'''
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.ssh_host, "python3 -"],
                                input=script, check=True, capture_output=True, text=True, timeout=30)
        chromium["login"] = json.loads(result.stdout)
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
    print("Updated private local navigation; Chromium login is not written to the repository")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh-host", required=True, help="Ubuntu SSH alias, e.g. ubuntu")
    parser.add_argument("--config", type=Path, default=Path.home() / "Library/Application Support/p2p-file/node/node.json")
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[1] / "service-navigation.html")
    parser.add_argument("--mac-web", default="http://127.0.0.1:7789/")
    args = parser.parse_args()
    local = json.loads(args.config.read_text())["settings"]
    # The remote command emits only public routing fields, never authentication data.
    command = "python3 -c " + "'import json; from pathlib import Path; s=json.loads((Path.home()/\".local/share/p2p-file/node/node.json\").read_text())[\"settings\"]; print(json.dumps([{k:x.get(k) for k in (\"id\",\"target\",\"enabled\")} for x in s[\"services\"]]))'"
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.ssh_host, command], check=True, capture_output=True, text=True, timeout=30)
    remote = {s["id"]: s for s in json.loads(result.stdout)}
    services = []
    for forward in local["forwards"]:
        service_id = local.get("service_ids", {}).get(forward["id"])
        service = remote.get(service_id)
        if service_id and service is None:
            raise ValueError(f"Remote service missing: {service_id}; navigation was not changed")
        title, description, category, protocol, path = SERVICE_UI.get(service_id, (forward["name"], "复制本机监听地址，使用对应客户端连接。", "其他", "tcp", ""))
        listen = forward["listen"]
        host, port = listen.rsplit(":", 1)
        # Existing HTTPS services use certificates that include localhost.
        url_host = "localhost" if protocol == "https" and host in ("127.0.0.1", "localhost") else host
        url = f"{protocol}://{url_host}:{port}{path}" if protocol != "tcp" else listen
        services.append(dict(id=forward["id"], serviceId=service_id, title=title, name=forward["name"], description=description,
                             category=category, protocol=protocol, url=url, listen=listen,
                             target=service["target"] if service else forward["target"],
                             enabled=bool(forward["enabled"] and (service is None or service["enabled"])),
                             autoStart=bool(forward["auto_start"]), remote=True))
    services.append(dict(id="mac-admin", serviceId="mac-admin", title="本机 Mac · 节点管理", name="Mac Veytrane 管理", description="配置这台 Mac 的监听端口，接收 Ubuntu 等远端设备提供的服务。", category="节点管理", protocol="http", url=args.mac_web, listen=args.mac_web.split("://", 1)[1].rstrip("/"), target="本机 Veytrane Web 管理", enabled=True, autoStart=None, remote=False))
    data = dict(updatedAt=datetime.datetime.now().astimezone().isoformat(timespec="seconds"), services=services)
    html = render_data(args.output.read_text(), data)
    write_private_navigation(args, html, data)
    args.output.write_text(html)
    print(f"Updated {args.output}: {len(services)} entries; no credentials included")


if __name__ == "__main__":
    main()
