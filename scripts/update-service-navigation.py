#!/usr/bin/env python3
"""Refresh the standalone local navigation from the deployed JSON configurations."""
import argparse
import datetime
import json
from pathlib import Path
import re
import subprocess

SERVICE_UI = {
    "ubuntu-admin": ("配置 Ubuntu", "管理 Ubuntu 的 P2P 节点、信任设备和端口服务。", "管理", "http", "/"),
    "sub2api": ("使用 Sub2API", "打开 Sub2API 控制台，管理账号与 API 服务。", "应用", "http", "/"),
    "streamdesk": ("打开 StreamDesk", "访问部署在 Ubuntu 内网的 StreamDesk。", "应用", "https", "/login"),
    "samba": ("Samba 文件共享", "在 Finder 中按 ⌘K，将共享地址粘贴到「连接服务器」。", "文件", "smb", ""),
}


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
        # StreamDesk's TLS certificate is issued for localhost.
        url_host = "localhost" if protocol == "https" and host in ("127.0.0.1", "localhost") else host
        url = f"{protocol}://{url_host}:{port}{path}" if protocol != "tcp" else listen
        services.append(dict(id=forward["id"], title=title, name=forward["name"], description=description,
                             category=category, protocol=protocol, url=url, listen=listen,
                             target=service["target"] if service else forward["target"],
                             enabled=bool(forward["enabled"] and (service is None or service["enabled"])),
                             autoStart=bool(forward["auto_start"]), remote=True))
    order = {"ubuntu-admin": 0, "sub2api": 1, "streamdesk": 2, "samba": 3}
    ids = local.get("service_ids", {})
    services.sort(key=lambda x: order.get(ids.get(x["id"]), 10))
    services.append(dict(id="mac-admin", title="配置本机 Mac", name="Mac P2P 管理", description="管理本机节点、映射规则及连接设置。", category="管理", protocol="http", url=args.mac_web, listen=args.mac_web.split("://", 1)[1].rstrip("/"), target="本机 P2P Web 管理", enabled=True, autoStart=None, remote=False))
    data = dict(updatedAt=datetime.datetime.now().astimezone().isoformat(timespec="seconds"), services=services)
    encoded = json.dumps(data, ensure_ascii=False, indent=2).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    html = args.output.read_text()
    html, count = re.subn(r'(<script id="navigation-data" type="application/json">)\s*[\s\S]*?(</script>)', lambda m: m[1] + "\n" + encoded + "\n" + m[2], html)
    if count != 1:
        raise ValueError("Expected exactly one navigation-data block")
    args.output.write_text(html)
    print(f"Updated {args.output}: {len(services)} entries; no credentials included")


if __name__ == "__main__":
    main()
