#!/usr/bin/env python3
"""Loopback HTTP adapter for navigation_manager; no background sync or probes."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import logging
import secrets
import signal
import threading
from pathlib import Path
from urllib.parse import urlsplit

from navigation_manager import AgentBridge, Manager, Problem


class Server(ThreadingHTTPServer):
    # Drain in-flight operations on graceful gateway replacement.
    daemon_threads = False
    allow_reuse_address = True

    def __init__(self, address, manager, origins):
        self.manager, self.origins = manager, set(origins)
        self.hosts = {urlsplit(origin).netloc for origin in origins}
        self.token = secrets.token_urlsafe(36)
        self.slots = threading.BoundedSemaphore(8)
        super().__init__(address, Handler)


class Handler(BaseHTTPRequestHandler):
    server_version = "VeytraneNavigation/1"

    def log_message(self, *_):
        # No paths, credentials, request bodies, or private query strings in logs.
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(50)

    def reply(self, status, value):
        data = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'")
        self.end_headers()
        self.wfile.write(data)

    def boundary(self, mutation=False):
        if not ipaddress.ip_address(self.client_address[0]).is_loopback or self.headers.get("Host") not in self.server.hosts:
            raise Problem("管理接口仅允许本机导航页访问。", "forbidden")
        origin = self.headers.get("Origin")
        if origin and origin not in self.server.origins:
            raise Problem("请求来源不受信任。", "forbidden")
        if self.headers.get("Sec-Fetch-Site") not in (None, "same-origin", "none"):
            raise Problem("不接受跨站管理请求。", "forbidden")
        if self.headers.get("X-Navigation-Client") != "1":
            raise Problem("请从导航页使用管理功能。", "forbidden")
        if mutation and (origin not in self.server.origins or not secrets.compare_digest(
                self.headers.get("X-Navigation-Token", ""), self.server.token)):
            raise Problem("管理会话已失效，请重新连接管理台。", "forbidden")

    def do_GET(self):
        try:
            self.boundary()
            if self.path != "/api/navigation/bootstrap":
                self.reply(404, {"error": {"message": "接口不存在"}})
                return
            self.reply(200, {"token": self.server.token, "version": 1})
        except Problem as error:
            self.reply(403, {"error": {"message": str(error), "code": error.code}})

    def do_POST(self):
        if not self.server.slots.acquire(blocking=False):
            self.reply(429, {"error": {"message": "管理请求较多，请稍后重试。"}})
            return
        try:
            self.boundary(True)
            if self.headers.get("Content-Type", "").split(";", 1)[0] != "application/json":
                raise Problem("仅接受 JSON 请求。")
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 32768 or self.headers.get("Transfer-Encoding"):
                raise Problem("请求大小超限。")
            body = json.loads(self.rfile.read(length))
            if not isinstance(body, dict):
                raise Problem("请求必须是 JSON 对象。")
            route = self.path.removeprefix("/api/navigation/")
            if not self.path.startswith("/api/navigation/"):
                raise Problem("接口不存在。")
            manager = self.server.manager
            if route == "state":
                value = manager.state(body.get("refreshRemote") is True)
            elif route == "catalog":
                value = manager.catalog(body.get("peer"))
            elif route == "preview":
                with manager.lock:
                    value = manager.preview(body)
            elif route == "diagnose":
                value = manager.diagnose(body.get("rule"))
            else:
                value = manager.mutate(route, body)
            self.reply(200, {"result": value})
        except Problem as error:
            status = 403 if error.code == "forbidden" else 409 if error.code in ("revision_conflict", "id_conflict", "port_in_use") else 400
            self.reply(status, {"error": {"message": str(error), "code": error.code,
                                         "uncertain": error.uncertain, "details": error.details}})
        except (ValueError, TypeError, KeyError, AttributeError):
            self.reply(400, {"error": {"message": "请求字段无效，请重新读取配置。", "code": "invalid_request"}})
        except Exception:
            logging.exception("Navigation request failed")
            self.reply(500, {"error": {"message": "管理请求未完成，请刷新状态确认结果。", "code": "outcome_unknown", "uncertain": True}})
        finally:
            self.server.slots.release()


def main():
    root = Path.home() / "Library/Application Support/p2p-file"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=17790)
    parser.add_argument("--ssh-host", default="ubuntu")
    parser.add_argument("--page", type=Path, default=root / "navigation/service-navigation.html")
    parser.add_argument("--state-dir", type=Path, default=root / "navigation/control")
    parser.add_argument("--installed", type=Path, default=root / "bin/installed.json")
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("port must be between 1024 and 65535")
    bridge = AgentBridge(args.installed, args.ssh_host)
    manager = Manager(bridge, args.page, args.state_dir)
    origins = [f"http://{host}:{port}" for host in ("127.0.0.1", "localhost", "[::1]") for port in (3001, args.port)]
    server = Server(("127.0.0.1", args.port), manager, origins)
    print(f"Navigation control listening on 127.0.0.1:{args.port}", flush=True)
    def stop(*_):
        threading.Thread(target=server.shutdown, daemon=True).start()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    with server:
        server.serve_forever()


if __name__ == "__main__":
    main()
