#!/usr/bin/env python3
"""Fail on a broken link between the docs: a relative link to a page that doesn't exist, or to
an anchor no heading on that page makes (GitHub's anchor rules).

    check_links.py      check README.md, docs/*.md and skills/**/*.md
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
LINK = re.compile(r"\]\(([^)\s]+)\)")
FENCE = re.compile(r"^(```|~~~).*?^\1", re.M | re.S)


def anchors(md):
    """The anchors a page's headings make, as GitHub makes them."""
    out, seen = set(), {}
    for h in re.findall(r"^#{1,6} (.+)$", FENCE.sub("", md), re.M):
        a = re.sub(r"[^\w\- ]", "", h.strip().lower()).replace(" ", "-")
        n = seen.get(a, 0)
        seen[a] = n + 1
        out.add(a if n == 0 else f"{a}-{n}")
    return out


def problems():
    found = []
    files = [ROOT / "README.md", *sorted((ROOT / "docs").glob("*.md")), *sorted((ROOT / "skills").rglob("*.md"))]
    cache = {}
    for f in files:
        md = re.sub(r"`[^`\n]*`", "", FENCE.sub("", f.read_text()))
        for target in LINK.findall(md):
            if re.match(r"^[a-z]+:", target) or target.startswith("/"):
                continue
            page, _, anchor = target.partition("#")
            path = (f.parent / page).resolve() if page else f
            if not path.exists():
                found.append(f"{f.relative_to(ROOT)}: `{target}`: no such file")
                continue
            if anchor and path.suffix == ".md":
                if path not in cache:
                    cache[path] = anchors(path.read_text())
                if anchor not in cache[path]:
                    found.append(f"{f.relative_to(ROOT)}: `{target}`: no heading makes `#{anchor}`")
    return found


if __name__ == "__main__":
    p = problems()
    for x in p:
        print(x)
    sys.exit(1 if p else 0)
