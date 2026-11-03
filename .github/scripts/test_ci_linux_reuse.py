#!/usr/bin/env python3
"""Check reuse against GitHub-shaped evidence, including every missing-coverage fallback."""
import os
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile
from unittest import mock

import ci_linux_reuse
from ci_linux_reuse import reusable_run

REPO = "example/engine"
MERGED, HEAD = "a" * 40, "b" * 40
PREFIX = f"repos/{REPO}"
RUNS = f"{PREFIX}/actions/workflows/ci.yml/runs?event=pull_request&head_sha={HEAD}&status=success&per_page=10"
JOBS = f"{PREFIX}/actions/runs/123/jobs?filter=latest&per_page=100"
ARTIFACTS = f"{PREFIX}/actions/runs/123/artifacts?per_page=100"
STEPS = ["Go tests", "Run cargo nextest run --workspace --no-fail-fast",
         "Validate and run public examples against this build", "Run cargo test --workspace --doc"]


def evidence():
    return {
        f"{PREFIX}/git/commits/{MERGED}": {"tree": {"sha": "tree"}},
        f"{PREFIX}/commits/{MERGED}/pulls": [{"merged_at": "2026-01-01T00:00:00Z",
            "number": 12, "merge_commit_sha": MERGED, "base": {"repo": {"full_name": REPO}},
            "head": {"sha": HEAD, "repo": {"full_name": REPO}}}],
        RUNS: {"workflow_runs": [{"id": 123, "run_attempt": 1, "head_sha": HEAD, "event": "pull_request",
            "status": "completed", "conclusion": "success", "path": ".github/workflows/ci.yml",
            "repository": {"full_name": REPO}, "head_repository": {"full_name": REPO},
            "html_url": "https://github.com/example/engine/actions/runs/123"}]},
        JOBS: {"jobs": [{"name": "test-linux", "status": "completed", "conclusion": "success",
                         "steps": [{"name": name, "conclusion": "success"} for name in STEPS]}]},
        ARTIFACTS: {"artifacts": [{"id": 456, "name": "linux-tested-tree-1",
                                    "expired": False, "size_in_bytes": 512}]},
    }


def reuse(data, proof=None, filename="linux-evidence.json", corrupt=False):
    if proof is None:
        proof = {"tree": "tree", "pr": 12, "run_id": 123, "run_attempt": 1}
    zipped = io.BytesIO()
    with zipfile.ZipFile(zipped, "w", compression=zipfile.ZIP_DEFLATED) as output:
        output.writestr(filename, json.dumps(proof))
    content = bytearray(zipped.getvalue())
    if corrupt:
        content[30 + len(filename.encode())] = 0xff
    def download(path):
        assert path == f"{PREFIX}/actions/artifacts/456/zip", path
        return content
    return reusable_run(REPO, MERGED, data.__getitem__, download)


class LinuxReuse(unittest.TestCase):
    def test_complete_identical_tree_reuses_passing_run(self):
        data = evidence()
        self.assertEqual(reuse(data),
                         data[RUNS]["workflow_runs"][0]["html_url"])

    def test_changed_merge_tree_or_other_pr_requires_full_tests(self):
        for change in ("tree", "other_pr", "unmerged", "other_merge"):
            with self.subTest(change=change):
                data = evidence()
                if change == "tree":
                    data[f"{PREFIX}/git/commits/{MERGED}"]["tree"]["sha"] = "different"
                elif change == "other_pr":
                    data[f"{PREFIX}/commits/{MERGED}/pulls"][0]["number"] = 99
                elif change == "unmerged":
                    data[f"{PREFIX}/commits/{MERGED}/pulls"][0]["merged_at"] = None
                else:
                    data[f"{PREFIX}/commits/{MERGED}/pulls"][0]["merge_commit_sha"] = HEAD
                self.assertIsNone(reuse(data))

    def test_same_head_with_another_synthetic_merge_tree_is_not_evidence(self):
        proof = {"tree": "other-target-merge-tree", "pr": 12, "run_id": 123, "run_attempt": 1}
        self.assertIsNone(reuse(evidence(), proof))
        proof["tree"] = "tree"
        for key in ("pr", "run_id", "run_attempt"):
            with self.subTest(key=key):
                self.assertIsNone(reuse(evidence(), {**proof, key: 999}))

    def test_missing_expired_or_oversized_artifact_is_not_evidence(self):
        for change in ("missing", "expired", "size", "attempt"):
            data = evidence()
            artifact = data[ARTIFACTS]["artifacts"][0]
            if change == "missing":
                data[ARTIFACTS]["artifacts"] = []
            elif change == "expired":
                artifact["expired"] = True
            elif change == "size":
                artifact["size_in_bytes"] = 4097
            else:
                artifact["name"] = "linux-tested-tree-2"
            self.assertIsNone(reuse(data))
        self.assertIsNone(reuse(evidence(), filename="../untrusted.json"))
        self.assertIsNone(reuse(evidence(), {"tree": "x" * 2048}))

    def test_run_identity_and_success_are_required(self):
        changes = {"head_sha": MERGED, "event": "push", "status": "in_progress",
                   "conclusion": "failure", "path": ".github/workflows/other.yml",
                   "repository": {"full_name": "other/repo"},
                   "head_repository": {"full_name": "other/repo"}}
        for key, value in changes.items():
            with self.subTest(key=key):
                data = evidence()
                data[RUNS]["workflow_runs"][0][key] = value
                self.assertIsNone(reuse(data))

    def test_skipped_failed_or_partial_linux_jobs_cannot_prove_coverage(self):
        for status in ("skipped", "failure", "cancelled", "timed_out"):
            data = evidence()
            data[JOBS]["jobs"][0]["conclusion"] = status
            self.assertIsNone(reuse(data))
        for step in STEPS:
            with self.subTest(step=step):
                data = evidence()
                data[JOBS]["jobs"][0]["steps"] = [s for s in data[JOBS]["jobs"][0]["steps"]
                                                   if s["name"] != step]
                self.assertIsNone(reuse(data))

    def test_missing_pr_run_or_job_requires_full_tests(self):
        for key, value in ((f"{PREFIX}/commits/{MERGED}/pulls", []),
                           (RUNS, {"workflow_runs": []}), (JOBS, {"jobs": []})):
            data = evidence()
            data[key] = value
            self.assertIsNone(reuse(data))

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

    def test_recorded_checkout_round_trips_and_dirty_source_is_rejected(self):
        script = Path(__file__).with_name("ci_linux_reuse.py").resolve()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", str(root), *args],
                                               stderr=subprocess.DEVNULL, text=True).strip()
            git("init", "-q")
            source = root / "source.txt"
            source.write_text("tested source\n")
            git("add", "source.txt")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "commit", "-qm", "source")
            output = root / "proof.json"
            env = {**os.environ, "PR_NUMBER": "12", "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1"}
            subprocess.run([os.sys.executable, str(script), "--record", str(output)],
                           cwd=root, env=env, check=True)
            proof = json.loads(output.read_text())
            self.assertEqual(proof, {"tree": git("rev-parse", "HEAD^{tree}"), "pr": 12,
                                     "run_id": 123, "run_attempt": 1})
            data = evidence()
            data[f"{PREFIX}/git/commits/{MERGED}"]["tree"]["sha"] = proof["tree"]
            self.assertIsNotNone(reuse(data, proof))
            source.write_text("untested change\n")
            rejected = root / "dirty.json"
            result = subprocess.run([os.sys.executable, str(script), "--record", str(rejected)],
                                    cwd=root, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(rejected.exists())

    def test_api_failures_and_malformed_evidence_fall_back_without_failing_scope(self):
        failures = [subprocess.CalledProcessError(1, ["gh", "api"]),
                    subprocess.TimeoutExpired(["gh", "api"], 30),
                    ValueError("invalid JSON"), KeyError("missing field"), TypeError("null repository"),
                    AttributeError("invalid PR entry"), zipfile.BadZipFile("bad archive")]
        with tempfile.TemporaryDirectory() as directory:
            for index, error in enumerate(failures):
                output = Path(directory) / str(index)
                env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/master",
                       "GITHUB_REPOSITORY": REPO, "GITHUB_SHA": MERGED, "GITHUB_OUTPUT": str(output)}
                with self.subTest(error=error), mock.patch.dict(os.environ, env, clear=True), \
                        mock.patch.object(ci_linux_reuse, "reusable_run", side_effect=error), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    ci_linux_reuse.main([])
                self.assertEqual(output.read_text(), "linux_reuse=false\n")

    def test_corrupt_compressed_artifact_falls_back_to_full_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/master",
                   "GITHUB_REPOSITORY": REPO, "GITHUB_SHA": MERGED, "GITHUB_OUTPUT": str(output)}
            with mock.patch.dict(os.environ, env, clear=True), \
                    mock.patch.object(ci_linux_reuse, "reusable_run",
                                      side_effect=lambda *_: reuse(evidence(), corrupt=True)), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                ci_linux_reuse.main([])
            self.assertEqual(output.read_text(), "linux_reuse=false\n")


if __name__ == "__main__":
    unittest.main()
