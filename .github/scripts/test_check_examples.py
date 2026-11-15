#!/usr/bin/env python3
"""Check public-example discovery and selection at the checker CLI boundary."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_examples.py")
EXAMPLES = SCRIPT.parents[2] / "examples"
# A stand-in CLI: logs each call's arguments, the env vars it was given and whether the
# overdue-loans seed exists, and succeeds.
FAKE_DRE = """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
if sys.argv[1:] == ["--version"]:
    print("dre 0.13.1")
    sys.exit(0)
project = Path(sys.argv[sys.argv.index("--project-dir") + 1])
with open(os.environ["FAKE_DRE_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps({"args": sys.argv[1:], "env": sorted(os.environ),
                          "seeded": (project / "reports/overdue/seed.sql").exists()}) + "\\n")
"""


def fake_dre(root):
    dre = root / "dre"
    dre.write_text(FAKE_DRE, encoding="utf-8")
    dre.chmod(0o755)
    return dre


class ExampleCli(unittest.TestCase):
    def test_lists_selected_projects_without_needing_a_cli(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--list", "--example", "data-tests"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), ["data-tests"])

    def test_unknown_selection_is_an_error(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--list", "--example", "typo"],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown example", result.stderr)

    def test_new_project_cannot_be_silently_omitted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / ".github/scripts/check_examples.py"
            script.parent.mkdir(parents=True)
            shutil.copy2(SCRIPT, script)
            new = root / "examples/new-project/dre_project.yml"
            new.parent.mkdir(parents=True)
            new.write_text("name: new_project\n")
            result = subprocess.run([sys.executable, str(script), "--list"], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("new-project", result.stderr)
            self.assertIn("register", result.stderr)


@unittest.skipIf(os.name == "nt", "the stand-in CLI is a POSIX script")
class ValidateOnly(unittest.TestCase):
    """Examples that need a service or an account are validated in every target, never run."""

    def calls(self, example):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "calls.jsonl"
            env = {k: v for k, v in os.environ.items() if not k.startswith(("DRE_", "PG", "DATABRICKS"))}
            env["FAKE_DRE_LOG"] = str(log)
            result = subprocess.run([sys.executable, str(SCRIPT), "--dre", str(fake_dre(root)),
                                     "--example", example], capture_output=True, text=True, env=env)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]

    def test_service_example_is_validated_in_every_target_and_not_run(self):
        calls = self.calls("warehouses")
        commands = [c["args"][0] for c in calls]
        self.assertNotIn("run", commands)
        self.assertTrue(any("--all-targets" in c["args"] for c in calls if c["args"][0] == "validate"))

    def test_credentials_get_inert_placeholders(self):
        calls = self.calls("warehouses")
        self.assertIn("DRE_SECRET_PG_PASSWORD", calls[0]["env"])
        self.assertIn("DATABRICKS_HOST", calls[0]["env"])

    def test_jdbc_example_is_prepared_then_validated(self):
        calls = self.calls("overdue-loans/oracle")
        self.assertTrue(calls and all(c["seeded"] for c in calls))
        self.assertEqual({c["args"][0] for c in calls}, {"validate"})


if __name__ == "__main__":
    unittest.main()
