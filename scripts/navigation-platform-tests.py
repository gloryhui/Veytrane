#!/usr/bin/env python3
"""OS adapters with mocked services: never stop/restart an installed Agent."""
import copy
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import navigation_platform as adapter


class PlatformTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="veytrane-nav-platform-")
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def config(self, system):
        with patch.object(adapter, "system_name", return_value=system), patch.object(adapter.Path, "home", return_value=self.root), patch.dict(os.environ, {"LOCALAPPDATA": str(self.root / "AppData"), "XDG_DATA_HOME": str(self.root / "data"), "XDG_CONFIG_HOME": str(self.root / "config")}, clear=False):
            return adapter.default_config()

    def test_platform_paths_and_unmanaged_defaults(self):
        for system in ("macos", "windows", "linux"):
            config = self.config(system)
            self.assertEqual(config["platform"], system)
            self.assertTrue(Path(config["installed"]).is_absolute())
            self.assertEqual(adapter.host_info(config)["localAdminId"], "mac-admin" if system == "macos" else "local-admin")
            self.assertEqual(bool(config["remote"]), system == "macos")
            self.assertEqual(config["http"], {"port": 3001, "public_port": 3001})
        self.assertIn("AppData/p2p-file/bin/installed.json", self.config("windows")["installed"])
        self.assertIn(".local/lib/p2p-file-node/installed.json", self.config("linux")["installed"])

    def load(self, source, system="linux"):
        target = self.root / "navigation.json"
        target.write_text(json.dumps(source), encoding="utf-8")
        with patch.object(adapter, "default_config", return_value=self.config(system)):
            return adapter.load_config(target)

    def test_relative_paths_custom_host_and_nested_defaults(self):
        config = self.load({"local_name": "研发工作站", "installed": "agent/installed.json", "http": {"port": 18880}, "remote": {"name": "存储", "node_id": "a" * 32, "ssh_host": "storage"}})
        self.assertEqual(config["installed"], str(self.root.resolve() / "agent/installed.json"))
        self.assertEqual(config["http"], {"port": 18880, "public_port": 3001})
        legacy = self.load({"http": {"port": 17790, "public_port": 3001}}, system="macos")
        self.assertEqual(legacy["http"], {"port": 3001, "public_port": 3001})
        self.assertEqual(config["remote"]["python"], "python3")
        self.assertEqual(adapter.host_info(config)["name"], "研发工作站")

    def test_reject_wrong_os_unknown_key_invalid_ports_and_remote_identity(self):
        for source in ({"platform": "windows"}, {"schema_version": 2}, {"http": {"port": True}}, {"http": {"port": 0}}, {"local_name": " "}, {"remote": {"ssh_host": "ubuntu"}}, {"remote": {"node_id": "a" * 32, "ssh_host": "-oProxyCommand=anything"}}, {"remote": {"node_id": "a" * 32, "ssh_host": "ubuntu", "python": "python3; other"}}, {"agent_web": "http://external.example/"}, {"lifecycle": {"kind": "launchd"}}):
            with self.subTest(source=source), self.assertRaises(adapter.PlatformError):
                self.load(source)

    def test_peer_targets_are_explicit_and_validated_per_device(self):
        peer = "a" * 32
        entry = {"name": "办公室", "platform": "windows", "targets": {"rdp": "127.0.0.1:3389"}}
        self.assertEqual(self.load({"peers": {peer: entry}})["peers"][peer], entry)
        for invalid in ([], {"rdp": ""}, {"rdp": None}):
            with self.subTest(targets=invalid), self.assertRaises(adapter.PlatformError):
                self.load({"peers": {peer: {**entry, "targets": invalid}}})
        with self.assertRaises(adapter.PlatformError):
            self.load({"peers": {peer: {**entry, "platform": "windwos"}}})

    def test_ssh_uses_config_and_saved_remote_data_directory(self):
        config = self.load({"remote": {"name": "服务机", "node_id": "a" * 32, "ssh_host": "storage", "installed": "~/custom bin/installed.json"}})
        script = adapter.remote_script(config, "probe")
        self.assertIn('Path(i["data_dir"])/"node.json"', script)
        self.assertNotIn('".local/share/p2p-file/node/node.json"', script)
        with patch.object(adapter.shutil, "which", return_value="/usr/bin/ssh"):
            argv = adapter.ssh_command(config, script)
        self.assertEqual(argv[-2], "storage")
        self.assertTrue(argv[-1].startswith("python3 -c "))
        import shlex
        bootstrap = shlex.split(argv[-1])[2]
        import base64
        payload = bootstrap.split("b64decode('", 1)[1].split("')", 1)[0]
        self.assertEqual(base64.b64decode(payload).decode("utf-8"), script)
        self.assertNotIn("\n", bootstrap)

    def test_windows_cli_has_exe_suffix(self):
        config = self.config("windows")
        self.assertTrue(adapter.executable(config, {"bin_dir": "/custom/bin"}, "p2p-node").endswith("p2p-node.exe"))

    @unittest.skipUnless(os.name == "nt", "requires Windows file ACLs")
    def test_private_file_acl_allows_only_the_current_windows_sid(self):
        path = self.root / "private.json"
        path.write_text("{}", encoding="utf-8")
        sid = adapter.current_sid()
        adapter.secure_private_file(path)
        result = json.loads(adapter.powershell(
            '$a=Get-Acl -LiteralPath $s.path;'
            '@{owner=$a.GetOwner([System.Security.Principal.SecurityIdentifier]).Value;'
            'protected=$a.AreAccessRulesProtected;'
            'rules=@($a.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])|'
            'ForEach-Object {@{sid=$_.IdentityReference.Value;'
            'rights=[string]$_.FileSystemRights;type=[string]$_.AccessControlType;'
            'inherited=$_.IsInherited}})}|ConvertTo-Json -Depth 5 -Compress',
            {"path": str(path)}))
        self.assertEqual(result["owner"], sid)
        self.assertTrue(result["protected"])
        self.assertEqual(result["rules"], [{
            "sid": sid, "rights": "FullControl", "type": "Allow", "inherited": False,
        }])

    def test_launchd_verifies_definition_and_no_force_start(self):
        config = self.config("macos")
        installed = {"bin_dir": "/test/bin", "data_dir": "/test/node"}
        definition = self.root / "agent.plist"
        config["lifecycle"]["definition"] = str(definition)
        definition.write_bytes(plistlib.dumps({"Label": config["lifecycle"]["service"], "ProgramArguments": ["/test/bin/p2p-agent", "--data-dir", "/test/node", "--managed"]}))
        adapter.validate_lifecycle(config, installed)
        with patch.object(adapter, "process", return_value=SimpleNamespace(returncode=0)) as run:
            adapter.start_agent(config)
        self.assertNotIn("-k", run.call_args.args[0])
        definition.write_bytes(plistlib.dumps({"ProgramArguments": ["/wrong/bin/p2p-agent", "--data-dir", "/test/node"]}))
        with self.assertRaises(adapter.PlatformError):
            adapter.validate_lifecycle(config, installed)

    def test_systemd_validates_loaded_unit_and_disallows_overriding_dropins(self):
        config = self.config("linux")
        installed = {"bin_dir": "/test bin", "data_dir": "/test node"}
        definition = self.root / "agent.service"
        config["lifecycle"]["definition"] = str(definition)
        definition.write_text('[Service]\nExecStart="/test bin/p2p-agent" --data-dir "/test node"\n', encoding="utf-8")
        with patch.object(adapter, "process", return_value=SimpleNamespace(returncode=0, stdout=f"FragmentPath={definition}\nDropInPaths=\n")):
            adapter.validate_lifecycle(config, installed)
        for output in (f"FragmentPath={definition}\nDropInPaths=/other/override.conf\n", "FragmentPath=/wrong/service\nDropInPaths=\n"):
            with patch.object(adapter, "process", return_value=SimpleNamespace(returncode=0, stdout=output)), self.assertRaises(adapter.PlatformError):
                adapter.validate_lifecycle(config, installed)
        with patch.object(adapter, "process", return_value=SimpleNamespace(returncode=0)) as run:
            adapter.start_agent(config)
        self.assertEqual(run.call_args.args[0], ["systemctl", "--user", "start", "p2p-file-agent.service"])

    def test_windows_task_checks_owner_executable_and_data_arguments(self):
        config = self.config("windows")
        installed = {"bin_dir": "/test/bin", "data_dir": "/test/node"}
        task = {"user": "S-1-5-21-123", "actions": [{"execute": "/test/bin/p2p-agent.exe", "arguments": "--data-dir /test/node"}]}
        with patch.object(adapter, "current_sid", return_value=task["user"]), patch.object(adapter, "powershell", return_value=json.dumps(task)):
            adapter.validate_lifecycle(config, installed)
        for change in ({"user": "S-1-5-21-other"}, {"actions": [{"execute": "/wrong/p2p-agent.exe", "arguments": "--data-dir /test/node"}]}, {"actions": [{"execute": "/test/bin/p2p-agent.exe", "arguments": "--data-dir /other/node"}]}):
            with patch.object(adapter, "current_sid", return_value=task["user"]), patch.object(adapter, "powershell", return_value=json.dumps({**task, **change})), self.assertRaises(adapter.PlatformError):
                adapter.validate_lifecycle(config, installed)
        with patch.object(adapter, "current_sid", return_value=task["user"]), patch.object(adapter, "powershell") as run:
            adapter.start_agent(config)
        self.assertIn("Start-ScheduledTask", run.call_args.args[0])
        self.assertNotIn("Stop-Process", run.call_args.args[0])

    def test_disabled_lifecycle_cannot_restart(self):
        config = self.config("linux")
        config["lifecycle"]["kind"] = "none"
        self.assertFalse(adapter.host_info(config)["agentRestartSupported"])
        with self.assertRaises(adapter.PlatformError):
            adapter.validate_lifecycle(config, {"bin_dir": "/test/bin", "data_dir": "/test/node"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
