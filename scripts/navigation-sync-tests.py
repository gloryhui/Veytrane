#!/usr/bin/env python3
"""Offline regression tests for the cross-platform navigation synchronizer."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shlex
import stat
import tempfile
import unittest
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = REPOSITORY_ROOT / "scripts/update-service-navigation.py"
SPEC = importlib.util.spec_from_file_location("update_service_navigation", MODULE_PATH)
NAVIGATION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NAVIGATION)

UBUNTU = "11111111111111111111111111111111"
WINDOWS = "22222222222222222222222222222222"
OTHER = "33333333333333333333333333333333"
WINDOWS_OTHER = "44444444444444444444444444444444"
TEMPLATE = """<!doctype html>
<html><body><script id="navigation-data" type="application/json">
{}
</script></body></html>
"""


class NavigationSyncTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="navigation-sync-tests-")
        self.root = Path(self.temporary.name)
        self.template = self.root / "template.html"
        self.template.write_text(TEMPLATE, encoding="utf-8")

    def tearDown(self):
        self.temporary.cleanup()

    def fixture(self, platform, forwards):
        installed = self.root / "bin/installed.json"
        data_dir = self.root / "node"
        installed.parent.mkdir(parents=True, exist_ok=True)
        data_dir.mkdir(parents=True, exist_ok=True)
        installed.write_text(json.dumps({
            "bin_dir": str(self.root / "bin"),
            "data_dir": str(data_dir),
        }), encoding="utf-8")
        (data_dir / "node.json").write_text(json.dumps({
            "settings": {
                "forwards": forwards,
                "services": [],
                "service_ids": {forward["id"]: forward["service_id"] for forward in forwards},
            }
        }), encoding="utf-8")
        lifecycle = {
            "macos": {"kind": "launchd", "service": "io.example.agent",
                      "definition": str(self.root / "agent.plist")},
            "windows": {"kind": "scheduled-task", "service": None, "definition": None},
            "linux": {"kind": "systemd", "service": "p2p-file-agent.service",
                      "definition": str(self.root / "p2p-file-agent.service")},
        }[platform]
        return {
            "schema_version": 1,
            "platform": platform,
            "local_name": {"macos": "Test Mac", "windows": "Test Windows", "linux": "Test Linux"}[platform],
            "installed": str(installed),
            "page": str(self.root / "private/service-navigation.html"),
            "state_dir": str(self.root / "control"),
            "http": {"port": 3001, "public_port": 3001},
            "remote": None,
            "peers": {},
            "lifecycle": lifecycle,
            "agent_web": "http://127.0.0.1:7788/",
        }

    @staticmethod
    def forward(rule_id, service_id, peer, listen, *, auto_start=False):
        return {
            "id": rule_id,
            "name": service_id,
            "peer_node_id": peer,
            "listen": listen,
            "enabled": True,
            "auto_start": auto_start,
            "service_id": service_id,
        }

    def test_catalog_only_generation_uses_dynamic_host_and_tcp_fallback(self):
        forward = self.forward("rule-custom", "custom-db", UBUNTU, "127.0.0.1:19001")
        config = self.fixture("windows", [forward])
        config["peers"] = {UBUNTU: {"name": "Build Ubuntu", "platform": "linux"}}
        output = self.root / "new/navigation.html"
        with mock.patch.object(NAVIGATION, "read_authorized_catalog",
                               return_value={"custom-db": "Custom database"}):
            data = NAVIGATION.generate(config, output=output, template=self.template)

        self.assertTrue(output.is_file())
        self.assertEqual(data["host"]["name"], "Test Windows")
        service = next(item for item in data["services"] if item["id"] == "rule-custom")
        self.assertEqual(service["protocol"], "tcp")
        self.assertEqual(service["target"], "远端目标未公开")
        self.assertEqual(service["targetSource"], "authorized-directory")
        self.assertFalse(service["remoteConfigVerified"])
        self.assertEqual(service["peerName"], "Build Ubuntu")
        local = next(item for item in data["services"] if item["remote"] is False)
        self.assertEqual(local["id"], "local-admin")
        self.assertIn("Test Windows", local["title"])

    def test_managed_remote_uses_configured_name_and_verified_target(self):
        forward = self.forward("rule-web", "custom-web", UBUNTU, "127.0.0.1:18080")
        config = self.fixture("macos", [forward])
        config["remote"] = {
            "name": "Linux server",
            "node_id": None,
            "management_service_id": "ops-admin",
            "ssh_host": "ops",
            "python": "python3",
            "installed": "/home/example/.local/lib/p2p-file-node/installed.json",
        }
        settings = json.loads((self.root / "node/node.json").read_text(encoding="utf-8"))["settings"]
        settings["service_ids"] = {"rule-web": "custom-web", "rule-admin": "ops-admin"}
        settings["forwards"].append(self.forward("rule-admin", "ops-admin", UBUNTU, "127.0.0.1:17788"))
        (self.root / "node/node.json").write_text(json.dumps({"settings": settings}), encoding="utf-8")
        with mock.patch.object(
                NAVIGATION,
                "remote_status_and_services",
                return_value=(UBUNTU, {
                    "custom-web": {"id": "custom-web", "target": "127.0.0.1:8080", "enabled": True},
                    "ops-admin": {"id": "ops-admin", "target": "127.0.0.1:7789", "enabled": True},
                })):
            data = NAVIGATION.generate(config, output=self.root / "managed.html", template=self.template)

        service = next(item for item in data["services"] if item["id"] == "rule-web")
        self.assertEqual(service["target"], "127.0.0.1:8080")
        self.assertEqual(service["remoteLabel"], "远端 Linux server")
        self.assertTrue(service["targetVerified"])
        self.assertTrue(service["remoteConfigVerified"])

    def test_remote_identity_mismatch_is_rejected_before_config_use(self):
        config = self.fixture("macos", [self.forward("rule-web", "custom-web", UBUNTU, "127.0.0.1:18080")])
        config["remote"] = {
            "name": "Linux server",
            "node_id": UBUNTU,
            "management_service_id": None,
            "ssh_host": "ops",
            "python": "python3",
            "installed": "/home/example/.local/lib/p2p-file-node/installed.json",
        }
        mismatched = json.dumps({"node_id": OTHER, "services": []})
        captured = {}

        def fake_ssh_command(_config, script):
            captured["script"] = script
            return ["ssh", "ops"]

        with mock.patch.object(NAVIGATION.navigation_platform, "ssh_command",
                               side_effect=fake_ssh_command), \
                mock.patch.object(NAVIGATION, "_process_result", return_value=mismatched):
            with self.assertRaises(NAVIGATION.NavigationError):
                NAVIGATION.remote_status_and_services(config, UBUNTU)
        self.assertIn('"status"', captured["script"])
        self.assertLess(captured["script"].index('"status"'), captured["script"].index('"node.json"'))

    def test_private_mac_login_never_leaks_into_public_output(self):
        config = self.fixture(
            "macos",
            [self.forward("rule-chromium", "chromium-web", UBUNTU, "127.0.0.1:13001")],
        )
        config["remote"] = {
            "name": "Ubuntu",
            "node_id": UBUNTU,
            "management_service_id": None,
            "ssh_host": "ubuntu",
            "python": "python3",
            "installed": "/home/ubuntu/.local/lib/p2p-file-node/installed.json",
        }
        private_root = Path(config["page"]).parent
        private_root.mkdir(parents=True)
        (private_root / "show-chromium-login").touch()
        public = self.root / "public/navigation.html"
        secret = {"username": "private-user", "password": "private-password"}
        remote_services = {
            "chromium-web": {
                "id": "chromium-web",
                "target": "127.0.0.1:3001",
                "enabled": True,
            }
        }
        with mock.patch.object(NAVIGATION, "remote_status_and_services",
                               return_value=(UBUNTU, remote_services)), \
                mock.patch.object(NAVIGATION, "_read_remote_secret", return_value=secret):
            NAVIGATION.generate(config, output=public, template=self.template)

        public_text = public.read_text(encoding="utf-8")
        private_text = Path(config["page"]).read_text(encoding="utf-8")
        self.assertNotIn("private-user", public_text)
        self.assertNotIn("private-password", public_text)
        self.assertIn("private-user", private_text)
        self.assertEqual(stat.S_IMODE(Path(config["page"]).stat().st_mode), 0o600)

    def test_private_acl_is_applied_to_empty_temporary_file_before_content(self):
        observed = []

        def inspect_empty_temporary(path):
            observed.append(Path(path).read_text(encoding="utf-8"))

        with mock.patch.object(NAVIGATION.navigation_platform, "secure_private_file",
                               side_effect=inspect_empty_temporary):
            NAVIGATION.atomic_write(self.root / "private.html", "secret", private=True)
        self.assertEqual(observed, [""])
        self.assertEqual((self.root / "private.html").read_text(encoding="utf-8"), "secret")

    def test_private_output_is_written_once_with_private_permissions(self):
        config = self.fixture(
            "macos",
            [self.forward("rule-web", "custom-web", UBUNTU, "127.0.0.1:18080")],
        )
        config["remote"] = {
            "name": "Ubuntu",
            "node_id": UBUNTU,
            "management_service_id": None,
            "ssh_host": "ubuntu",
            "python": "python3",
            "installed": "/home/example/.local/lib/p2p-file-node/installed.json",
        }
        remote_services = {
            "custom-web": {"id": "custom-web", "target": "127.0.0.1:8080", "enabled": True}
        }
        output = io.StringIO()
        with mock.patch.object(NAVIGATION, "remote_status_and_services",
                               return_value=(UBUNTU, remote_services)), \
                mock.patch.object(NAVIGATION, "atomic_write", wraps=NAVIGATION.atomic_write) as atomic, \
                contextlib.redirect_stdout(output):
            NAVIGATION.generate(config, output=Path(config["page"]), template=self.template)

        self.assertEqual(atomic.call_count, 1)
        self.assertTrue(atomic.call_args.kwargs["private"])
        self.assertEqual(stat.S_IMODE(Path(config["page"]).stat().st_mode), 0o600)
        self.assertNotIn("no credentials included", output.getvalue())

    def test_private_credential_failure_preserves_existing_private_page(self):
        config = self.fixture(
            "macos",
            [self.forward("rule-chromium", "chromium-web", UBUNTU, "127.0.0.1:13001")],
        )
        config["remote"] = {
            "name": "Ubuntu",
            "node_id": UBUNTU,
            "management_service_id": None,
            "ssh_host": "ubuntu",
            "python": "python3",
            "installed": "/home/example/.local/lib/p2p-file-node/installed.json",
        }
        page = Path(config["page"])
        page.parent.mkdir(parents=True)
        page.write_text("existing-private-page", encoding="utf-8")
        page.chmod(0o600)
        (page.parent / "show-chromium-login").touch()
        remote_services = {
            "chromium-web": {"id": "chromium-web", "target": "127.0.0.1:3001", "enabled": True}
        }
        with mock.patch.object(NAVIGATION, "remote_status_and_services",
                               return_value=(UBUNTU, remote_services)), \
                mock.patch.object(NAVIGATION, "_read_remote_secret",
                                  side_effect=NAVIGATION.NavigationError("credential lookup failed")), \
                mock.patch.object(NAVIGATION, "atomic_write", wraps=NAVIGATION.atomic_write) as atomic:
            with self.assertRaises(NAVIGATION.NavigationError):
                NAVIGATION.generate(config, output=page, template=self.template)

        atomic.assert_not_called()
        self.assertEqual(page.read_text(encoding="utf-8"), "existing-private-page")
        self.assertEqual(stat.S_IMODE(page.stat().st_mode), 0o600)

    def test_mac_token_command_uses_metadata_data_dir_and_neutral_fallback(self):
        config = self.fixture("macos", [])
        custom_data = self.root / "custom data/agent"
        installed = json.loads((self.root / "bin/installed.json").read_text(encoding="utf-8"))
        installed["data_dir"] = str(custom_data)
        (self.root / "bin/installed.json").write_text(json.dumps(installed), encoding="utf-8")
        host = NAVIGATION.navigation_host(config)
        expected = "pbcopy < " + shlex.quote(str((custom_data / "admin.token").resolve()))
        self.assertEqual(host["tokenCommand"], expected)

        (self.root / "bin/installed.json").unlink()
        neutral = NAVIGATION.navigation_host(config)
        self.assertNotIn("tokenCommand", neutral)
        left = NAVIGATION.local_admin_service(config, host)
        right = NAVIGATION.local_admin_service(host)
        self.assertEqual(left["id"], right["id"])
        self.assertEqual(left["title"], right["title"])

    def test_catalog_targets_are_per_peer_and_remain_unverified(self):
        config = self.fixture("linux", [
            self.forward("rule-rdp-a", "windows-rdp", WINDOWS, "127.0.0.1:13389"),
            self.forward("rule-rdp-b", "windows-rdp", WINDOWS_OTHER, "127.0.0.1:14389"),
        ])
        config["peers"] = {
            WINDOWS: {
                "name": "Windows workstation A",
                "platform": "windows",
                "targets": {"windows-rdp": "192.0.2.10:3389"},
            },
            WINDOWS_OTHER: {
                "name": "Windows workstation B",
                "platform": "windows",
            },
        }
        with mock.patch.object(NAVIGATION, "read_authorized_catalog",
                               side_effect=lambda _local, _peer: {"windows-rdp": "Remote desktop"}):
            data = NAVIGATION.generate(config, output=self.root / "windows-peer.html", template=self.template)

        service_a = next(item for item in data["services"] if item["id"] == "rule-rdp-a")
        service_b = next(item for item in data["services"] if item["id"] == "rule-rdp-b")
        self.assertEqual(service_a["protocol"], "rdp")
        self.assertEqual(service_a["target"], "192.0.2.10:3389")
        self.assertEqual(service_a["targetSource"], "user-confirmed")
        self.assertFalse(service_a["targetVerified"])
        self.assertTrue(service_a["authorizedDirectoryVerified"])
        self.assertEqual(service_b["protocol"], "rdp")
        self.assertEqual(service_b["target"], "远端目标未公开")
        self.assertEqual(service_b["targetSource"], "authorized-directory")
        self.assertFalse(service_b["targetVerified"])
        self.assertTrue(service_b["authorizedDirectoryVerified"])

    def test_legacy_config_and_new_navigation_config_coexist(self):
        args = NAVIGATION.parse_args([
            "--config", "/tmp/local-node.json",
            "--navigation-config", "/tmp/navigation.json",
            "--ssh-host", "ubuntu",
        ])
        self.assertEqual(args.config, Path("/tmp/local-node.json"))
        self.assertEqual(args.navigation_config, Path("/tmp/navigation.json"))
        self.assertEqual(args.ssh_host, "ubuntu")

    def test_generation_does_not_modify_template_or_source_node_config(self):
        config = self.fixture(
            "linux",
            [self.forward("rule-custom", "custom", UBUNTU, "127.0.0.1:19002")],
        )
        config["peers"] = {UBUNTU: {"name": "Build Ubuntu", "platform": "linux"}}
        template_before = self.template.read_bytes()
        node_before = (self.root / "node/node.json").read_bytes()
        with mock.patch.object(NAVIGATION, "read_authorized_catalog",
                               return_value={"custom": "Custom service"}):
            NAVIGATION.generate(config, output=self.root / "output.html", template=self.template)
        self.assertEqual(self.template.read_bytes(), template_before)
        self.assertEqual((self.root / "node/node.json").read_bytes(), node_before)

    def test_example_configs_are_json_without_persisted_platform(self):
        directory = REPOSITORY_ROOT / "docs/examples/navigation"
        files = sorted(directory.glob("*.json"))
        self.assertEqual({path.name for path in files}, {"macos.json", "windows.json", "linux.json"})
        required = {"schema_version", "local_name", "installed", "page", "state_dir",
                    "http", "remote", "peers", "lifecycle", "agent_web"}
        for path in files:
            value = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(set(value), required)
            self.assertNotIn("platform", value)
            self.assertEqual(value["agent_web"], "http://127.0.0.1:7789/")
            self.assertEqual(value["http"], {"port": 3001, "public_port": 3001})


if __name__ == "__main__":
    unittest.main(verbosity=2)
