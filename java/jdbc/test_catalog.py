#!/usr/bin/env python3
"""Exercise the existing catalog CLI contract with the synthetic JDBC driver."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent


class CatalogCli(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="dre-jdbc-catalog-")
        base = Path(cls.temporary.name)
        subprocess.run(["javac", "-d", str(base), str(ROOT / "tests/FixtureDriver.java")], check=True)
        services = base / "META-INF/services"
        services.mkdir(parents=True)
        (services / "java.sql.Driver").write_text("fixture.FixtureDriver\n")
        cls.jar = base / "driver with spaces.jar"
        subprocess.run(["jar", "--create", "--file", str(cls.jar), "-C", str(base), "fixture",
                        "-C", str(base), "META-INF"], check=True)
        cls.cli = os.environ["DRE_BIN"]
        cls.plugins = base / "plugins with spaces"
        cls.plugins.mkdir()
        shutil.copy2(ROOT / "dre-plugin-jdbc", cls.plugins)
        (cls.plugins / "target").symlink_to(ROOT / "target", target_is_directory=True)

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def project(self, suffix):
        project = Path(self.temporary.name) / suffix
        project.mkdir()
        (project / "dre_project.yml").write_text("name: catalog\ndefault_profile: jdbc\n")
        (project / "dependencies.yml").write_text("plugins: [jdbc]\n")
        (project / "profiles.yml").write_text(
            "connections:\n  jdbc:\n    targets:\n      dev:\n        type: jdbc\n"
            "        url: \"jdbc:fixture:" + suffix + "\"\n"
            f"        driver_jars: '{self.jar}'\n"
            "        properties: {Mixed.Case: true, number: 42, decimal: '12345678901234567890.125'}\n")
        (project / "tables.json").write_text(json.dumps({"tables": ["sales schema.order items"], "schemas": []}))
        return project

    def catalog(self, project):
        env = os.environ.copy()
        env["DRE_PLUGINS_DIR"] = str(self.plugins)
        return subprocess.run(
            [self.cli, "catalog", "--profile", "jdbc", "--tables-file", "tables.json",
             "--output-file", "catalog.json"],
            cwd=project, env=env, capture_output=True, text=True, timeout=30)

    def test_existing_catalog_command_returns_mixed_case_metadata(self):
        project = self.project("catalog")
        run = self.catalog(project)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        catalog = json.loads((project / "catalog.json").read_text())
        self.assertEqual(catalog["schema_version"], "dre/catalog/v1")
        self.assertEqual(catalog["source_type"], "jdbc")
        self.assertEqual(catalog["dialect"]["identifier_case"], "upper_unless_quoted")
        self.assertEqual(catalog["tables"][0]["name"], "ORDER ITEMS")
        self.assertEqual(catalog["tables"][0]["columns"][0]["name"], "Order ID")

    def test_missing_comments_warn_and_permission_failure_is_redacted(self):
        no_comments = self.project("no-comments")
        run = self.catalog(no_comments)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        catalog = json.loads((no_comments / "catalog.json").read_text())
        self.assertIn("descriptions are unavailable", catalog["warnings"][0])

        denied = self.project("permission")
        (denied / "tables.json").write_text(json.dumps({"tables": [], "schemas": ["sales schema"]}))
        run = self.catalog(denied)
        output = run.stdout + run.stderr
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("schema metadata", output)
        self.assertIn("SQLSTATE 42501", output)
        self.assertNotIn("jdbc:fixture:permission", output)
        self.assertFalse((denied / "catalog.json").exists())


if __name__ == "__main__":
    unittest.main()
