#!/usr/bin/env python3
"""Exercise the classifier CLI against real git diffs, including deletions and renames."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("ci_scope.py")


class ScopeCli(unittest.TestCase):
    def test_job_selection(self):
        cases = [
            (["docs/building-reports.md"], False, []),
            (["docs/schemas/project.json", "skills/dre-run/references/cli.md"], False, []),
            (["examples/tutorial/reports/sales/sales.sql"], False, ["tutorial"]),
            (["examples/new-project/dre_project.yml"], False, ["new-project"]),
            (["docs/first-report.md"], False, ["tutorial"]),
            (["docs/examples.md"], False, ["*"]),
            (["examples/tutorial/dre_project.yml", "crates/dre-cli/src/main.rs"], True, ["tutorial"]),
            ([".github/workflows/ci.yml"], True, []),
            ([".github/scripts/check_examples.py"], True, []),
            (["unknown/config.yml"], True, []),
            ([f"docs/page-{i}.md" for i in range(3001)] +
             ["examples/tutorial/reports/sales/sales.sql"], False, ["tutorial"]),
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            git("commit", "--allow-empty", "-qm", "base")
            base = git("rev-parse", "HEAD")
            for paths, full, examples in cases:
                with self.subTest(paths=paths):
                    git("reset", "--hard", base)
                    git("clean", "-fdq")
                    for name in paths:
                        path = root / name
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_text("fixture\n")
                    git("add", ".")
                    git("commit", "-qm", "change")
                    output = root / "output"
                    result = subprocess.run([os.sys.executable, str(SCRIPT), "--base", base],
                                            cwd=root, env={**os.environ, "GITHUB_OUTPUT": str(output)},
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads(result.stdout), {"full": full, "examples": examples})
                    self.assertIn(f"full={str(full).lower()}\n", output.read_text())
            # Rename a source file into docs: the removed source still requires full CI.
            source = root / "unknown/config.yml"
            source.rename(root / "notes.md")
            git("add", "-A")
            previous = git("rev-parse", "HEAD")
            git("commit", "-qm", "rename")
            result = subprocess.check_output([os.sys.executable, str(SCRIPT), "--base", previous], cwd=root)
            self.assertTrue(json.loads(result)["full"])
            for base_arg in ([], ["--base", "missing"], ["--base", "0" * 40]):
                result = subprocess.check_output([os.sys.executable, str(SCRIPT), *base_arg], cwd=root)
                self.assertTrue(json.loads(result)["full"])


if __name__ == "__main__":
    unittest.main()
