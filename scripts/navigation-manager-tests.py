#!/usr/bin/env python3
"""Control-plane regressions: preserve existing rules, CAS, partial saves and HTTP boundaries."""
import copy
import http.client
import importlib.util
import json
import plistlib
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from navigation_manager import AgentBridge, Manager, Problem, write_private

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("navigation_server", HERE / "navigation-server.py")
server_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(server_module)
MAC, UBUNTU, WINDOWS = "a" * 32, "b" * 32, "c" * 32


def settings():
    return dict(forwards=[dict(id="admin", name="Ubuntu 管理", peer_node_id=UBUNTU, listen="127.0.0.1:27788", target="127.0.0.1:1", enabled=True, auto_start=True),
                          dict(id="existing", name="Existing", peer_node_id=UBUNTU, listen="127.0.0.1:28081", target="127.0.0.1:1", enabled=True, auto_start=True)],
                service_ids={"admin": "ubuntu-admin", "existing": "test-http"},
                signal_server="must-be-preserved", direct={"peers": {UBUNTU: ["192.0.2.1:1234"]}},
                trusted_devices=[{"node_id": UBUNTU}], resources={"peers": []})


class FakeBridge:
    def __init__(self):
        self.local = {"revision": 25, "settings": settings()}
        self.remote = {"revision": 19, "settings": dict(services=[dict(id="test-http", name="Test HTTP", target="127.0.0.1:8080", enabled=True, allowed_peers=[MAC]),
            dict(id="ubuntu-admin", name="Ubuntu 管理", target="127.0.0.1:8787", enabled=True, allowed_peers=[MAC])], other="preserved")}
        self.commands = []
        self.fail_local = None
        self.fail_rollback = False
        self.remote_identity = UBUNTU
        self.running = {"admin", "existing"}

    def call(self, command, remote=False, request_id=None):
        self.commands.append((copy.deepcopy(command), remote, request_id))
        state = self.remote if remote else self.local
        method = command["method"]
        if method == "config":
            return copy.deepcopy(state)
        if method == "status":
            return dict(epoch="original-epoch", core_running=True, node={"node_id": self.remote_identity if remote else MAC, "peers": {UBUNTU: {"phase": "connected"}}},
                        forwards={"rules": [dict(id=r["id"], listening=True, peer_ready=True) for r in self.local["settings"]["forwards"] if r["id"] in self.running], "active_streams": 7})
        if method == "services":
            return [{"id": "test-http", "name": "Test HTTP"}]
        if method == "apply_config":
            if not remote and self.fail_local:
                raise self.fail_local
            if remote and self.fail_rollback and command["expected_revision"] == 20:
                raise Problem("rollback conflict", "revision_conflict")
            if state["revision"] != command["expected_revision"]:
                raise Problem("conflict", "revision_conflict")
            state["settings"] = copy.deepcopy(command["settings"])
            state["revision"] += 1
            return {"revision": state["revision"]}
        if method == "start_forward":
            self.running.add(command["rule"])
        if method == "stop_forward":
            self.running.discard(command["rule"])
        return {"accepted": True}

    def target_probe(self, service):
        return {"reachable": True, "target": "127.0.0.1:8080"}

    def restart(self):
        return {"message": "restarted"}


class ManagerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="veytrane-navigation-tests-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.page = self.root / "navigation.html"
        baseline = {"updatedAt": "2026-11-07T00:00:00+08:00", "services": [dict(id="existing", peerNodeId=UBUNTU, serviceId="test-http", remote=True, protocol="http", url="http://127.0.0.1:28081/", name="Existing", title="Existing", target="127.0.0.1:8080")]}
        self.page.write_text('<script id="navigation-data" type="application/json">' + json.dumps(baseline) + '</script>')
        self.bridge = FakeBridge()
        self.manager = Manager(self.bridge, self.page, self.root / "control")

    def form(self, **overrides):
        value = dict(id="existing", name="Existing edited", peer=UBUNTU, serviceId="test-http", listen="127.0.0.1:28081", target="127.0.0.1:8080", enabled=True, autoStart=True, protocol="http", category="应用", path="/", healthPath="")
        value.update(overrides)
        return value

    def save(self, plan, operation="1" * 32):
        return self.manager.mutate("apply", {"plan": plan, "operation": operation})

    def test_edit_preserves_all_unrelated_configuration_and_no_restart(self):
        before = copy.deepcopy(self.bridge.local["settings"])
        plan = self.manager.preview(self.form())["plan"]
        self.save(plan)
        after = self.bridge.local["settings"]
        expected = copy.deepcopy(before)
        expected["forwards"][1]["name"] = "Existing edited"
        self.assertEqual(after, expected)
        self.assertEqual(self.bridge.remote["revision"], 19)
        self.assertFalse(any(c[0]["method"] in ("stop", "disconnect", "stop_forward") for c in self.bridge.commands))
        self.assertEqual(self.manager.state()["agent"]["epoch"], "original-epoch")

    def test_revision_conflict_before_any_write(self):
        plan = self.manager.preview(self.form(target="127.0.0.1:8081"))["plan"]
        self.bridge.local["revision"] += 1
        with self.assertRaisesRegex(Problem, "Mac 配置已变更"):
            self.save(plan)
        self.assertFalse(any(c[0]["method"] == "apply_config" for c in self.bridge.commands))

    def test_remote_revision_conflict_before_any_write(self):
        plan = self.manager.preview(self.form(target="127.0.0.1:8081"))["plan"]
        self.bridge.remote["revision"] += 1
        with self.assertRaisesRegex(Problem, "Ubuntu 配置已变更"):
            self.save(plan)
        self.assertEqual(self.bridge.local["revision"], 25)

    def test_known_local_failure_rolls_remote_back_with_cas(self):
        before = copy.deepcopy(self.bridge.remote["settings"])
        plan = self.manager.preview(self.form(target="127.0.0.1:8081"))["plan"]
        self.bridge.fail_local = Problem("bind failed", "port_in_use")
        with self.assertRaisesRegex(Problem, "Ubuntu 配置已恢复"):
            self.save(plan)
        self.assertEqual(self.bridge.remote["settings"], before)
        self.assertEqual(self.bridge.remote["revision"], 21)

    def test_unknown_local_result_does_not_undo_possible_success(self):
        plan = self.manager.preview(self.form(target="127.0.0.1:8081"))["plan"]
        self.bridge.fail_local = Problem("timeout", "outcome_unknown", True)
        with self.assertRaises(Problem) as caught:
            self.save(plan)
        self.assertEqual(caught.exception.code, "partial_unknown")
        self.assertEqual(self.bridge.remote["revision"], 20)
        self.assertEqual(self.bridge.remote["settings"]["services"][0]["target"], "127.0.0.1:8081")

    def test_duplicate_request_returns_receipt_even_after_restart(self):
        plan = self.manager.preview(self.form())["plan"]
        first = self.save(plan)
        reloaded = Manager(self.bridge, self.page, self.root / "control")
        second = reloaded.mutate("apply", {"plan": plan, "operation": "1" * 32})
        self.assertEqual(first, second)
        self.assertEqual(self.bridge.local["revision"], 26)

    def test_duplicate_id_different_payload_rejected(self):
        plan = self.manager.preview(self.form())["plan"]
        self.save(plan)
        with self.assertRaises(Problem) as caught:
            self.save("different")
        self.assertEqual(caught.exception.code, "id_conflict")

    def test_new_service_is_authorized_only_to_mac(self):
        form = self.form(id=None, name="New", serviceId="new-service", listen="127.0.0.1:28765", target="127.0.0.1:9000", autoStart=False)
        plan = self.manager.preview(form)
        self.save(plan["plan"])
        service = self.bridge.remote["settings"]["services"][-1]
        self.assertEqual(service["allowed_peers"], [MAC])
        self.assertEqual(self.bridge.local["settings"]["forwards"][:2], settings()["forwards"])

    def test_port_conflict_has_no_writes(self):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
            with self.assertRaises(Problem) as caught:
                self.manager.preview(self.form(id=None, listen=f"127.0.0.1:{port}"))
            self.assertEqual(caught.exception.code, "port_in_use")
        self.assertEqual(self.bridge.local["revision"], 25)

    def test_reject_non_loopback_and_credentials_in_path(self):
        for overrides in ({"listen": "0.0.0.0:8080"}, {"listen": "192.0.2.1:8080"}, {"path": "/?token=secret"}, {"path": "//example.com/"}):
            with self.subTest(overrides=overrides), self.assertRaises(Problem):
                self.manager.preview(self.form(**overrides))

    def test_remote_node_identity_is_pinned(self):
        self.bridge.remote_identity = WINDOWS
        with self.assertRaises(Problem) as caught:
            self.manager.preview(self.form())
        self.assertEqual(caught.exception.code, "identity_mismatch")

    def test_trusted_device_can_add_a_mapping_after_its_last_mapping_was_removed(self):
        self.bridge.local["settings"]["trusted_devices"].append({"node_id": WINDOWS})
        form = self.form(id=None, peer=WINDOWS, listen="127.0.0.1:28766", target="")
        preview = self.manager.preview(form)
        self.save(preview["plan"])
        self.assertEqual(self.bridge.local["settings"]["forwards"][-1]["peer_node_id"], WINDOWS)

    def test_reserved_ubuntu_service_id_cannot_reidentify_another_node(self):
        self.bridge.local["settings"]["trusted_devices"].append({"node_id": WINDOWS})
        with self.assertRaisesRegex(Problem, "只能用于原 Ubuntu"):
            self.manager.preview(self.form(id=None, peer=WINDOWS, serviceId="ubuntu-admin", target="", listen="127.0.0.1:28766"))

    def test_single_rule_restart_never_restarts_other_rule_or_peer(self):
        self.manager.mutate("restart_forward", {"rule": "existing", "confirm": "restart_rule", "operation": "2" * 32})
        mutations = [x[0] for x in self.bridge.commands if x[0]["method"] not in ("config", "status")]
        self.assertEqual(mutations, [{"method": "stop_forward", "rule": "existing"}, {"method": "start_forward", "rule": "existing"}])
        self.assertIn("admin", self.bridge.running)

    def test_delete_only_selected_mapping_and_preserve_remote(self):
        before = copy.deepcopy(self.bridge.remote)
        preview = self.manager.preview({"id": "existing", "remove": True})
        self.save(preview["plan"])
        self.assertEqual([r["id"] for r in self.bridge.local["settings"]["forwards"]], ["admin"])
        self.assertEqual(self.bridge.remote, before)
        with self.assertRaises(Problem):
            self.manager.preview({"id": "admin", "remove": True})

    def test_tcp_local_listener_is_not_reported_as_end_to_end_success(self):
        self.manager.metadata["existing"] = {"peer": UBUNTU, "serviceId": "test-http", "protocol": "tcp"}
        check = self.manager.diagnose("existing")
        self.assertIsNone(check["tunnel"])
        self.assertIsNone(check["http"])
        self.assertTrue(check["target"]["reachable"])

    def test_http_401_is_response_not_healthy(self):
        class Response:
            status = 401
        with patch("navigation_manager.http.client.HTTPConnection") as http:
            http.return_value.getresponse.return_value = Response()
            result = self.manager.diagnose("existing")
        self.assertTrue(result["tunnel"])
        self.assertIsNone(result["http"]["healthy"])
        self.assertEqual(result["http"]["status"], 401)

    def test_old_health_path_is_not_used_after_peer_or_service_reassignment(self):
        self.manager.metadata["existing"] = {"peer": WINDOWS, "serviceId": "other-service", "healthPath": "/old-private-check"}
        with patch("navigation_manager.http.client.HTTPConnection") as http:
            http.return_value.getresponse.return_value = SimpleNamespace(status=200)
            result = self.manager.diagnose("existing")
        self.assertEqual(http.return_value.request.call_args.args[1], "/")
        self.assertFalse(result["http"]["healthCheck"])

    def test_old_target_credentials_are_not_reused_for_changed_target(self):
        baseline = self.manager.baseline()
        baseline["services"][0].update(login={"username": "private-user", "password": "private-password"}, loginUrl="http://127.0.0.1:28081/?token=private")
        self.page.write_text('<script id="navigation-data" type="application/json">' + json.dumps(baseline) + '</script>')
        self.bridge.remote["settings"]["services"][0]["target"] = "127.0.0.1:9090"
        self.manager.remote_config()
        service = next(s for s in self.manager.navigation(self.bridge.local)["services"] if s["id"] == "existing")
        self.assertNotIn("login", service)
        self.assertNotIn("loginUrl", service)

    def test_private_login_link_follows_local_port_change_for_same_target(self):
        baseline = self.manager.baseline()
        baseline["services"][0]["loginUrl"] = "http://127.0.0.1:28081/?token=private"
        self.page.write_text('<script id="navigation-data" type="application/json">' + json.dumps(baseline) + '</script>')
        self.bridge.local["settings"]["forwards"][1]["listen"] = "127.0.0.1:28082"
        service = next(s for s in self.manager.navigation(self.bridge.local)["services"] if s["id"] == "existing")
        self.assertEqual(service["loginUrl"], "http://127.0.0.1:28082/?token=private")

    def test_private_files_have_private_permissions(self):
        plan = self.manager.preview(self.form())["plan"]
        self.save(plan)
        for path in (self.manager.receipts_path, self.manager.meta_path, self.page):
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def lifecycle_bridge(self):
        installed = self.root / "installed.json"
        installed.write_text(json.dumps({"bin_dir": "/test/bin", "data_dir": "/test/node"}))
        lifecycle = self.root / "Library/LaunchAgents/io.github.gloryhui.p2p-agent.plist"
        lifecycle.parent.mkdir(parents=True)
        lifecycle.write_bytes(plistlib.dumps({"ProgramArguments": ["/test/bin/p2p-agent", "--data-dir", "/test/node", "--managed"]}))
        return AgentBridge(installed, "ubuntu"), lifecycle

    def test_agent_restart_is_graceful_and_waits_for_ownership_release(self):
        bridge, _ = self.lifecycle_bridge()
        with patch("navigation_manager.Path.home", return_value=self.root), patch.object(bridge, "call", side_effect=[{"epoch": "before"}, {}, {"epoch": "after", "core_running": True}]) as call, patch("navigation_manager.subprocess.run", return_value=SimpleNamespace(returncode=0)) as process:
            result = bridge.restart()
        self.assertEqual(result["epoch"], "after")
        self.assertEqual([c.args[0]["method"] for c in call.call_args_list], ["status", "stop", "status"])
        self.assertEqual(process.call_args_list[0].args[0][-1], "ownership-status")
        self.assertEqual(process.call_args_list[1].args[0][1], "kickstart")
        self.assertNotIn("-k", process.call_args_list[1].args[0])

    def test_agent_restart_does_not_start_another_owner_if_locks_remain(self):
        bridge, _ = self.lifecycle_bridge()
        with patch("navigation_manager.Path.home", return_value=self.root), patch.object(bridge, "call", side_effect=[{"epoch": "before"}, {}]), patch("navigation_manager.subprocess.run", return_value=SimpleNamespace(returncode=1)) as process, patch("navigation_manager.time.monotonic", side_effect=[0, 1, 20]), patch("navigation_manager.time.sleep"):
            with self.assertRaises(Problem) as caught:
                bridge.restart()
        self.assertEqual(caught.exception.code, "restart_blocked")
        self.assertEqual(process.call_count, 1)

    def test_agent_restart_rejects_mismatched_launchagent_before_stopping(self):
        bridge, lifecycle = self.lifecycle_bridge()
        lifecycle.write_bytes(plistlib.dumps({"ProgramArguments": ["/other/p2p-agent", "--data-dir", "/other/node"]}))
        with patch("navigation_manager.Path.home", return_value=self.root), patch.object(bridge, "call") as call:
            with self.assertRaises(Problem) as caught:
                bridge.restart()
        self.assertEqual(caught.exception.code, "restart_host_mismatch")
        call.assert_not_called()


class BoundaryTests(unittest.TestCase):
    setUp = ManagerTests.setUp
    def test_http_origin_host_and_session_boundary(self):
        server = server_module.Server(("127.0.0.1", 0), self.manager, ["http://127.0.0.1:3001"])
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            def call(method, route, body=None, **extra):
                connection = http.client.HTTPConnection(*server.server_address, timeout=5)
                headers = {"Host": "127.0.0.1:3001", "X-Navigation-Client": "1", **extra}
                if body is not None:
                    headers["Content-Type"] = "application/json"
                connection.request(method, route, json.dumps(body) if body is not None else None, headers)
                response = connection.getresponse()
                status, value = response.status, json.loads(response.read())
                connection.close()
                return status, value
            self.assertEqual(call("GET", "/api/navigation/bootstrap", **{"Host": "attacker.test"})[0], 403)
            self.assertEqual(call("GET", "/api/navigation/bootstrap", **{"Sec-Fetch-Site": "cross-site"})[0], 403)
            self.assertEqual(call("GET", "/api/navigation/bootstrap", **{"X-Navigation-Client": ""})[0], 403)
            token = call("GET", "/api/navigation/bootstrap")[1]["token"]
            self.assertEqual(call("POST", "/api/navigation/state", {}, **{"Origin": "http://evil.test", "X-Navigation-Token": token})[0], 403)
            self.assertEqual(call("POST", "/api/navigation/state", {}, **{"Origin": "http://127.0.0.1:3001"})[0], 403)
            status, value = call("POST", "/api/navigation/state", {}, **{"Origin": "http://127.0.0.1:3001", "X-Navigation-Token": token})
            self.assertEqual(status, 200)
            self.assertNotIn("settings", value["result"])
            self.assertNotIn("remote_auth", json.dumps(value))
            self.assertEqual(value["result"]["revision"], 25)
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
