#!/usr/bin/env python3
"""Mock-only regressions for the integrated navigation preparation tool."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "navigation_install_under_test", HERE / "install-navigation-control.py")
INSTALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALL)


def page_html(host):
    data = {"updatedAt": "2026-11-14T00:00:00+08:00", "host": host, "services": []}
    return (
        '<!doctype html><script id="navigation-data" type="application/json">'
        + json.dumps(data)
        + "</script>"
    )


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="navigation-install-tests-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.base = self.root / "p2p-file"
        self.data_dir = self.base / "node"
        self.data_dir.mkdir(parents=True)
        self.navigation = self.base / "navigation"
        self.page = self.navigation / "service-navigation.html"
        self.state = self.navigation / "control"
        self.installed = self.base / "bin/installed.json"
        self.installed.parent.mkdir(parents=True)
        self.installed.write_text(json.dumps({
            "bin_dir": str(self.base / "bin"),
            "data_dir": str(self.data_dir),
        }), encoding="utf-8")

    def config(self):
        host_platform = {"Darwin": "macos", "Windows": "windows",
                         "Linux": "linux"}[platform.system()]
        kind = {"macos": "launchd", "windows": "scheduled-task",
                "linux": "systemd"}[host_platform]
        return {
            "schema_version": 1,
            "platform": host_platform,
            "local_name": "Test workstation",
            "installed": str(self.installed),
            "page": str(self.page),
            "state_dir": str(self.state),
            "navigation_root": str(self.navigation),
            "config_file": str(self.navigation / "source-config.json"),
            "http": {"port": 3001, "public_port": 3001},
            "remote": None,
            "peers": {},
            "lifecycle": {
                "kind": kind,
                "service": "p2p-file-agent.service",
                "definition": str(self.root / "agent-service"),
            },
            "agent_web": "http://127.0.0.1:7789/",
        }

    def args(self, **values):
        defaults = {
            "installed": None,
            "page": None,
            "state_dir": None,
            "ssh_host": None,
            "replace": False,
            "no_autostart": False,
        }
        defaults.update(values)
        return SimpleNamespace(**defaults)

    def test_private_write_secures_before_content_and_final_paths(self):
        target = self.navigation / "config.json"
        observations = []

        def inspect(path):
            observations.append(Path(path).read_text(encoding="utf-8"))

        with patch.object(INSTALL, "secure_private_file", side_effect=inspect) as secure:
            INSTALL.atomic_private(target, {"name": "中文"})
        self.assertEqual(observations, ["", json.dumps(
            {"name": "中文"}, ensure_ascii=False, indent=2) + "\n"])
        self.assertEqual(secure.call_count, 2)
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)

    def test_private_write_failure_never_writes_sensitive_content(self):
        target = self.navigation / "config.json"
        with patch.object(
                INSTALL, "secure_private_file",
                side_effect=RuntimeError("ACL denied")):
            with self.assertRaisesRegex(RuntimeError, "ACL denied"):
                INSTALL.atomic_private(target, "sensitive")
        self.assertFalse(target.exists())
        temporary = list(target.parent.glob(".navigation-*"))
        self.assertEqual(len(temporary), 1)
        self.assertEqual(temporary[0].stat().st_size, 0)

    def test_secure_directory_creation_supports_missing_first_install(self):
        directory = self.navigation / "new/nested/control"
        self.assertEqual(INSTALL.ensure_directory(directory), directory)
        self.assertTrue(directory.is_dir())

    def test_secure_directory_creation_rejects_symlink_component(self):
        outside = self.root / "outside"
        outside.mkdir()
        linked = self.root / "linked"
        try:
            linked.symlink_to(outside, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlinks are unavailable: {error}")
        with self.assertRaisesRegex(RuntimeError, "canonical non-symlink"):
            INSTALL.ensure_directory(linked / "private")
        self.assertFalse((outside / "private").exists())

    def test_validate_derives_standard_config_and_private_paths(self):
        config = self.config()
        locations = INSTALL.validate_config(config)
        self.assertEqual(locations["data_dir"], self.data_dir)
        self.assertEqual(locations["navigation_root"], self.navigation)
        self.assertEqual(locations["config_path"], self.navigation / "config.json")
        self.assertEqual(locations["page"], self.page)
        self.assertEqual(config["navigation_root"], str(self.navigation))

    def test_rejects_page_or_state_outside_agent_navigation_root(self):
        for field, value in (
                ("page", self.root / "outside/page.html"),
                ("state_dir", self.root / "outside/control")):
            with self.subTest(field=field):
                config = self.config()
                config[field] = str(value)
                with self.assertRaisesRegex(RuntimeError, "navigation"):
                    INSTALL.validate_config(config)

    def test_rejects_symlinked_navigation_root(self):
        outside = self.root / "outside-navigation"
        outside.mkdir()
        try:
            self.navigation.symlink_to(outside, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlinks are unavailable: {error}")
        with self.assertRaisesRegex(RuntimeError, "canonical non-symlink"):
            INSTALL.validate_config(self.config())
        self.assertEqual(list(outside.iterdir()), [])

    def test_persisted_config_filters_runtime_fields_and_uses_private_mode(self):
        config = self.config()
        locations = INSTALL.validate_config(config)
        path = INSTALL.persist_config(locations["config_path"], config)
        stored = json.loads(path.read_text(encoding="utf-8"))
        self.assertNotIn("platform", stored)
        self.assertNotIn("navigation_root", stored)
        self.assertNotIn("config_file", stored)
        self.assertEqual(stored["http"], {"port": 3001, "public_port": 3001})
        self.assertEqual(path, self.navigation / "config.json")
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_effective_config_keeps_source_and_applies_explicit_overrides(self):
        config = self.config()
        other_page = self.navigation / "alternate.html"
        other_state = self.navigation / "alternate-state"
        value = INSTALL.effective_config(config, self.args(
            page=other_page, state_dir=other_state))
        self.assertEqual(value["page"], str(other_page.resolve(strict=False)))
        self.assertEqual(value["state_dir"], str(other_state.resolve(strict=False)))
        self.assertIsNone(value["remote"])
        self.assertEqual(config["page"], str(self.page))
        with self.assertRaisesRegex(ValueError, "non-null remote"):
            INSTALL.effective_config(config, self.args(ssh_host="ops"))

    def test_rejects_non_integrated_ports(self):
        config = self.config()
        config["http"] = {"port": 17790, "public_port": 3001}
        with self.assertRaisesRegex(RuntimeError, "3001"):
            INSTALL.validate_config(config)

    def test_failed_generation_preserves_existing_private_page(self):
        self.navigation.mkdir(parents=True)
        self.page.write_text(page_html({
            "platform": self.config()["platform"],
            "name": "Test workstation",
        }), encoding="utf-8")
        self.page.chmod(0o600)
        before = self.page.read_bytes()
        failed = SimpleNamespace(returncode=1, stdout="", stderr="remote unavailable")
        with patch.object(INSTALL, "run", return_value=failed) as run:
            source = INSTALL.generate_page(HERE, self.config(), self.navigation / "config.json")
        self.assertEqual(source, "preserved-existing")
        self.assertEqual(self.page.read_bytes(), before)
        self.assertEqual(self.page.stat().st_mode & 0o777, 0o600)
        argv = run.call_args.args[0]
        self.assertEqual(Path(argv[1]).name, "update-service-navigation.py")
        self.assertNotIn("navigation-server.py", " ".join(str(item) for item in argv))
        self.assertNotIn("nginx", " ".join(str(item) for item in argv).lower())

    def test_failed_first_generation_creates_private_local_only_page(self):
        failed = SimpleNamespace(returncode=1, stdout="", stderr="remote unavailable")

        def fallback(_here, _config, page):
            INSTALL.atomic_private(page, page_html({
                "platform": _config["platform"], "name": _config["local_name"],
            }))

        with patch.object(INSTALL, "run", return_value=failed), patch.object(
                INSTALL, "fallback_local_page", side_effect=fallback) as local:
            source = INSTALL.generate_page(HERE, self.config(), self.navigation / "config.json")
        self.assertEqual(source, "local-only-fallback")
        self.assertTrue(INSTALL.page_matches_host(self.config(), self.page))
        self.assertEqual(self.page.stat().st_mode & 0o777, 0o600)
        local.assert_called_once()

    def test_real_cross_module_fallback_uses_generator_contracts(self):
        failed = SimpleNamespace(returncode=1, stdout="", stderr="remote unavailable")
        with patch.object(INSTALL, "run", return_value=failed):
            source = INSTALL.generate_page(HERE, self.config(), self.navigation / "config.json")
        self.assertEqual(source, "local-only-fallback")
        match = INSTALL.re.search(
            r'<script id="navigation-data" type="application/json">([\s\S]*?)</script>',
            self.page.read_text(encoding="utf-8"),
        )
        self.assertIsNotNone(match)
        data = json.loads(match.group(1))
        self.assertEqual(data["host"]["platform"], self.config()["platform"])
        self.assertEqual(len(data["services"]), 1)
        self.assertEqual(data["services"][0]["id"], data["host"]["localAdminId"])

    def test_repository_template_copy_is_not_treated_as_private_page(self):
        self.navigation.mkdir(parents=True)
        template = HERE.parent / "service-navigation.html"
        self.page.write_bytes(template.read_bytes())
        failed = SimpleNamespace(returncode=1, stdout="", stderr="remote unavailable")
        with patch.object(INSTALL, "run", return_value=failed):
            source = INSTALL.generate_page(HERE, self.config(), self.navigation / "config.json")
        self.assertEqual(source, "local-only-fallback")
        self.assertNotEqual(self.page.read_bytes(), template.read_bytes())

    def test_prepare_reports_integrated_contract_without_running_agent(self):
        config = self.config()
        locations = INSTALL.validate_config(config)
        args = INSTALL.parse_args(["--replace", "--no-autostart"])
        with patch.object(INSTALL, "load_config", return_value=config), patch.object(
                INSTALL, "effective_config", return_value=config), patch.object(
                INSTALL, "validate_config", return_value=locations), patch.object(
                INSTALL, "persist_config", return_value=locations["config_path"]), patch.object(
                INSTALL, "generate_page", return_value="preserved-existing"):
            result = INSTALL.prepare(args)
        self.assertTrue(result["integrated"])
        self.assertTrue(result["requires_agent_restart"])
        self.assertFalse(result["agent_started"])
        self.assertFalse(result["agent_restarted"])
        self.assertEqual(result["config"], str(locations["config_path"]))
        self.assertEqual(result["page"], str(self.page))
        self.assertEqual(result["port"], 3001)
        self.assertTrue(result["compatibility"]["replace"])
        self.assertTrue(result["compatibility"]["no_autostart_or_foreground"])
        self.assertNotIn("runtime", result)
        for removed in ("install_macos", "install_linux", "install_windows",
                        "update_nginx", "stage_runtime", "navigation_lifecycle"):
            self.assertFalse(hasattr(INSTALL, removed))

    def test_foreground_is_an_alias_and_never_changes_execution(self):
        args = INSTALL.parse_args(["--foreground"])
        self.assertTrue(args.no_autostart)

    def test_main_prints_integrated_contract(self):
        locations = INSTALL.validate_config(self.config())
        output = io.StringIO()
        with patch.object(INSTALL, "load_config", return_value=self.config()), patch.object(
                INSTALL, "effective_config", return_value=self.config()), patch.object(
                INSTALL, "validate_config", return_value=locations), patch.object(
                INSTALL, "persist_config", return_value=locations["config_path"]), patch.object(
                INSTALL, "generate_page", return_value="remote-or-authorized-catalog"), \
                contextlib.redirect_stdout(output):
            INSTALL.main([])
        result = json.loads(output.getvalue())
        self.assertTrue(result["integrated"])
        self.assertTrue(result["requires_agent_restart"])
        self.assertFalse(result["agent_started"])
        self.assertEqual(result["config"], str(locations["config_path"]))
        self.assertEqual(result["page"], str(self.page))


if __name__ == "__main__":
    unittest.main(verbosity=2)
