#!/usr/bin/env python3
"""Exercise the integrated Rust navigation using an isolated Agent and identity.

No installed Agent, navigation page, SSH connection or system service is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def private_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600),
                   "w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    agent = (args.bin_dir / ("p2p-agent" + suffix)).resolve()
    node = (args.bin_dir / ("p2p-node" + suffix)).resolve()
    assert agent.is_file() and node.is_file()
    checks = []
    # Keep the production Unix socket path below macOS's sockaddr_un limit.
    temporary_parent = "/tmp" if sys.platform == "darwin" else None
    with tempfile.TemporaryDirectory(prefix="vnav-", dir=temporary_parent) as directory:
        root = Path(directory).resolve()
        data = root / "node"
        navigation = root / "navigation"
        navigation.mkdir(mode=0o700)
        command = [str(node), "--data-dir", str(data)]

        def cli(*parts):
            result = subprocess.run(command + list(parts), capture_output=True,
                                    text=True, encoding="utf-8", timeout=20)
            assert result.returncode == 0, result.stderr
            value = json.loads(result.stdout)
            assert not value.get("error"), value
            return value.get("result", value)

        cli("init")
        record = json.loads((data / "node.json").read_text(encoding="utf-8"))
        listen = free_port()
        record["settings"]["forwards"] = [{
            "id": "isolated-navigation-rule",
            "name": "隔离导航回归",
            "peer_node_id": "a" * 32,
            "listen": f"127.0.0.1:{listen}",
            "target": "127.0.0.1:1",
            "enabled": True, "auto_start": True,
        }]
        record["settings"]["service_ids"]["isolated-navigation-rule"] = "isolated-service"
        (data / "node.json").write_text(json.dumps(record), encoding="utf-8")
        (data / "node.json").chmod(0o600)
        identity_before = digest(data / "identity.key")
        config_before = digest(data / "node.json")
        port = free_port()
        private_json(root / "installed.json", {"bin_dir": str(agent.parent), "data_dir": str(data)})
        private_json(navigation / "config.json", {
            "schema_version": 1, "local_name": "隔离测试节点",
            "installed": str(root / "installed.json"),
            "page": str(navigation / "service-navigation.html"),
            "state_dir": str(navigation / "control"),
            "http": {"port": port, "public_port": port},
            "remote": None, "peers": {"a" * 32: {"name": "隔离远端"}},
            "lifecycle": {"kind": "none"}, "agent_web": "http://127.0.0.1:7789/",
        })
        origin = f"http://127.0.0.1:{port}"
        token = None

        def http(path="/", body=None):
            headers = {"X-Navigation-Client": "1", "Origin": origin}
            if body is not None:
                headers.update({"X-Navigation-Token": token,
                                "Content-Type": "application/json"})
            request = urllib.request.Request(
                origin + path, headers=headers,
                data=None if body is None else json.dumps(body).encode(),
            )
            with urllib.request.urlopen(request, timeout=15) as response:
                content = response.read()
                assert response.headers["Cache-Control"] == "no-store"
                return content if path in ("/", "/service-navigation.html") else json.loads(content)

        def api(action, body):
            return http("/api/navigation/" + action, body)["result"]

        def until(fn, seconds=25):
            deadline = time.monotonic() + seconds
            last = None
            while time.monotonic() < deadline:
                try:
                    value = fn()
                    if value:
                        return value
                except (OSError, ValueError, AssertionError) as error:
                    last = error
                time.sleep(0.1)
            raise AssertionError(f"isolated Agent did not become ready: {last}")

        with (root / "agent.log").open("w", encoding="utf-8") as log:
            process = subprocess.Popen(
                [str(agent), "--data-dir", str(data), "--navigation-config",
                 str(navigation / "config.json")], stdout=log, stderr=log)
            try:
                bootstrap = until(lambda: http("/api/navigation/bootstrap"))
                token = bootstrap["token"]
                assert bootstrap["host"]["name"] == "隔离测试节点"
                initial = until(lambda: (state if (state := api("state", {}))
                                         ["agent"].get("running") else None))
                assert http("/") == http("/service-navigation.html")
                assert initial["rules"][0]["listening"]
                assert initial["navigation"]["services"][0]["remoteLabel"] == "远端 隔离远端"
                checks.append("Rust serves the existing HTML and API from one Agent")

                operation = uuid.uuid4().hex
                body = {"rule": "isolated-navigation-rule", "operation": operation}
                stopped = api("stop", body)
                assert api("stop", body) == stopped
                assert not any(rule.get("listening") for rule in api("state", {})["rules"])
                assert digest(data / "node.json") == config_before
                api("start", {**body, "operation": uuid.uuid4().hex})
                assert api("state", {})["rules"][0]["listening"]
                checks.append("single-rule operations are idempotent and retain saved configuration")

                restarted = api("restart_agent", {
                    "confirm": "restart_all", "operation": uuid.uuid4().hex,
                })
                assert restarted.get("accepted")
                old_epoch = initial["agent"]["epoch"]
                page_checks = [0]

                def core_restarted():
                    assert b"navigation-data" in http("/")
                    page_checks[0] += 1
                    state = api("state", {})
                    return state if state["agent"].get("running") and \
                        state["agent"].get("epoch") != old_epoch else None

                fresh = until(core_restarted, seconds=35)
                assert process.poll() is None
                assert fresh["rules"][0]["listening"]
                assert http("/api/navigation/bootstrap")["token"] == token
                assert digest(data / "identity.key") == identity_before
                assert digest(data / "node.json") == config_before
                checks.append("core restart retains the HTTP host, identity, configuration and listener")

                preview = api("preview", {"id": "isolated-navigation-rule", "remove": True})
                apply_body = {"plan": preview["plan"], "operation": uuid.uuid4().hex}
                applied = api("apply", apply_body)
                assert api("apply", apply_body) == applied
                state = api("state", {})
                assert state["revision"] == initial["revision"] + 1
                assert not state["rules"]
                assert digest(data / "identity.key") == identity_before
                checks.append("preview/apply uses the Agent revision API and retains identity")
                print(json.dumps({"result": "PASS", "checks": checks,
                                  "pageChecksDuringRestart": page_checks[0],
                                  "installedAgentRequests": 0}, ensure_ascii=False))
            except Exception:
                log.flush()
                print((root / "agent.log").read_text(encoding="utf-8", errors="replace")[-5000:])
                raise
            finally:
                if process.poll() is None:
                    try:
                        cli("stop")
                        process.wait(timeout=20)
                    except (OSError, AssertionError, subprocess.TimeoutExpired):
                        process.terminate()
                        process.wait(timeout=20)


if __name__ == "__main__":
    main()
