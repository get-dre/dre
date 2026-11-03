#!/usr/bin/env python3
"""Check public-example discovery and selection at the checker CLI boundary."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_examples.py")


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


if __name__ == "__main__":
    unittest.main()
