#!/usr/bin/env python3
"""Exercise the portability checker against the two Windows failure shapes and permitted code."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_portable_tests.py")

# The assertion that failed on Windows in `concurrent_runs_isolate_evidence_by_binding_and_run`
# before 2282650, and the workaround 2282650/ee90575 added for the diagnostic path.
HISTORICAL = """\
fn legacy() {
    let path = String::from("east\\\\runs\\\\x");
    assert!(path.contains(&format!("/{binding}/runs/")), "{path}");
    let needle = "checks/macros/check.sql:2:".replace('/', std::path::MAIN_SEPARATOR_STR);
    let home = "/Users/someone/project";
    let windows = "C:\\\\Users\\\\someone";
    let parts: Vec<&str> = path.split('/').collect();
}
"""

ACCEPTED = """\
fn portable() {
    let segments: Vec<&str> = path.split(['/', '\\\\']).collect();
    assert!(segments.windows(2).any(|w| w == [binding, dre_core::runs::RUNS]));
    assert!(pdf.contains("/Title (Acme)"));
    assert!(local.replace('\\\\', "/").ends_with("/detail.zip"));
    assert!(url.contains(&format!("https://example.invalid/{id}")));
    assert!(out.contains("1\\\\n"));
}
"""


def write(root: Path, relative: str, text: str):
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


class CheckPortableTests(unittest.TestCase):
    def run_check(self, root: Path):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root)], capture_output=True, text=True
        )

    def test_accepted_code_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(root, "crates/dre-cli/tests/portable.rs", ACCEPTED)
            write(root, "crates/dre-cli/tests/common/mod.rs", "fn helper(rel: &str) { let _ = rel.split('/'); }\n")
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("2 files checked", result.stdout)

    def test_the_historical_failures_are_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(root, "crates/dre-cli/tests/data_tests.rs", HISTORICAL)
            result = self.run_check(root)
            self.assertEqual(result.returncode, 1)
            messages = result.stdout.splitlines()
            self.assertTrue(any("crates/dre-cli/tests/data_tests.rs:3:" in m for m in messages), messages)
            self.assertTrue(any(":4:" in m for m in messages), messages)
            self.assertTrue(any(":5:" in m for m in messages), messages)
            self.assertTrue(any(":6:" in m for m in messages), messages)
            self.assertTrue(any(":7:" in m for m in messages), messages)
            self.assertIn("5 portability problem(s)", result.stderr)

    def test_an_empty_tree_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_check(Path(directory))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("0 files checked", result.stdout)


if __name__ == "__main__":
    unittest.main()
