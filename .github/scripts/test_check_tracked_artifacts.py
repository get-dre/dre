#!/usr/bin/env python3
"""Tests for check_tracked_artifacts.py: python3 .github/scripts/test_check_tracked_artifacts.py"""

import os
import pathlib
import subprocess
import tempfile
import unittest

import check_tracked_artifacts as c


class TrackedArtifacts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.tmp.name)
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.root), *args], check=True, capture_output=True)

    def write(self, name, data=b"text\n", executable=False):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        if executable:
            os.chmod(path, 0o755)

    def add(self):
        self.git("add", "-A", "-f")

    def test_a_small_text_file_is_fine(self):
        self.write("src/main.rs")
        self.add()
        self.assertEqual(c.problems(self.root), [])

    def test_a_file_over_one_mib_fails(self):
        self.write("big.txt", b"x" * (c.MAX_BYTES + 1))
        self.add()
        [problem] = c.problems(self.root)
        self.assertIn("over the 1 MiB limit", problem)

    def test_a_compiled_binary_fails(self):
        self.write("plugin", b"\x7fELF" + b"\0" * 8)
        self.add()
        [problem] = c.problems(self.root)
        self.assertIn("compiled binary", problem)

    def test_a_binary_suffix_fails(self):
        self.write("module.pyc", b"not really bytecode")
        self.add()
        [problem] = c.problems(self.root)
        self.assertIn("compiled binary", problem)

    def test_an_uppercase_binary_suffix_fails(self):
        self.write("module.PYC", b"not really bytecode")
        self.add()
        [problem] = c.problems(self.root)
        self.assertIn("compiled binary", problem)

    def test_an_executable_without_a_shebang_fails(self):
        self.write("plugin", b"binary without magic", executable=True)
        self.add()
        [problem] = c.problems(self.root)
        self.assertIn("executable but not a script", problem)

    def test_an_executable_script_is_fine(self):
        self.write("run.sh", b"#!/bin/sh\nexit 0\n", executable=True)
        self.add()
        self.assertEqual(c.problems(self.root), [])

    def test_an_untracked_artifact_is_ignored(self):
        self.write("plugin", b"\x7fELF" + b"\0" * 8)
        self.assertEqual(c.problems(self.root), [])

    def test_a_symlink_is_ignored(self):
        self.write("target.txt")
        (self.root / "link").symlink_to("target.txt")
        self.add()
        self.assertEqual(c.problems(self.root), [])


if __name__ == "__main__":
    unittest.main()
