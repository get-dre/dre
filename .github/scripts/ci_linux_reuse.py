#!/usr/bin/env python3
"""Reuse Linux PR evidence only for an identical squash-merge tree; otherwise run tests."""
import json
import os
import re
import subprocess
import sys


def api(path):
    result = subprocess.run(["gh", "api", path], capture_output=True, text=True, check=True,
                            timeout=30)
    return json.loads(result.stdout)


def reusable_run(repo, sha, get=api):
    commit = get(f"repos/{repo}/git/commits/{sha}")
    if len(commit["parents"]) != 1:
        return None
    parent = commit["parents"][0]["sha"]
    for pr in get(f"repos/{repo}/commits/{sha}/pulls"):
        if not pr.get("merged_at") or pr["merge_commit_sha"] != sha:
            continue
        if pr["base"]["repo"]["full_name"] != repo:
            continue
        head = pr["head"]["sha"]
        if not re.fullmatch(r"[0-9a-f]{40}", head):
            continue
        tested = get(f"repos/{repo}/git/commits/{head}")
        if tested["tree"]["sha"] != commit["tree"]["sha"]:
            continue
        # The PR checkout merges its target into its head. Prove that target was already
        # included, so the checkout cannot have tested a different merge result.
        comparison = get(f"repos/{repo}/compare/{parent}...{head}")
        if comparison["status"] not in ("ahead", "identical"):
            continue
        runs = get(f"repos/{repo}/actions/workflows/ci.yml/runs?event=pull_request"
                   f"&head_sha={head}&status=success&per_page=10")
        for run in runs["workflow_runs"]:
            if (run["head_sha"] != head or run["event"] != "pull_request"
                    or run["status"] != "completed" or run["conclusion"] != "success"
                    or run["path"] != ".github/workflows/ci.yml"
                    or run["repository"]["full_name"] != repo
                    or run["head_repository"]["full_name"] != pr["head"]["repo"]["full_name"]):
                continue
            jobs = get(f"repos/{repo}/actions/runs/{int(run['id'])}/jobs?filter=latest&per_page=100")
            for job in jobs["jobs"]:
                if (job["name"] != "test-linux" or job["status"] != "completed"
                        or job["conclusion"] != "success"):
                    continue
                passed = {s["name"] for s in job["steps"] if s.get("conclusion") == "success"}
                required = {"Go tests", "Run cargo nextest run --workspace --no-fail-fast",
                            "Validate and run public examples against this build",
                            "Run cargo test --workspace --doc"}
                if required <= passed:
                    return run["html_url"]
    return None


def main():
    evidence = None
    if os.environ.get("GITHUB_EVENT_NAME") == "push" and os.environ.get("GITHUB_REF") == "refs/heads/master":
        try:
            evidence = reusable_run(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_SHA"])
        except (AttributeError, KeyError, TypeError, ValueError, OSError, subprocess.SubprocessError) as error:
            print(f"::notice::Could not verify Linux PR evidence; running full tests: {error}", file=sys.stderr)
    print(f"Linux: {'reuse ' + evidence if evidence else 'run full tests'}")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"linux_reuse={str(bool(evidence)).lower()}\n")


if __name__ == "__main__":
    main()
