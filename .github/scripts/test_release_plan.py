#!/usr/bin/env python3
"""Tests for release_plan.py: python3 .github/scripts/test_release_plan.py"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import release_plan  # noqa: E402


class Plan(unittest.TestCase):
    def test_jdbc_reads_its_pom_and_reports_java(self):
        self.assertEqual(
            release_plan.plan("jdbc-v1.0.0"),
            {
                "kind": "plugin",
                "package": "jdbc",
                "version": "1.0.0",
                "prerelease": "false",
                "go": "",
                "lang": "java",
                "java": "java/jdbc",
            },
        )


if __name__ == "__main__":
    unittest.main()
