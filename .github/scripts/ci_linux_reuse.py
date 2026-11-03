#!/usr/bin/env python3
"""Record the tested Linux checkout; reuse matching full-suite PR evidence on master."""
import argparse
import io
import json
import os
import re
import subprocess
import sys
from pathlib import Path
import zipfile


def api(path):
    result = subprocess.run(["gh", "api", path], capture_output=True, text=True, check=True,
                            timeout=30)
    return json.loads(result.stdout)


def archive(path):
    return subprocess.run(["gh", "api", path], capture_output=True, check=True, timeout=30).stdout


def reusable_run(repo, sha, get=api, download=archive):
    commit = get(f"repos/{repo}/git/commits/{sha}")
    for pr in get(f"repos/{repo}/commits/{sha}/pulls"):
        if not pr.get("merged_at") or pr["merge_commit_sha"] != sha:
            continue
        if pr["base"]["repo"]["full_name"] != repo:
            continue
        head = pr["head"]["sha"]
        if not re.fullmatch(r"[0-9a-f]{40}", head):
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
                if not required <= passed:
                    continue
                artifacts = get(f"repos/{repo}/actions/runs/{int(run['id'])}/artifacts?per_page=100")
                for artifact in artifacts["artifacts"]:
                    if (artifact["name"] != f"linux-tested-tree-{int(run['run_attempt'])}"
                            or artifact["expired"] or artifact["size_in_bytes"] > 4096):
                        continue
                    # Read bounded JSON only: never extract or execute a PR artifact.
                    data = download(f"repos/{repo}/actions/artifacts/{int(artifact['id'])}/zip")
                    with zipfile.ZipFile(io.BytesIO(data)) as zipped:
                        if zipped.namelist() != ["linux-evidence.json"]:
                            continue
                        if zipped.getinfo("linux-evidence.json").file_size > 1024:
                            continue
                        proof = json.loads(zipped.read("linux-evidence.json"))
                    if (proof == {"tree": commit["tree"]["sha"], "pr": pr["number"],
                                  "run_id": run["id"], "run_attempt": run["run_attempt"]}):
                        return run["html_url"]
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--record", type=Path, help="write the actual tested checkout and PR/run identity")
    args = parser.parse_args(argv)
    if args.record:
        subprocess.run(["git", "diff", "--exit-code", "--quiet", "HEAD", "--"], check=True)
        tree = subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], text=True).strip()
        proof = {"tree": tree, "pr": int(os.environ["PR_NUMBER"]),
                 "run_id": int(os.environ["GITHUB_RUN_ID"]),
                 "run_attempt": int(os.environ["GITHUB_RUN_ATTEMPT"])}
        args.record.write_text(json.dumps(proof), encoding="utf-8")
        return
    evidence = None
    if os.environ.get("GITHUB_EVENT_NAME") == "push" and os.environ.get("GITHUB_REF") == "refs/heads/master":
        try:
            evidence = reusable_run(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_SHA"])
        except Exception as error:
            # Any failure to establish proof requires full tests, including malformed archives.
            print(f"::notice::Could not verify Linux PR evidence; running full tests: {error}", file=sys.stderr)
    print(f"Linux: {'reuse ' + evidence if evidence else 'run full tests'}")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"linux_reuse={str(bool(evidence)).lower()}\n")


if __name__ == "__main__":
    main()
