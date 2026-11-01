#!/usr/bin/env python3
"""Tests for release_plan.py: python3 .github/scripts/test_release_plan.py"""

import json
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import release_plan  # noqa: E402


class Plan(unittest.TestCase):
    def write_schemas(self, directory, version, *, missing=()):
        directory.mkdir(parents=True, exist_ok=True)
        for name in release_plan.SCHEMA_FILES:
            if name not in missing:
                (directory / name).write_text(json.dumps({"x-dre-schema-version": version}))

    def test_core_schema_preflight_rejects_stale_minor_stamps(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.write_schemas(directory, "0.9")

            problems = release_plan.schema_version_problems("0.10.0", directory)

        self.assertEqual(len(problems), len(release_plan.SCHEMA_FILES))
        self.assertIn('project.schema.json has x-dre-schema-version "0.9"; expected "0.10"', problems)

    def test_core_schema_preflight_rejects_missing_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.write_schemas(directory, "0.10", missing=("project.schema.json",))

            problems = release_plan.schema_version_problems("0.10.0", directory)

        self.assertEqual(problems, ["project.schema.json is missing"])

    def test_core_schema_preflight_accepts_matching_minor_stamps(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            self.write_schemas(directory, "0.10")

            problems = release_plan.schema_version_problems("0.10.0", directory)

        self.assertEqual(problems, [])

    @patch("release_plan.protocol_changes")
    @patch("release_plan.metadata", return_value={
        "version": "0.10.0",
        "dependencies": [{"name": "dre-core", "req": "=0.10.0"}],
    })
    @patch("release_plan.schema_version_problems", return_value=[
        'project.schema.json has x-dre-schema-version "0.9"; expected "0.10"',
    ])
    def test_core_plan_rejects_stale_schemas_before_registry_checks(
        self, schema_check, metadata, protocol_check
    ):
        with self.assertRaisesRegex(SystemExit, "project.schema.json"):
            release_plan.plan("v0.10.0")

        protocol_check.assert_not_called()

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
