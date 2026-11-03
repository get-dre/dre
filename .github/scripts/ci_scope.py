#!/usr/bin/env python3
"""Select CI jobs from a complete git diff; unknown files retain full coverage."""
import argparse
import json
import os
from pathlib import PurePosixPath
import subprocess


def scope(paths):
    full, examples = False, set()
    for path in paths:
        parts = PurePosixPath(path).parts
        if path == "docs/first-report.md":
            examples.add("tutorial")
        elif path in ("docs/examples.md", "examples/README.md"):
            examples.add("*")
        elif len(parts) >= 3 and parts[0] == "examples":
            examples.add(parts[1])
        elif path.endswith(".md") or path.startswith("docs/") or path == "LICENSE":
            continue
        elif path.startswith(("skills/", ".claude-plugin/")) and path.endswith(".json"):
            continue
        else:
            full = True
    return {"full": full, "examples": sorted(examples)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    args = parser.parse_args()
    try:
        if not args.base or not args.base.strip("0"):
            raise ValueError("no comparable base")
        result = subprocess.run(
            ["git", "diff", "--name-only", "--no-renames", "-z", args.base, args.head, "--"],
            check=True, capture_output=True)
        paths = os.fsdecode(result.stdout).split("\0")
        selected = scope([p for p in paths if p])
    except (ValueError, subprocess.CalledProcessError):
        selected = {"full": True, "examples": []}
    print(json.dumps(selected))
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"full={str(selected['full']).lower()}\n")
            stream.write(f"examples={json.dumps(selected['examples'])}\n")


if __name__ == "__main__":
    main()
