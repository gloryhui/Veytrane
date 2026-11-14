#!/usr/bin/env python3
"""Local navigation control plane. Owns no tunnels, identities, or Agent config files.

All node mutations use the installed Agent's versioned management API. Ubuntu is
managed over an independently configured SSH alias, even when P2P is unavailable.
"""
import copy
import datetime
import hashlib
import http.client
import ipaddress
import json
import os
import plistlib
from pathlib import Path
import re
import shlex
import socket
import ssl
import subprocess
import threading
import time
import uuid


class Problem(Exception):
    def __init__(self, message, code="invalid_request", uncertain=False, details=None):
        super().__init__(message)
        self.code, self.uncertain, self.details = code, uncertain, details


def stamp():
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def write_private(path, value):
    """Atomic private state; never overwrite Agent configuration or key material."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Exclusive, task-owned temporary file. A failed replace is left for inspection.
    temporary = path.parent / (".navigation-" + uuid.uuid4().hex)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        stream.write(value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, indent=2))
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def address(value, loopback=False):
    try:
        if not isinstance(value, str) or len(value) > 100:
            raise ValueError()
        host, port = value.rsplit(":", 1)
        ip = ipaddress.ip_address(host.strip("[]"))
        port = int(port)
        if not 0 < port < 65536 or ip.is_unspecified or ip.is_multicast:
            raise ValueError()
        if loopback and not ip.is_loopback:
            raise ValueError()
        return str(ip), port
    except ValueError:
        raise Problem("请填写有效的" + ("回环监听地址" if loopback else "目标 IP:端口") + "，例如 127.0.0.1:18080")


def run_json(argv, request=None):
    try:
        result = subprocess.run(argv, input=json.dumps(request) if request is not None else None,
                                capture_output=True, text=True, timeout=40)
    except subprocess.TimeoutExpired:
        raise Problem("管理请求超时，请刷新状态确认结果，不要重复提交。", "outcome_unknown", True)
    try:
        value = json.loads(result.stdout)
    except ValueError:
        raise Problem("管理通道未返回有效响应，请检查 Agent 或独立 SSH 连接。", "agent_unavailable", True)
    if value.get("error"):
        error = value["error"]
        code = error.get("code", "agent_error")
        raise Problem(error.get("message", "Agent 拒绝了操作"), code,
                      code in ("outcome_unknown", "agent_unavailable", "recovery_required"))
    if result.returncode:
        raise Problem("管理进程未正常完成，请刷新确认。", "outcome_unknown", True)
    return value.get("result", value)


# Fixed remote program: no browser-provided shell, filename, host, or executable.
REMOTE_REQUEST = '''import json, subprocess, sys
from pathlib import Path
i=json.loads((Path.home()/".local/lib/p2p-file-node/installed.json").read_text())
p=subprocess.run([i["bin_dir"]+"/p2p-node","--data-dir",i["data_dir"],"request","-"],input=sys.stdin.read(),text=True,capture_output=True,timeout=35)
sys.stdout.write(p.stdout)
sys.exit(p.returncode)
'''


class AgentBridge:
    def __init__(self, installed, ssh_host):
        self.installed, self.ssh_host = Path(installed), ssh_host

    def installation(self):
        return json.loads(self.installed.read_text())

    def call(self, command, remote=False, request_id=None):
        request = dict(api_version=1, request_id=request_id or uuid.uuid4().hex,
                       timeout_ms=30000, command=command)
        if remote:
            argv = ["/usr/bin/ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8",
                    self.ssh_host, "python3 -c " + shlex.quote(REMOTE_REQUEST)]
        else:
            i = self.installation()
            argv = [str(Path(i["bin_dir"]) / "p2p-node"), "--data-dir", i["data_dir"], "request", "-"]
        return run_json(argv, request)

    def target_probe(self, service):
        # Probe the ID's *current saved target*, not a browser-supplied destination.
        script = '''import json, socket, sys
from pathlib import Path
s=json.loads((Path.home()/".local/share/p2p-file/node/node.json").read_text())["settings"]
service_id=json.loads(sys.stdin.read())
v=next(x for x in s["services"] if x["id"]==service_id)
host,port=v["target"].rsplit(":",1)
try:
 c=socket.create_connection((host.strip("[]"),int(port)),3);c.close();print(json.dumps({"reachable":True,"target":v["target"]}))
except OSError:
 print(json.dumps({"reachable":False,"target":v["target"]}))
'''
        return run_json(["/usr/bin/ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8",
                         self.ssh_host, "python3 -c " + shlex.quote(script)], service)

    def restart(self):
        """Graceful stop, prove exclusive ownership released, then ask launchd to start."""
        i = self.installation()
        lifecycle = Path.home() / "Library/LaunchAgents/io.github.gloryhui.p2p-agent.plist"
        try:
            args = plistlib.loads(lifecycle.read_bytes())["ProgramArguments"]
            matches = (args[0] == str(Path(i["bin_dir"]) / "p2p-agent") and
                       args[args.index("--data-dir") + 1] == i["data_dir"])
        except (OSError, ValueError, KeyError, IndexError):
            matches = False
        if not matches:
            raise Problem("已安装的 Agent 与 launchd 启动配置不一致，已拒绝重启。", "restart_host_mismatch")
        cli = [str(Path(i["bin_dir"]) / "p2p-node"), "--data-dir", i["data_dir"]]
        try:
            before = self.call({"method": "status"}).get("epoch")
            self.call({"method": "stop"})
        except Problem as error:
            before = None
            if error.code not in ("agent_unavailable",):
                raise
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            result = subprocess.run(cli + ["ownership-status"], capture_output=True, text=True, timeout=5)
            if result.returncode == 0:
                break
            time.sleep(.3)
        else:
            raise Problem("Agent 尚未释放配置/身份锁，已停止重启流程。请检查节点日志。", "restart_blocked", True)
        result = subprocess.run(["/bin/launchctl", "kickstart",
                                 f"gui/{os.getuid()}/io.github.gloryhui.p2p-agent"],
                                capture_output=True, text=True, timeout=10)
        if result.returncode:
            raise Problem("launchd 未能启动 Agent，请检查已安装的节点服务。", "restart_failed", True)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            try:
                status = self.call({"method": "status"})
                if status.get("epoch") != before and status.get("core_running"):
                    return {"message": "Mac Agent 已重新启动；请刷新确认各设备连接和应用状态。", "epoch": status["epoch"]}
            except Problem:
                pass
            time.sleep(.4)
        raise Problem("已请求启动 Agent，尚未确认就绪。请刷新状态确认。", "outcome_unknown", True)


class Manager:
    def __init__(self, bridge, page, state_dir):
        self.bridge, self.page, self.root = bridge, Path(page), Path(state_dir)
        self.lock = threading.Lock()
        self.plans = {}
        self.receipts_path = self.root / "operations.json"
        self.meta_path = self.root / "presentation.json"
        self.receipts = json.loads(self.receipts_path.read_text()) if self.receipts_path.exists() else {}
        self.metadata = json.loads(self.meta_path.read_text()) if self.meta_path.exists() else {}
        self.remote = None
        self.remote_error = None

    def baseline(self):
        match = re.search(r'<script id="navigation-data" type="application/json">([\s\S]*?)</script>', self.page.read_text())
        if not match:
            raise Problem("导航数据缺失，请重新同步导航页。")
        return json.loads(match[1])

    @staticmethod
    def ubuntu_peer(settings):
        peers = {r["peer_node_id"] for r in settings["forwards"]
                 if settings["service_ids"].get(r["id"]) == "ubuntu-admin"}
        if len(peers) != 1:
            raise Problem("无法唯一识别 Ubuntu 节点，请保留 Ubuntu 管理服务映射。")
        return peers.pop()

    @staticmethod
    def known_peers(settings):
        return ({r["peer_node_id"] for r in settings["forwards"]} |
                {d["node_id"] for d in settings.get("trusted_devices", [])})

    def remote_config(self):
        try:
            expected = self.ubuntu_peer(self.local_config()["settings"])
            actual = self.bridge.call({"method": "status"}, remote=True)["node"]["node_id"]
            if actual != expected:
                raise Problem("独立 SSH 连接的节点身份与 Ubuntu 映射不一致，已拒绝操作。", "identity_mismatch")
            self.remote = self.bridge.call({"method": "config"}, remote=True)
            self.remote_error = None
            return self.remote
        except Problem as error:
            self.remote_error = str(error)
            raise

    def local_config(self):
        return self.bridge.call({"method": "config"})

    def navigation(self, config):
        baseline = self.baseline()
        old = {s["id"]: s for s in baseline["services"]}
        settings = config["settings"]
        ubuntu = self.ubuntu_peer(settings)
        remote = {s["id"]: s for s in self.remote["settings"]["services"]} if self.remote else {}
        services = []
        for rule in settings["forwards"]:
            sid = settings["service_ids"].get(rule["id"], "")
            prior = old.get(rule["id"], {})
            same = prior.get("peerNodeId") == rule["peer_node_id"] and prior.get("serviceId") == sid
            s = dict(prior) if same else {}
            server = remote.get(sid) if rule["peer_node_id"] == ubuntu else None
            meta = self.metadata.get(rule["id"], {})
            if meta.get("peer") != rule["peer_node_id"] or meta.get("serviceId") != sid:
                meta = {}
            protocol = meta.get("protocol", s.get("protocol", "tcp"))
            path = meta.get("path", self.url_path(s.get("url", "")))
            s.update(id=rule["id"], serviceId=sid, peerNodeId=rule["peer_node_id"],
                     name=rule["name"], title=meta.get("title") or s.get("title") or rule["name"],
                     description=s.get("description", "在此管理端口映射或复制地址，使用对应客户端连接。"),
                     listen=rule["listen"], enabled=rule["enabled"] and (server is None or server["enabled"]),
                     ruleEnabled=rule["enabled"], healthPath=meta.get("healthPath", ""),
                     autoStart=rule["auto_start"], remote=True, protocol=protocol,
                     category=meta.get("category", s.get("category", "其他")),
                     remoteLabel="远端 Ubuntu" if rule["peer_node_id"] == ubuntu else s.get("remoteLabel", "远端设备"),
                     target=server["target"] if server else s.get("target", "未核实"),
                     targetVerified=server is not None, targetCurrent=server is not None and self.remote_error is None)
            s["url"] = f"{protocol}://{rule['listen']}{path if protocol in ('http', 'https') else ''}" if protocol != "tcp" else rule["listen"]
            if prior.get("target") != s["target"]:
                # Previously displayed credentials belong to the previously verified target.
                s.pop("login", None)
                s.pop("loginUrl", None)
            elif s.get("loginUrl"):
                from urllib.parse import urlsplit, urlunsplit
                login = urlsplit(s["loginUrl"])
                s["loginUrl"] = urlunsplit((protocol, rule["listen"], path, login.query, ""))
            services.append(s)
        services.extend(s for s in baseline["services"] if not s.get("remote"))
        return {"updatedAt": stamp(), "services": services}

    @staticmethod
    def url_path(url):
        from urllib.parse import urlsplit
        u = urlsplit(url)
        return u.path or "/"

    def state(self, refresh_remote=False):
        errors = []
        try:
            config = self.local_config()
        except Problem as error:
            return {"checkedAt": stamp(), "navigation": self.baseline(), "agent": {"available": False},
                    "errors": [str(error)], "history": self.history()}
        try:
            status = self.bridge.call({"method": "status"})
        except Problem as error:
            status = {}
            errors.append(str(error))
        if refresh_remote:
            try:
                self.remote_config()
            except Problem as error:
                errors.append("Ubuntu 独立管理通道：" + str(error))
        s = config["settings"]
        ubuntu = self.ubuntu_peer(s)
        peers = (status.get("node") or {}).get("peers", {})
        old = self.baseline()["services"]
        nodes = []
        for peer in sorted(self.known_peers(s)):
            known = next((x for x in old if x.get("peerNodeId") == peer), {})
            label = "Ubuntu" if peer == ubuntu else ("Windows" if "Windows" in (known.get("remoteLabel", "") + known.get("title", "")) else peer[:8])
            p = peers.get(peer, {})
            nodes.append({"id": peer, "label": label, "phase": p.get("phase", "unknown"),
                          "rttMs": round(p.get("transport", {}).get("rtt_us", 0) / 1000, 1) if p.get("transport") else None,
                          "editableTarget": peer == ubuntu,
                          "mappings": sum(r["peer_node_id"] == peer for r in s["forwards"])})
        return {"checkedAt": stamp(), "navigation": self.navigation(config), "revision": config["revision"],
                "agent": {"available": bool(status), "running": status.get("core_running", False),
                          "epoch": status.get("epoch"), "activeStreams": status.get("forwards", {}).get("active_streams", 0),
                          "lastError": status.get("last_error"), "diagnostics": status.get("diagnostics", []),
                          "signal": (status.get("node") or {}).get("signal")},
                "rules": status.get("forwards", {}).get("rules", []), "nodes": nodes,
                "remoteConfigAvailable": self.remote_error is None and self.remote is not None,
                "errors": errors, "history": self.history()}

    def catalog(self, peer):
        config = self.local_config()
        ubuntu = self.ubuntu_peer(config["settings"])
        known = self.known_peers(config["settings"])
        if peer not in known:
            raise Problem("请选择已有映射的设备。")
        if peer == ubuntu:
            remote = self.remote_config()
            node_id = self.bridge.call({"method": "status"})["node"]["node_id"]
            return {"editableTarget": True, "services": [dict(s, authorized=node_id in s["allowed_peers"])
                    for s in remote["settings"]["services"]], "revision": remote["revision"]}
        services = self.bridge.call({"method": "services", "peer": peer})
        return {"editableTarget": False, "services": services}

    def check_port(self, settings, rule, previous):
        host, port = address(rule["listen"], True)
        if any(r["id"] != rule["id"] and address(r["listen"], True) == (host, port) for r in settings["forwards"]):
            raise Problem("监听端口已被另一条映射使用。", "port_in_use")
        if previous and previous["listen"] == rule["listen"]:
            return
        family = socket.AF_INET6 if ":" in host else socket.AF_INET
        with socket.socket(family, socket.SOCK_STREAM) as listener:
            try:
                listener.bind((host, port))
            except OSError:
                raise Problem("Mac 监听端口已被占用，请换一个端口。", "port_in_use")

    def preview(self, form):
        self.plans = {k: v for k, v in self.plans.items() if v["expires"] > time.monotonic()}
        if len(self.plans) >= 32:
            raise Problem("待确认草稿过多，请稍后重试。")
        config = self.local_config()
        settings = copy.deepcopy(config["settings"])
        existing_id = form.get("id")
        previous = next((r for r in settings["forwards"] if r["id"] == existing_id), None)
        if existing_id and previous is None:
            raise Problem("映射已被其他入口移除，请刷新后重试。", "revision_conflict")
        if form.get("remove") is True:
            if previous is None or settings["service_ids"].get(existing_id) == "ubuntu-admin":
                raise Problem("不能移除 Ubuntu 管理入口或不存在的映射。")
            settings["forwards"] = [r for r in settings["forwards"] if r["id"] != existing_id]
            settings["service_ids"].pop(existing_id)
            token = uuid.uuid4().hex
            self.plans[token] = dict(expires=time.monotonic() + 600, config=config, settings=settings,
                                    remote_before=None, remote_after=None, rule=previous, previous=previous, meta=None)
            return dict(plan=token, changes=[f"Mac：移除 {previous['name']}（{previous['listen']}）的映射。"],
                        warnings=["仅移除这条 Mac 转发；远端服务和其他映射保留。"], ruleId=existing_id,
                        localRevision=config["revision"], remoteRevision=None)
        name, peer, sid = (str(form.get(k, "")).strip() for k in ("name", "peer", "serviceId"))
        if not name or len(name) > 128 or not re.fullmatch(r"[a-f0-9]{32}", peer) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", sid):
            raise Problem("请填写名称、已有设备和有效服务 ID（字母、数字、点、横线、下划线）。")
        if peer not in self.known_peers(settings):
            raise Problem("请选择已有映射的设备。")
        if sid == "ubuntu-admin" and peer != self.ubuntu_peer(settings):
            raise Problem("Ubuntu 管理服务 ID 只能用于原 Ubuntu 节点。")
        host, port = address(form.get("listen"), True)
        listen = f"[{host}]:{port}" if ":" in host else f"{host}:{port}"
        for key in ("enabled", "autoStart"):
            if type(form.get(key)) is not bool:
                raise Problem("启用和自动启动必须是布尔值。")
        rule = dict(previous) if previous else {"id": "nav-" + uuid.uuid4().hex, "target": "127.0.0.1:1"}
        rule.update(name=name, peer_node_id=peer, listen=listen, enabled=form["enabled"], auto_start=form["autoStart"])
        self.check_port(settings, rule, previous)
        old_sid = settings["service_ids"].get(existing_id)
        if old_sid == "ubuntu-admin" and (sid != old_sid or peer != previous["peer_node_id"]):
            raise Problem("请保留 Ubuntu 管理服务的设备与服务 ID。")
        protocol = form.get("protocol", "tcp")
        path = form.get("path", "/").strip() or "/"
        health_path = form.get("healthPath", "").strip()
        category = form.get("category", "其他")
        if protocol not in ("http", "https", "tcp", "ssh", "smb", "rdp") or category not in ("节点管理", "路由与网络", "应用", "文件", "其他"):
            raise Problem("访问协议或分组不受支持。")
        for p in (path, health_path):
            if p and (not p.startswith("/") or p.startswith("//") or len(p) > 512 or any(ord(c) < 32 or ord(c) > 126 for c in p) or "\\" in p or "?" in p or "#" in p):
                raise Problem("路径应为 / 开头的 ASCII 路径，不含域名、查询参数或凭据。")
        meta = {"peer": peer, "serviceId": sid, "title": name, "protocol": protocol,
                "path": path, "category": category, "healthPath": health_path}
        ubuntu = self.ubuntu_peer(settings)
        remote_before, remote_after = None, None
        changes, warnings = [], []
        if peer == ubuntu:
            remote_before = self.remote_config()
            remote_after = copy.deepcopy(remote_before["settings"])
            service = next((s for s in remote_after["services"] if s["id"] == sid), None)
            target = str(form.get("target", "")).strip()
            if target:
                ip, target_port = address(target)
                target = f"[{ip}]:{target_port}" if ":" in ip else f"{ip}:{target_port}"
            if service is None:
                if not target:
                    raise Problem("创建 Ubuntu 服务时请填写目标 IP:端口。")
                node_id = self.bridge.call({"method": "status"})["node"]["node_id"]
                service = dict(id=sid, name=name, target=target, enabled=True, allowed_peers=[node_id])
                remote_after["services"].append(service)
                changes.append(f"Ubuntu：创建服务 {sid} → {target}，仅授权这台 Mac。")
            else:
                node_id = self.bridge.call({"method": "status"})["node"]["node_id"]
                if node_id not in service["allowed_peers"]:
                    raise Problem("Ubuntu 服务尚未授权这台 Mac，请在节点管理中核实授权。")
                if not service["enabled"]:
                    raise Problem("Ubuntu 服务已禁用，请先在节点管理中启用。")
                if target and target != service["target"]:
                    if sid == "ubuntu-admin":
                        raise Problem("Ubuntu 节点管理目标请在原节点管理页修改。")
                    changes.append(f"Ubuntu：{sid} 的目标 {service['target']} → {target}。")
                    warnings.append(f"这是共享服务目标，将影响获授权的 {len(service['allowed_peers'])} 台设备之后建立的连接。")
                    service["target"] = target
        else:
            catalog = self.bridge.call({"method": "services", "peer": peer})
            if sid not in {s["id"] for s in catalog}:
                raise Problem("服务未出现在该设备的授权目录中。请先在远端发布并授权。")
            if form.get("target"):
                raise Problem("此设备暂不支持从本页修改服务目标，请选择已授权服务。")
        if previous:
            settings["forwards"] = [rule if r["id"] == existing_id else r for r in settings["forwards"]]
            for field, label in (("name", "名称"), ("listen", "Mac 监听"), ("peer_node_id", "远端设备"), ("enabled", "启用"), ("auto_start", "自动启动")):
                if previous[field] != rule[field]:
                    changes.append(f"Mac {label}：{previous[field]} → {rule[field]}。")
            if old_sid != sid:
                changes.append(f"Mac 服务：{old_sid} → {sid}。")
        else:
            settings["forwards"].append(rule)
            changes.append(f"Mac：新增 {listen} → {sid}。")
        settings["service_ids"][rule["id"]] = sid
        if previous and (previous["listen"] != listen or previous["peer_node_id"] != peer or old_sid != sid or not rule["enabled"]):
            warnings.append("本条规则的新连接会使用新设置；其他未修改的映射保留。")
        if not changes:
            changes.append("仅更新导航展示设置；Agent 配置保持当前版本。")
        token = uuid.uuid4().hex
        self.plans = {k: v for k, v in self.plans.items() if v["expires"] > time.monotonic()}
        if len(self.plans) >= 32:
            raise Problem("待确认草稿过多，请稍后重试。")
        self.plans[token] = dict(expires=time.monotonic() + 600, config=config, settings=settings,
                                 remote_before=remote_before, remote_after=remote_after, rule=rule,
                                 previous=previous, meta=meta)
        return dict(plan=token, changes=changes, warnings=warnings, ruleId=rule["id"],
                    localRevision=config["revision"], remoteRevision=remote_before["revision"] if remote_before else None)

    def apply(self, token, operation):
        plan = self.plans.get(token)
        if not plan or plan["expires"] < time.monotonic():
            raise Problem("预览已过期，请重新检查配置。", "preview_expired")
        current = self.local_config()
        if current["revision"] != plan["config"]["revision"]:
            raise Problem("Mac 配置已变更，请重新预览；草稿仍保留。", "revision_conflict")
        self.check_port(current["settings"], plan["rule"], plan["previous"])
        before = plan["remote_before"]
        remote_changed = before is not None and before["settings"] != plan["remote_after"]
        if before is not None:
            now = self.remote_config()
            if now["revision"] != before["revision"]:
                raise Problem("Ubuntu 配置已变更，请重新预览；草稿仍保留。", "revision_conflict")
        remote_result = None
        if remote_changed:
            remote_result = self.bridge.call({"method": "apply_config", "expected_revision": before["revision"],
                                              "settings": plan["remote_after"]}, True, operation + "-remote")
        try:
            if current["settings"] != plan["settings"]:
                local_result = self.bridge.call({"method": "apply_config", "expected_revision": current["revision"],
                                                "settings": plan["settings"]}, request_id=operation + "-local")
            else:
                local_result = {"revision": current["revision"]}
        except Problem as error:
            if remote_result:
                if error.uncertain:
                    raise Problem("Ubuntu 已保存，Mac 结果尚未确认。请刷新两端配置后重新预览，未自动回滚。",
                                  "partial_unknown", True, {"remoteRevision": remote_result["revision"]})
                try:
                    self.bridge.call({"method": "apply_config", "expected_revision": remote_result["revision"],
                                      "settings": before["settings"]}, True, operation + "-rollback")
                except Problem:
                    raise Problem("Mac 保存失败，Ubuntu 回滚未确认。请刷新两端配置核对。", "partial_unknown", True)
                raise Problem("Mac 保存失败，Ubuntu 配置已恢复。原因：" + str(error), error.code)
            raise
        if plan["meta"] is None:
            self.metadata.pop(plan["rule"]["id"], None)
        else:
            self.metadata[plan["rule"]["id"]] = plan["meta"]
        try:
            write_private(self.meta_path, self.metadata)
            if remote_result:
                self.remote = {"revision": remote_result["revision"], "settings": plan["remote_after"]}
            self.persist_page({"revision": local_result["revision"], "settings": plan["settings"]})
        except OSError:
            raise Problem("Agent 配置已保存，导航文件更新失败。请刷新状态确认映射，勿重复提交。", "navigation_write_failed", True)
        self.plans.pop(token, None)
        warnings = [r["durability_warning"] for r in (remote_result, local_result) if r and r.get("durability_warning")]
        return {"message": "此映射已移除，远端服务保留。" if plan["meta"] is None else "配置已保存，导航已更新。", "ruleId": plan["rule"]["id"],
                "revision": local_result["revision"], "warnings": warnings}

    def persist_page(self, config):
        nav = self.navigation(config)
        encoded = json.dumps(nav, ensure_ascii=False, indent=2).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
        html, count = re.subn(r'(<script id="navigation-data" type="application/json">)[\s\S]*?(</script>)',
                              lambda m: m[1] + "\n" + encoded + "\n" + m[2], self.page.read_text())
        if count != 1:
            raise Problem("配置已保存，导航数据块异常。请刷新状态。", "navigation_write_failed", True)
        write_private(self.page, html)

    def history(self):
        return [{"id": key, "action": v["action"], "at": v["at"], "state": v["state"],
                 "subject": v.get("subject", ""),
                 "message": v.get("result", {}).get("message") or v.get("error", {}).get("message", "执行结果未确认，请刷新状态。")}
                for key, v in list(self.receipts.items())[-15:]][::-1]

    def mutate(self, action, body):
        operation = body.get("operation", "")
        if not re.fullmatch(r"[a-f0-9]{32}", operation):
            raise Problem("操作 ID 无效，请刷新页面。")
        digest = hashlib.sha256(json.dumps([action, body], sort_keys=True).encode()).hexdigest()
        with self.lock:
            receipt = self.receipts.get(operation)
            if receipt:
                if receipt["digest"] != digest:
                    raise Problem("操作 ID 已被其他请求使用。", "id_conflict")
                if receipt["state"] == "complete":
                    return receipt["result"]
                e = receipt.get("error", {})
                raise Problem(e.get("message", "上次操作结果未确认，请刷新状态后再决定下一步。"),
                              e.get("code", "outcome_unknown"), e.get("uncertain", True))
            if action not in ("apply", "start", "stop", "restart_forward", "reconnect", "restart_agent"):
                raise Problem("不支持此操作。")
            # Retain bounded receipts. Existing pending operations remain visible.
            if len(self.receipts) >= 300:
                oldest = next((k for k, v in self.receipts.items() if v["state"] != "pending"), None)
                if oldest:
                    self.receipts.pop(oldest)
                else:
                    raise Problem("未确认的操作过多，请先核对操作记录。")
            subject = body.get("rule") or body.get("peer") or "Mac Agent"
            if action == "apply":
                rule = self.plans.get(body.get("plan"), {}).get("rule", {})
                subject = (rule.get("name", "配置草稿") + " · " + rule.get("listen", "")).strip(" ·")
            self.receipts[operation] = {"action": action, "at": stamp(), "digest": digest, "state": "pending", "subject": subject}
            write_private(self.receipts_path, self.receipts)
            try:
                result = self.apply(body.get("plan"), operation) if action == "apply" else self.action(action, body, operation)
                self.receipts[operation].update(state="complete", result=result)
            except Problem as error:
                self.receipts[operation].update(state="unknown" if error.uncertain else "failed",
                    error={"message": str(error), "code": error.code, "uncertain": error.uncertain})
                write_private(self.receipts_path, self.receipts)
                raise
            write_private(self.receipts_path, self.receipts)
            return result

    def action(self, action, body, operation):
        config = self.local_config() if action != "restart_agent" else None
        if action == "restart_agent":
            if body.get("confirm") != "restart_all":
                raise Problem("请确认整台 Mac Agent 重启会影响全部转发。")
            return self.bridge.restart()
        settings = config["settings"]
        if action == "reconnect":
            peer = body.get("peer")
            if peer not in {r["peer_node_id"] for r in settings["forwards"]} or body.get("confirm") != "reconnect_peer":
                raise Problem("请确认要重新连接的设备及影响范围。")
            self.bridge.call({"method": "disconnect", "peer": peer}, request_id=operation + "-disconnect")
            self.bridge.call({"method": "connect", "peer": peer, "password": None}, request_id=operation + "-connect")
            return {"message": "已请求重新连接设备；请刷新状态确认认证结果。"}
        rule = next((r for r in settings["forwards"] if r["id"] == body.get("rule")), None)
        if rule is None:
            raise Problem("映射不存在，请刷新状态。")
        if action == "restart_forward" and body.get("confirm") != "restart_rule":
            raise Problem("请确认重启此条转发。")
        if action != "stop" and not rule["enabled"]:
            raise Problem("规则已禁用，请先在配置中启用。")
        if action in ("stop", "restart_forward"):
            self.bridge.call({"method": "stop_forward", "rule": rule["id"]}, request_id=operation + "-stop")
        if action in ("start", "restart_forward"):
            self.bridge.call({"method": "start_forward", "rule": rule["id"]}, request_id=operation + "-start")
        return {"message": {"start": "本条转发已启动。", "stop": "本条转发已停止；保存的启用/自动启动配置未改变。",
                             "restart_forward": "本条转发已重新启动。"}[action]}

    def diagnose(self, rule_id):
        config = self.local_config()
        rule = next((r for r in config["settings"]["forwards"] if r["id"] == rule_id), None)
        if rule is None:
            raise Problem("映射不存在，请刷新状态。")
        status = self.bridge.call({"method": "status"})
        runtime = next((r for r in status.get("forwards", {}).get("rules", []) if r["id"] == rule_id), {})
        result = {"checkedAt": stamp(), "rule": rule_id, "listening": runtime.get("listening", False),
                  "peerReady": runtime.get("peer_ready", False), "lastError": runtime.get("last_error"),
                  "target": None, "tunnel": None, "http": None}
        ubuntu = self.ubuntu_peer(config["settings"])
        sid = config["settings"]["service_ids"][rule_id]
        if rule["peer_node_id"] == ubuntu:
            try:
                result["target"] = self.bridge.target_probe(sid)
            except Problem as error:
                result["targetError"] = str(error)
        if not result["listening"] or not result["peerReady"]:
            return result
        host, port = address(rule["listen"], True)
        service = next(s for s in self.navigation(config)["services"] if s["id"] == rule_id)
        health_path = service.get("healthPath", "")
        path = health_path or self.url_path(service["url"])
        if service["protocol"] in ("http", "https"):
            connection = None
            try:
                if service["protocol"] == "https":
                    # Known local tunnel targets commonly use private/self-signed certificates.
                    connection = http.client.HTTPSConnection(host, port, timeout=6, context=ssl._create_unverified_context())
                else:
                    connection = http.client.HTTPConnection(host, port, timeout=6)
                connection.request("GET", path, headers={"Connection": "close", "User-Agent": "Veytrane-Navigation/1"})
                response = connection.getresponse()
                code = response.status
                result["http"] = {"status": code, "path": path, "healthCheck": bool(health_path),
                                  "healthy": 200 <= code < 300 if health_path else None,
                                  "tlsVerified": False if service["protocol"] == "https" else None}
                result["tunnel"] = True
            except (OSError, http.client.HTTPException):
                result["tunnel"] = False
                result["probeError"] = "映射没有返回有效 HTTP 响应，可能是目标服务、协议或 P2P 通道异常。"
            finally:
                if connection:
                    connection.close()
        else:
            # A successful local accept alone cannot prove the remote TCP stream opened.
            result["probeNote"] = "非 HTTP 服务：仅显示监听、P2P 和独立目标检测，未验证应用协议。"
        return result
