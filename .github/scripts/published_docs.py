#!/usr/bin/env python3
"""Render published-release snippets: `check` offline, or `sync` after publication."""
import argparse
import json
from pathlib import Path
import re
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[2]
METADATA = ".github/doc-snippets/release.json"
SCHEMA_URL = re.compile(r"https://getdre.com/schemas/v\d+\.\d+/([\w-]+\.schema\.json)")


def read_json(url):
    with urlopen(Request(url, headers={"User-Agent": "dre-published-docs"}), timeout=30) as response:
        return json.load(response)


def latest_release():
    release = read_json("https://api.github.com/repos/get-dre/dre/releases/latest")
    tag = release["tag_name"]
    if release["draft"] or release["prerelease"] or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError(f"not a stable core release: {tag}")
    return tag


def rendered(root, tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError(f"not a stable core release: {tag}")
    pages = {}
    for template in sorted((root / ".github/doc-snippets").glob("*.txt")):
        name = template.stem
        path = root / f"docs/{name}.md"
        snippet = template.read_text().replace("{{DRE_RELEASE}}", tag)
        if "{{" in snippet.replace("${{", ""):
            raise ValueError(f"unresolved placeholder in {template}")
        pattern = re.compile(
            rf"(<!-- published-snippet:{name} -->\n```[^\n]*\n).*?(```\n<!-- /published-snippet:{name} -->)",
            re.DOTALL)
        text, count = pattern.subn(lambda m: m[1] + snippet + m[2], path.read_text())
        if count != 1:
            raise ValueError(f"expected one published snippet in {path}")
        pages[path] = text
    editor = root / "docs/editor-setup.md"
    minor = ".".join(tag[1:].split(".")[:2])
    pages[editor] = SCHEMA_URL.sub(lambda m: f"https://getdre.com/schemas/v{minor}/{m[1]}", editor.read_text())
    return pages


def sync(root):
    tag = latest_release()
    pages = rendered(root, tag)
    minor = ".".join(tag[1:].split(".")[:2])
    # Validate every distinct URL before changing any file; 200 alone can be an HTML fallback.
    urls = set(SCHEMA_URL.findall(pages[root / "docs/editor-setup.md"]))
    if not urls:
        raise ValueError("editor setup has no schema URLs")
    for name in sorted(urls):
        url = f"https://getdre.com/schemas/v{minor}/{name}"
        schema = read_json(url)
        if schema.get("x-dre-schema-version") != minor:
            raise ValueError(f"{url}: schema minor is not {minor}")
        print(f"verified {url}")
    for path, text in pages.items():
        path.write_text(text)
    (root / METADATA).write_text(json.dumps({"tag": tag}, indent=2) + "\n")


def check(root):
    tag = json.loads((root / METADATA).read_text())["tag"]
    problems = []
    for path, text in rendered(root, tag).items():
        if path.read_text() != text:
            problems.append(f"{path.relative_to(root)}: stale published snippet or schema URL")
    for path in (root / "docs").glob("*.md"):
        if "{{DRE_RELEASE}}" in path.read_text():
            problems.append(f"{path.relative_to(root)}: unresolved release placeholder")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "sync"))
    args = parser.parse_args()
    if args.command == "sync":
        sync(ROOT)
    elif problems := check(ROOT):
        raise SystemExit("\n".join(problems) + "\nRun published_docs.py sync after the release and schemas are live.")
    else:
        print("ok")


if __name__ == "__main__":
    main()
