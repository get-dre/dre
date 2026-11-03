#!/usr/bin/env python3
"""Reject separator assumptions in tests, so Windows fails on Windows, not in review.

Two Windows CI failures came from tests that assumed POSIX paths (`/east/runs/`) while DRE
printed native ones (`east\\runs\\`). The producer side of that class is fixed: a diagnostic
and a render error print their file through `dre_core::slash` (see `crates/dre-core/src/diag.rs`),
and machine-readable paths stay native. These rules catch the test-side habit that caused the
failures without flagging normalized-contract assertions, URLs, SQL or legitimate path literals.

- `format!("/{...` inside a `contains`/`starts_with`/`ends_with`/`find` call: a needle that
  starts with a separator. Compare path components instead: `path.split(['/', '\\\\'])`.
- `MAIN_SEPARATOR`: a test working around a native producer. Normalize at the producer instead.
- `"/Users/"`, `"/home/"`, `"/root/"`, `"C:\\\\`: a hardcoded home or Windows root.
- `split('/')` or `split("\\\\")` outside `tests/common/`: splitting a value on one separator.
  The shared helpers parse slash-form fixture paths by design.

Anything else (URLs, SQL, escaped strings, fixture paths joined with `/`) is accepted: only
these shapes are unambiguous. A bare slash needle against normalized output is correct by
contract, so it is not flagged.
"""

import argparse
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]

FORMATTED_NEEDLE = re.compile(r'\.(contains|starts_with|ends_with|find)\(\s*&?format!\(\s*"/\{')
WORKAROUND = re.compile(r"MAIN_SEPARATOR")
WINDOWS_ROOT = re.compile(r'"C:\\\\')
HOME_ROOT = re.compile(r'"(/Users/|/home/|/root/)')
SPLIT_ONE_SEPARATOR = tuple(
    f".{name}('{separator}')" for name in ("split", "rsplit") for separator in ("/", "\\\\")
)

RULES = (
    (FORMATTED_NEEDLE, "a formatted needle starts with a separator; compare path components with str.split(['/', '\\\\'])"),
    (WORKAROUND, "a native-separator workaround; make the producer print a normalized path (dre_core::slash)"),
    (WINDOWS_ROOT, "a hardcoded Windows root; use an isolated test home or a temporary directory"),
    (HOME_ROOT, "a hardcoded home directory; use an isolated test home or a temporary directory"),
)


def test_files(root: Path):
    files = set(root.glob("crates/*/tests/**/*.rs"))
    files |= set(root.glob("plugins/*/tests/**/*.rs"))
    return sorted(p for p in files if p.is_file())


def problems_in(relative: str, line: str):
    for pattern, message in RULES:
        if pattern.search(line):
            yield message
    # The shared helpers parse slash-form fixture paths by design.
    if "/tests/common/" not in "/" + relative and any(token in line for token in SPLIT_ONE_SEPARATOR):
        yield "splits a value on one separator; split on both: str.split(['/', '\\\\'])"


def check(root: Path):
    problems = []
    for path in test_files(root):
        relative = path.relative_to(root).as_posix()
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            for message in problems_in(relative, line):
                problems.append(f"{relative}:{number}: {message}")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root (default: this checkout)")
    args = parser.parse_args()
    problems = check(args.root)
    for problem in problems:
        print(problem)
    if problems:
        print(f"\n{len(problems)} portability problem(s); see .github/scripts/check_portable_tests.py", file=sys.stderr)
        return 1
    print(f"Portable test paths: {len(test_files(args.root))} files checked")
    return 0


if __name__ == "__main__":
    sys.exit(main())
