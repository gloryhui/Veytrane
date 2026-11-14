#!/usr/bin/env python3
"""Machine configuration and OS adapters for the shared navigation controller.

Only owner-managed configuration is accepted here. Browser requests cannot choose
executables, service definitions, SSH hosts or filesystem paths.
"""
import base64
import functools
import hashlib
import json
import ntpath
import os
from pathlib import Path
import platform
import plistlib
import re
import shlex
import shutil
import subprocess


class PlatformError(ValueError):
    pass


def system_name():
    try:
        return {"Darwin": "macos", "Windows": "windows", "Linux": "linux"}[platform.system()]
    except KeyError:
        raise PlatformError("导航管理台支持 macOS、Windows 和 Linux。")


def default_config():
    system = system_name()
    home = Path.home()
    if system == "macos":
        root = home / "Library/Application Support/p2p-file"
        installed = root / "bin/installed.json"
        lifecycle = {"kind": "launchd", "service": "io.github.gloryhui.p2p-agent",
                     "definition": str(home / "Library/LaunchAgents/io.github.gloryhui.p2p-agent.plist")}
    elif system == "windows":
        root = Path(os.environ.get("LOCALAPPDATA", home / "AppData/Local")) / "p2p-file"
        installed = root / "bin/installed.json"
        lifecycle = {"kind": "scheduled-task", "service": None, "definition": None}
    else:
        root = Path(os.environ.get("XDG_DATA_HOME", home / ".local/share")) / "p2p-file"
        installed = home / ".local/lib/p2p-file-node/installed.json"
        lifecycle = {"kind": "systemd", "service": "p2p-file-agent.service",
                     "definition": str(Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")) /
                                       "systemd/user/p2p-file-agent.service")}
    navigation = root / "navigation"
    # Preserve the original Mac installation. Other hosts start with no assumed
    # remote management channel; their authorized peer catalogs still work.
    remote = {"name": "Ubuntu", "node_id": None, "management_service_id": "ubuntu-admin",
              "ssh_host": "ubuntu", "python": "python3",
              "installed": "~/.local/lib/p2p-file-node/installed.json"} if system == "macos" else None
    return {"schema_version": 1, "platform": system,
            "local_name": {"macos": "Mac", "windows": "Windows", "linux": "Linux"}[system],
            "installed": str(installed), "page": str(navigation / "service-navigation.html"),
            "state_dir": str(navigation / "control"), "navigation_root": str(navigation),
            "http": {"port": 3001, "public_port": 3001},
            "remote": remote, "peers": {}, "lifecycle": lifecycle,
            "agent_web": "http://127.0.0.1:7789/"}


def load_config(path=None):
    value = default_config()
    target = Path(path).expanduser() if path else Path(value["navigation_root"]) / "config.json"
    if path or target.exists():
        source = json.loads(target.read_text(encoding="utf-8"))
        if not isinstance(source, dict) or source.get("schema_version", 1) != 1:
            raise PlatformError("导航配置 schema_version 必须为 1。")
        unknown = set(source) - (set(value) - {"platform", "navigation_root"})
        if unknown:
            raise PlatformError("未知导航配置项：" + ", ".join(sorted(unknown)))
        for key, item in source.items():
            if key in ("http", "lifecycle") or (key == "remote" and item is not None):
                if not isinstance(item, dict):
                    raise PlatformError(key + " 必须为配置对象。")
                base = value.get(key) or default_remote()
                value[key] = {**base, **item}
            else:
                value[key] = item
    # The former Mac split gateway used a 17790 backend behind the 3001 front
    # proxy. The integrated Agent serves HTML and API directly on 3001.
    http = value["http"]
    if http.get("port") == 17790 and http.get("public_port") == 3001:
        http["port"] = 3001
        http["public_port"] = 3001
    for key in ("installed", "page", "state_dir"):
        if not isinstance(value[key], str) or not value[key]:
            raise PlatformError(key + " 必须为有效路径。")
        candidate = Path(os.path.expandvars(value[key])).expanduser()
        if not candidate.is_absolute():
            candidate = target.resolve().parent / candidate
        value[key] = str(candidate.absolute())
    value["navigation_root"] = str(Path(value["page"]).parent)
    value["config_file"] = str(target.absolute())
    if not isinstance(value["local_name"], str) or not value["local_name"].strip() or len(value["local_name"]) > 64:
        raise PlatformError("local_name 应为 1 到 64 字符的设备名称。")
    value["local_name"] = value["local_name"].strip()
    for key in ("port", "public_port"):
        port = value["http"].get(key)
        if type(port) is not int or not 1024 <= port <= 65535:
            raise PlatformError("导航 HTTP 端口应在 1024 到 65535 之间。")
    expected_kind = {"macos": "launchd", "windows": "scheduled-task", "linux": "systemd"}[value["platform"]]
    if value["lifecycle"].get("kind") not in (expected_kind, "none"):
        raise PlatformError("节点启动方式与当前系统不匹配。")
    service = value["lifecycle"].get("service")
    if service is not None and (not isinstance(service, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,180}", service)):
        raise PlatformError("节点系统服务名称无效。")
    if value["lifecycle"].get("definition"):
        p = Path(os.path.expandvars(value["lifecycle"]["definition"])).expanduser()
        value["lifecycle"]["definition"] = str(p if p.is_absolute() else target.resolve().parent / p)
    if not isinstance(value["peers"], dict):
        raise PlatformError("peers 必须为节点 ID 到设备信息的配置对象。")
    for peer, item in value["peers"].items():
        if not re.fullmatch(r"[a-f0-9]{32}", peer) or not isinstance(item, dict) or not isinstance(item.get("name"), str) or not item["name"].strip():
            raise PlatformError("peers 需要完整节点 ID 和非空 name。")
        if item.get("platform", "unknown") not in ("macos", "windows", "linux", "unknown"):
            raise PlatformError("peers.platform 应为 macos、windows、linux 或 unknown。")
        targets = item.get("targets", {})
        if not isinstance(targets, dict) or any(
                not isinstance(service_id, str) or not service_id or
                not isinstance(address, str) or not address.strip() or len(address) > 512
                for service_id, address in targets.items()):
            raise PlatformError("peers.targets 应为服务 ID 到已确认目标地址的配置对象。")
    remote = value["remote"]
    if remote is not None:
        if not isinstance(remote, dict) or not remote.get("name"):
            raise PlatformError("remote 需要设备名称。")
        if remote.get("node_id") is not None and not re.fullmatch(r"[a-f0-9]{32}", remote["node_id"]):
            raise PlatformError("remote.node_id 必须为完整节点 ID。")
        if not remote.get("node_id") and not remote.get("management_service_id"):
            raise PlatformError("独立 SSH 管理必须绑定节点 ID 或已有管理服务映射。")
        host = remote.get("ssh_host", "")
        if not isinstance(host, str) or not host or host.startswith("-") or any(c.isspace() for c in host):
            raise PlatformError("remote.ssh_host 必须为已有 SSH 主机或别名。")
        if not re.fullmatch(r"[A-Za-z0-9_./-]+", remote.get("python", "")):
            raise PlatformError("远端 Python 必须为命令名或不含空格的路径。")
        if not isinstance(remote.get("installed"), str) or not remote["installed"]:
            raise PlatformError("请配置远端 installed.json 路径。")
    from urllib.parse import urlsplit
    web = urlsplit(value["agent_web"])
    if web.scheme not in ("http", "https") or web.hostname not in ("127.0.0.1", "localhost", "::1") or web.username or web.password or web.query or web.fragment:
        raise PlatformError("agent_web 应为本机环回管理地址，不含凭据。")
    return value


def default_remote():
    return {"name": "远端设备", "node_id": None, "management_service_id": None,
            "ssh_host": "", "python": "python3", "installed": "~/.local/lib/p2p-file-node/installed.json"}


def host_info(config):
    host = {"platform": config["platform"], "name": config["local_name"],
            "localAdminId": "mac-admin" if config["platform"] == "macos" else "local-admin",
            "adminUrl": config["agent_web"],
            "managedRemoteName": (config.get("remote") or {}).get("name"),
            "agentRestartSupported": config["lifecycle"].get("kind") != "none"}
    if config["platform"] == "macos":
        try:
            installed = json.loads(Path(config["installed"]).read_text(encoding="utf-8"))
            token_path = Path(installed["data_dir"]).expanduser().resolve() / "admin.token"
            host["tokenCommand"] = "pbcopy < " + shlex.quote(str(token_path))
        except (OSError, UnicodeError, ValueError, KeyError, TypeError):
            pass
    return host


def executable(config, installation, name):
    return str(Path(installation["bin_dir"]) / (name + (".exe" if config["platform"] == "windows" else "")))


def ssh_command(config, script):
    remote = config.get("remote")
    if not remote:
        raise PlatformError("未配置独立 SSH 管理通道；可选择设备已经发布的授权服务。")
    # A single double-quoted ASCII bootstrap works in POSIX shells and Windows
    # OpenSSH's cmd/PowerShell without shell-expanding the actual Python source.
    payload = base64.b64encode(script.encode("utf-8")).decode("ascii")
    bootstrap = f"exec(__import__('base64').b64decode('{payload}'))"
    command = remote["python"] + ' -c "' + bootstrap + '"'
    return [shutil.which("ssh") or "ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8",
            remote["ssh_host"], command]


def remote_script(config, operation):
    """A fixed remote program; only trusted installation metadata is variable."""
    installed = json.dumps(config["remote"]["installed"], ensure_ascii=True)
    prelude = 'import json,os,socket,subprocess,sys\nfrom pathlib import Path\ni=json.loads(Path(os.path.expandvars(' + installed + ')).expanduser().read_text(encoding="utf-8"))\n'
    if operation == "request":
        return prelude + '''p=subprocess.run([str(Path(i["bin_dir"])/("p2p-node.exe" if os.name=="nt" else "p2p-node")),"--data-dir",i["data_dir"],"request","-"],input=sys.stdin.read(),text=True,encoding="utf-8",capture_output=True,timeout=35)
sys.stdout.write(p.stdout)
sys.exit(p.returncode)
'''
    if operation == "probe":
        return prelude + '''s=json.loads((Path(i["data_dir"])/"node.json").read_text(encoding="utf-8"))["settings"]
service_id=json.loads(sys.stdin.read())
v=next(x for x in s["services"] if x["id"]==service_id)
host,port=v["target"].rsplit(":",1)
try:
 c=socket.create_connection((host.strip("[]"),int(port)),3);c.close();print(json.dumps({"reachable":True,"target":v["target"]}))
except OSError:
 print(json.dumps({"reachable":False,"target":v["target"]}))
'''
    raise PlatformError("未知远端管理操作。")


def process(argv, **kwargs):
    return subprocess.run(argv, capture_output=True, text=True, encoding="utf-8", errors="replace",
                          timeout=kwargs.pop("timeout", 10),
                          creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0, **kwargs)


def powershell(script, values=None):
    command = shutil.which("powershell.exe") or shutil.which("pwsh")
    if not command:
        raise PlatformError("Windows 管理需要 PowerShell 和 ScheduledTasks 模块。")
    env = os.environ.copy()
    env.pop("PSModulePath", None)
    env["VEYTRANE_NAV_VALUES"] = json.dumps(values or {}, ensure_ascii=True)
    result = process([command, "-NoProfile", "-NonInteractive", "-Command",
                      "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false);"
                      "$ErrorActionPreference='Stop';$s=$env:VEYTRANE_NAV_VALUES|ConvertFrom-Json;" + script], env=env, timeout=20)
    if result.returncode:
        raise PlatformError("Windows 系统服务操作失败，请检查当前用户的计划任务配置。")
    return result.stdout.strip()


@functools.lru_cache(maxsize=1)
def current_sid():
    return powershell("[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value")


def scheduled_agent_name(config):
    return config["lifecycle"].get("service") or ("P2PFile-Agent-" + hashlib.sha256(current_sid().encode()).hexdigest()[:12])


def secure_private_file(path):
    # POSIX mode 0600 does not enforce a Windows ACL. Apply the ACL to each new
    # temporary file before atomic replacement, never to an arbitrary parent.
    if os.name == "nt":
        powershell('$a=[System.Security.AccessControl.FileSecurity]::new();'
                   '$a.SetAccessRuleProtection($true,$false);'
                   '$a.SetOwner([System.Security.Principal.SecurityIdentifier]::new($s.sid));'
                   '$r=[System.Security.AccessControl.FileSystemAccessRule]::new($s.sid,"FullControl","Allow");'
                   '$a.AddAccessRule($r);Set-Acl -LiteralPath $s.path -AclObject $a',
                   {"path": str(path), "sid": current_sid()})
    else:
        Path(path).chmod(0o600)


def validate_lifecycle(config, installed):
    """Validate the actual launch definition before sending Agent stop."""
    lifecycle = config["lifecycle"]
    kind = lifecycle.get("kind")
    agent = executable(config, installed, "p2p-agent")
    data = installed["data_dir"]
    expected = [agent, "--data-dir", data]
    try:
        if kind == "launchd":
            definition = plistlib.loads(Path(lifecycle["definition"]).read_bytes())
            args = definition["ProgramArguments"]
            valid = args in (expected, expected + ["--managed"]) and definition.get("Label", lifecycle["service"]) == lifecycle["service"]
        elif kind == "systemd":
            result = process(["systemctl", "--user", "show", lifecycle["service"], "-p", "FragmentPath", "-p", "DropInPaths"], timeout=10)
            properties = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
            path = Path(lifecycle["definition"])
            valid = result.returncode == 0 and properties.get("FragmentPath") == str(path) and not properties.get("DropInPaths")
            commands = []
            section = ""
            for line in path.read_text(encoding="utf-8").splitlines():
                text = line.strip()
                if text.startswith("["):
                    section = text
                if section == "[Service]" and text.startswith("ExecStart="):
                    commands.append(shlex.split(text.split("=", 1)[1].replace("%%", "%")))
            valid = valid and len(commands) == 1 and commands[0] in (expected, expected + ["--managed"])
        elif kind == "scheduled-task":
            name = scheduled_agent_name(config)
            task = json.loads(powershell('$t=Get-ScheduledTask -TaskName $s.name -TaskPath "\\";'
                '@{actions=@($t.Actions|ForEach-Object { @{execute=$_.Execute;arguments=$_.Arguments} });user=$t.Principal.UserId}|ConvertTo-Json -Depth 5 -Compress', {"name": name}))
            actions = task["actions"]
            valid = len(actions) == 1 and ntpath.normcase(actions[0]["execute"]) == ntpath.normcase(agent) and actions[0]["arguments"] in (
                subprocess.list2cmdline(expected[1:]), subprocess.list2cmdline(expected[1:] + ["--managed"])) and task["user"] == current_sid()
        else:
            raise PlatformError("未配置本机节点的系统启动方式；映射管理仍可使用。")
    except (OSError, ValueError, KeyError, TypeError):
        valid = False
    if not valid:
        raise PlatformError("已安装的 Agent 与系统启动配置不一致，已拒绝重启。")


def start_agent(config):
    lifecycle = config["lifecycle"]
    kind = lifecycle["kind"]
    if kind == "launchd":
        result = process(["/bin/launchctl", "kickstart", f'gui/{os.getuid()}/{lifecycle["service"]}'])
    elif kind == "systemd":
        result = process(["systemctl", "--user", "start", lifecycle["service"]])
    elif kind == "scheduled-task":
        powershell('Start-ScheduledTask -TaskName $s.name -TaskPath "\\"', {"name": scheduled_agent_name(config)})
        return
    else:
        raise PlatformError("未配置本机节点的系统启动方式。")
    if result.returncode:
        raise PlatformError("系统未能启动 Agent，请检查本机节点服务。")
