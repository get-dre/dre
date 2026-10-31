#!/usr/bin/env python3
"""Run an ordinary database-backed report through DRE and inspect its workbook."""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "check_examples", ROOT.parents[1] / ".github/scripts/check_examples.py")
check_examples = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_examples)

EXPECTED = {
    "A2": "000019",
    "A3": "000020",
    "B2": "1234567890123456789012345678901234567890",
    "B3": "9876543210987654321098765432109876543210",
    "C2": 12.34,
    "C3": 0.0,
    "D2": "12345678901234567890.123456789012345678",
    "D3": -1e-18,
    "E2": 46053.04309170139,
    "E3": 46054.0,
    "F2": "2026/01/31",
    "F3": "2026/02/01",
    "G2": "002aff",
    "H3": 99.99,
}


class JdbcReport(unittest.TestCase):
    def setUp(self):
        self.database = os.environ["DRE_TEST_JDBC_DATABASE"]
        self.assertTrue(self.database in ("oracle", "sqlserver"))
        self.env = os.environ.copy()
        self.cli = self.env["DRE_BIN"]
        self.xlsx = Path(self.env["DRE_TEST_XLSX_BIN"])
        self.temporary = tempfile.TemporaryDirectory(prefix="dre-jdbc-report-")
        self.base = Path(self.temporary.name)
        self.plugins = self.base / "plugins with spaces"
        self.plugins.mkdir()
        shutil.copy2(ROOT / "dre-plugin-jdbc", self.plugins)
        (self.plugins / "target").symlink_to(ROOT / "target", target_is_directory=True)
        (self.plugins / "dre-plugin-xlsx").symlink_to(self.xlsx)
        self.env["DRE_PLUGINS_DIR"] = str(self.plugins)

    def tearDown(self):
        self.temporary.cleanup()

    def project(self, name="project"):
        project = self.base / name
        shutil.copytree(ROOT / "tests" / self.database, project)
        return project

    def run_report(self, project):
        return subprocess.run(
            [self.cli, "run", "accounts", "--preview"],
            cwd=project, env=self.env, capture_output=True, text=True, timeout=90)

    def assert_report(self, run, project):
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        outputs = list(project.glob("target/run/**/accounts.xlsx"))
        self.assertEqual(len(outputs), 1)
        cells = check_examples.workbook(outputs[0])["Accounts"]
        for cell, expected in EXPECTED.items():
            actual = cells[cell][0]
            if isinstance(expected, str) and cell.startswith(("G",)):
                actual = actual.lower()
            self.assertEqual(actual, expected, (cell, actual, expected))
        self.assertNotIn("G3", cells)
        self.assertNotIn("H2", cells)
        self.assertEqual(cells["C2"][1], "#,##0.00")
        self.assertEqual(cells["H3"][1], "#,##0.00")

    def test_shared_typed_rows_through_cli(self):
        project = self.project()
        self.assert_report(self.run_report(project), project)

    def test_complete_url_connects(self):
        project = self.project("complete-url")
        profile = project / "profiles.yml"
        url = self.env["DRE_TEST_JDBC_URL"]
        user = self.env["DRE_TEST_JDBC_USER"]
        password = self.env["DRE_TEST_JDBC_PASSWORD"]
        if self.database == "oracle":
            url = url.replace("jdbc:oracle:thin:@", f"jdbc:oracle:thin:{user}/{password}@")
        else:
            url = (f"{url};databaseName=master;user={user};password={password};"
                   "encrypt=false;trustServerCertificate=true")
        profile.write_text(
            "connections:\n  jdbc_fixture:\n    targets:\n      dev:\n        type: jdbc\n"
            f"        url: '{url}'\n"
            f"        driver_jars: '{self.env['DRE_TEST_JDBC_JAR']}'\n")
        run = self.run_report(project)
        self.assert_report(run, project)

    def test_failed_credentials_are_redacted(self):
        for complete_url in (False, True):
            project = self.project(f"secret-{complete_url}")
            self.env["DRE_TEST_JDBC_PASSWORD"] = "CLI_JDBC_PASSWORD_SENTINEL"
            self.env["DRE_TEST_JDBC_VENDOR_SECRET"] = "VENDOR_PROPERTY_SENTINEL"
            if complete_url:
                profile = project / "profiles.yml"
                url = self.env["DRE_TEST_JDBC_URL"]
                user = self.env["DRE_TEST_JDBC_USER"]
                if self.database == "oracle":
                    url = url.replace(
                        "jdbc:oracle:thin:@",
                        f"jdbc:oracle:thin:{user}/CLI_URL_SECRET_SENTINEL@")
                else:
                    url = (f"{url};databaseName=master;user={user};"
                           "password=CLI_URL_SECRET_SENTINEL;encrypt=false;"
                           "trustServerCertificate=true")
                profile.write_text(
                    "connections:\n  jdbc_fixture:\n    targets:\n      dev:\n        type: jdbc\n"
                    f"        url: '{url}'\n"
                    f"        driver_jars: '{self.env['DRE_TEST_JDBC_JAR']}'\n")
            run = self.run_report(project)
            self.assertNotEqual(run.returncode, 0)
            output = run.stdout + run.stderr
            self.assertIn("SQLSTATE", output)
            self.assertNotIn("SENTINEL", output)
            for path in (project / "target").rglob("*"):
                if path.is_file() and path.suffix in (".json", ".log", ".yml"):
                    self.assertNotIn("SENTINEL", path.read_text())

    def test_unsupported_timestamp_precision_fails_with_guidance(self):
        project = self.project("precision")
        sql = (
            "select timestamp '2026-01-31 01:02:03.123456' as precise from dual;\n"
            if self.database == "oracle"
            else "select cast('2026-01-31T01:02:03.1234' as datetime2(4)) as precise;\n"
        )
        (project / "reports/accounts/accounts.sql").write_text(sql)
        (project / "reports/accounts/accounts.yml").write_text(
            "queries: [accounts]\noutput: {format: xlsx}\n")
        run = self.run_report(project)
        output = run.stdout + run.stderr
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("cast explicitly to millisecond precision", output)
        self.assertNotIn("SENTINEL", output)


if __name__ == "__main__":
    unittest.main()
