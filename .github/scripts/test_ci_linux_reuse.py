#!/usr/bin/env python3
"""Check reuse against GitHub-shaped evidence, including every missing-coverage fallback."""
import os
import contextlib
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import ci_linux_reuse
from ci_linux_reuse import reusable_run

REPO = "example/engine"
MERGED, HEAD, PARENT = "a" * 40, "b" * 40, "c" * 40
PREFIX = f"repos/{REPO}"
RUNS = f"{PREFIX}/actions/workflows/ci.yml/runs?event=pull_request&head_sha={HEAD}&status=success&per_page=10"
JOBS = f"{PREFIX}/actions/runs/123/jobs?filter=latest&per_page=100"
STEPS = ["Go tests", "Run cargo nextest run --workspace --no-fail-fast",
         "Validate and run public examples against this build", "Run cargo test --workspace --doc"]


def evidence():
    return {
        f"{PREFIX}/git/commits/{MERGED}": {"parents": [{"sha": PARENT}], "tree": {"sha": "tree"}},
        f"{PREFIX}/commits/{MERGED}/pulls": [{"merged_at": "2026-01-01T00:00:00Z",
            "merge_commit_sha": MERGED, "base": {"repo": {"full_name": REPO}},
            "head": {"sha": HEAD, "repo": {"full_name": REPO}}}],
        f"{PREFIX}/git/commits/{HEAD}": {"tree": {"sha": "tree"}},
        f"{PREFIX}/compare/{PARENT}...{HEAD}": {"status": "ahead"},
        RUNS: {"workflow_runs": [{"id": 123, "head_sha": HEAD, "event": "pull_request",
            "status": "completed", "conclusion": "success", "path": ".github/workflows/ci.yml",
            "repository": {"full_name": REPO}, "head_repository": {"full_name": REPO},
            "html_url": "https://github.com/example/engine/actions/runs/123"}]},
        JOBS: {"jobs": [{"name": "test-linux", "status": "completed", "conclusion": "success",
                         "steps": [{"name": name, "conclusion": "success"} for name in STEPS]}]},
    }


class LinuxReuse(unittest.TestCase):
    def test_complete_identical_tree_reuses_passing_run(self):
        data = evidence()
        self.assertEqual(reusable_run(REPO, MERGED, data.__getitem__),
                         data[RUNS]["workflow_runs"][0]["html_url"])

    def test_changed_merge_tree_or_base_requires_full_tests(self):
        for change in ("tree", "diverged", "behind", "parents", "unmerged", "other_merge"):
            with self.subTest(change=change):
                data = evidence()
                if change == "tree":
                    data[f"{PREFIX}/git/commits/{HEAD}"]["tree"]["sha"] = "different"
                elif change in ("diverged", "behind"):
                    data[f"{PREFIX}/compare/{PARENT}...{HEAD}"]["status"] = change
                elif change == "parents":
                    data[f"{PREFIX}/git/commits/{MERGED}"]["parents"].append({"sha": HEAD})
                elif change == "unmerged":
                    data[f"{PREFIX}/commits/{MERGED}/pulls"][0]["merged_at"] = None
                else:
                    data[f"{PREFIX}/commits/{MERGED}/pulls"][0]["merge_commit_sha"] = HEAD
                self.assertIsNone(reusable_run(REPO, MERGED, data.__getitem__))

    def test_run_identity_and_success_are_required(self):
        changes = {"head_sha": MERGED, "event": "push", "status": "in_progress",
                   "conclusion": "failure", "path": ".github/workflows/other.yml",
                   "repository": {"full_name": "other/repo"},
                   "head_repository": {"full_name": "other/repo"}}
        for key, value in changes.items():
            with self.subTest(key=key):
                data = evidence()
                data[RUNS]["workflow_runs"][0][key] = value
                self.assertIsNone(reusable_run(REPO, MERGED, data.__getitem__))

    def test_skipped_failed_or_partial_linux_jobs_cannot_prove_coverage(self):
        for status in ("skipped", "failure", "cancelled", "timed_out"):
            data = evidence()
            data[JOBS]["jobs"][0]["conclusion"] = status
            self.assertIsNone(reusable_run(REPO, MERGED, data.__getitem__))
        for step in STEPS:
            with self.subTest(step=step):
                data = evidence()
                data[JOBS]["jobs"][0]["steps"] = [s for s in data[JOBS]["jobs"][0]["steps"]
                                                   if s["name"] != step]
                self.assertIsNone(reusable_run(REPO, MERGED, data.__getitem__))

    def test_missing_pr_run_or_job_requires_full_tests(self):
        for key, value in ((f"{PREFIX}/commits/{MERGED}/pulls", []),
                           (RUNS, {"workflow_runs": []}), (JOBS, {"jobs": []})):
            data = evidence()
            data[key] = value
            self.assertIsNone(reusable_run(REPO, MERGED, data.__getitem__))

    def test_cli_manual_runs_and_api_errors_default_to_full_tests(self):
        script = Path(__file__).with_name("ci_linux_reuse.py")
        with tempfile.TemporaryDirectory() as directory:
            for event in ("workflow_dispatch", "pull_request", "push"):
                output = Path(directory) / event
                env = {**os.environ, "GITHUB_EVENT_NAME": event, "GITHUB_REF": "refs/heads/master",
                       "GITHUB_OUTPUT": str(output)}
                env.pop("GITHUB_REPOSITORY", None)
                result = subprocess.run([os.sys.executable, str(script)], env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output.read_text(), "linux_reuse=false\n")

    def test_api_failures_and_malformed_evidence_fall_back_without_failing_scope(self):
        failures = [subprocess.CalledProcessError(1, ["gh", "api"]),
                    subprocess.TimeoutExpired(["gh", "api"], 30),
                    ValueError("invalid JSON"), KeyError("missing field"), TypeError("null repository"),
                    AttributeError("invalid PR entry")]
        with tempfile.TemporaryDirectory() as directory:
            for index, error in enumerate(failures):
                output = Path(directory) / str(index)
                env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/master",
                       "GITHUB_REPOSITORY": REPO, "GITHUB_SHA": MERGED, "GITHUB_OUTPUT": str(output)}
                with self.subTest(error=error), mock.patch.dict(os.environ, env, clear=True), \
                        mock.patch.object(ci_linux_reuse, "reusable_run", side_effect=error), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    ci_linux_reuse.main()
                self.assertEqual(output.read_text(), "linux_reuse=false\n")


if __name__ == "__main__":
    unittest.main()
