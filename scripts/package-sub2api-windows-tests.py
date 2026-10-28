#!/usr/bin/env python3
"""Verify fixed-relay distribution guards without using real credentials."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("package_sub2api", Path(__file__).with_name("package-sub2api-windows.py"))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class ProfileTests(unittest.TestCase):
    def check_profile(self, profile, binary=None):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "profile.json"
            encoded = json.dumps(profile).encode()
            path.write_bytes(encoded)
            package.validate_profile(path, b"executable" + encoded if binary is None else binary)

    def test_rejects_missing_or_blank_relay_even_if_embedded(self):
        for relay in [None, "", " ", 123]:
            with self.subTest(relay=relay), self.assertRaisesRegex(SystemExit, "fixed relay_server"):
                self.check_profile({"relay_server": relay})

    def test_rejects_wrong_executable(self):
        with self.assertRaisesRegex(SystemExit, "does not contain"):
            self.check_profile({"relay_server": "192.0.2.1:8900"}, b"another executable")

    def test_accepts_matching_profile_with_fixed_relay(self):
        self.check_profile({"relay_server": "192.0.2.1:8900"})


if __name__ == "__main__":
    unittest.main()
