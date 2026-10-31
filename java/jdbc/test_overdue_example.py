#!/usr/bin/env python3
"""Exercise the downloadable example through the ordinary JDBC report CLI."""

import importlib.util
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("jdbc_report", ROOT / "java/jdbc/test_jdbc_report.py")
jdbc_report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(jdbc_report)
spec = importlib.util.spec_from_file_location("package_jdbc", ROOT / ".github/scripts/package_jdbc.py")
package_jdbc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_jdbc)


class OverdueExample(jdbc_report.JdbcReport):
    # Inherit the existing plugin-host setup, not the unrelated account tests.
    def test_downloadable_project(self):
        example = self.base / "overdue-loans"
        archive = self.base / "overdue-loans.tar.gz"
        subprocess.run([sys.executable, str(ROOT / "examples/overdue-loans/package.py"),
                        "--output", str(archive)], check=True)
        with tarfile.open(archive) as download:
            download.extractall(self.base, filter="data")
        # Exercise an installed package, not the development-launcher override.
        self.assertNotEqual(os.name, "nt", "this database test uses the POSIX package")
        plugin = self.base / "jdbc.tar.gz"
        target = ("macos" if sys.platform == "darwin" else "linux") + "-" + platform.machine().replace("arm64", "aarch64")
        package_jdbc.package(plugin, platform=target,
                             jar=ROOT / "java/jdbc/target/dre-plugin-jdbc.jar",
                             launcher=ROOT / "java/jdbc/launcher/dre-plugin-jdbc",
                             license=ROOT / "LICENSE", notice=ROOT / "NOTICE",
                             third_party=ROOT / "THIRD_PARTY_NOTICES.md")
        (self.plugins / "dre-plugin-jdbc").unlink()
        registry = self.base / "registry.json"
        registry.write_text(json.dumps({"schema": 2, "plugins": [{
            "name": "jdbc", "provides": ["source/jdbc"], "versions": [{
                "version": "1.0.0", "protocol": 1, "artifacts": {target: {
                    "url": str(plugin), "sha256": hashlib.sha256(plugin.read_bytes()).hexdigest()}}}]}]}))
        self.env["DRE_REGISTRY_URL"] = str(registry)
        installed = subprocess.run([self.cli, "plugin", "install", "jdbc"], cwd=example / self.database,
                                   env=self.env, capture_output=True, text=True, timeout=90)
        self.assertEqual(installed.returncode, 0, installed.stdout + installed.stderr)
        self.env["DRE_TEST_JDBC_ENCRYPT"] = "false"
        self.env["DRE_TEST_JDBC_TRUST_CERTIFICATE"] = "true"
        subprocess.run([sys.executable, str(example / "check.py"), self.database,
                        "--dre", self.cli], env=self.env, check=True, timeout=300)


if __name__ == "__main__":
    unittest.main(defaultTest="OverdueExample.test_downloadable_project")
