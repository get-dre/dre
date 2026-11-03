#!/usr/bin/env python3
"""Exercise the Linux-only guard with complete, absent and empty emulator settings."""
from pathlib import Path
import subprocess
import sys
import unittest

from check_emulator_env import REQUIRED_ENV

SCRIPT = Path(__file__).with_name("check_emulator_env.py")


class EmulatorEnvironment(unittest.TestCase):
    def run_guard(self, env):
        return subprocess.run([sys.executable, str(SCRIPT)], env=env, capture_output=True, text=True)

    def test_complete_environment_succeeds_without_printing_values(self):
        result = self.run_guard(dict.fromkeys(REQUIRED_ENV, "private-value"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout + result.stderr, "")

    def test_each_missing_or_empty_variable_fails(self):
        for name in REQUIRED_ENV:
            for value in (None, ""):
                with self.subTest(name=name, value=value):
                    env = dict.fromkeys(REQUIRED_ENV, "private-value")
                    if value is None:
                        del env[name]
                    else:
                        env[name] = value
                    result = self.run_guard(env)
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn(name, result.stderr)
                    self.assertNotIn("private-value", result.stdout + result.stderr)

    def test_guard_runs_only_in_linux_before_go_and_rust_tests(self):
        workflow = SCRIPT.parent.parent.joinpath("workflows/ci.yml").read_text()
        linux = workflow.split("\n  test-linux:\n", 1)[1].split("\n  test:\n", 1)[0]
        guard = "python3 .github/scripts/check_emulator_env.py"
        self.assertEqual(workflow.count(guard), 1)
        self.assertIn(guard, linux)
        self.assertLess(linux.index("name: Start the SSH bastion"), linux.index(guard))
        self.assertLess(linux.index(guard), linux.index("name: Go tests"))
        self.assertLess(linux.index(guard), linux.index("cargo nextest run"))


if __name__ == "__main__":
    unittest.main()
