#!/usr/bin/env python3
"""Keep the PyPI and crates.io pages in step with the README.

    package_readme.py sync     write crates/dre-cli/README.md from the README
    package_readme.py check    every CI check (changes nothing)

What DRE is gets written once, in README.md between `<!-- package-description:start -->` and
`<!-- package-description:end -->`. The `dre-cli` pages on PyPI and crates.io are built from that
intro, with relative links made absolute (`docs/<page>.md` goes to getdre.com/docs/<page>/), plus
their own install lines:

- PyPI: build_wheels.py calls `pypi_readme()` for the wheel's long description.
- crates.io: `sync` writes crates/dre-cli/README.md, which Cargo publishes.

The one-line summary and the keywords live in .github/package-description.json: build_wheels.py
reads them, and `check` fails when crates/dre-cli/Cargo.toml's `description` or `keywords` differ,
or when crates/dre-cli/README.md is stale. The registries only show new text after a release.
"""

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
REPO = "get-dre/dre"
SITE = "https://getdre.com"
START, END = "<!-- package-description:start -->", "<!-- package-description:end -->"
META_FILE = ROOT / ".github/package-description.json"
CRATE_README = ROOT / "crates/dre-cli/README.md"
CRATE_TOML = ROOT / "crates/dre-cli/Cargo.toml"


def meta():
    return json.loads(META_FILE.read_text())


def intro(readme):
    """The README text between the package-description markers."""
    m = re.search(re.escape(START) + r"\n(.*?)\n" + re.escape(END), readme, re.S)
    if not m:
        sys.exit(f"README.md has no {START} ... {END} block")
    return m.group(1).strip()


def absolute_links(text):
    """Relative Markdown links made absolute: docs pages to the site, anything else to GitHub."""

    def fix(m):
        href = m.group(1)
        if re.match(r"^(https?:|mailto:|#)", href):
            return m.group(0)
        path, _, hash_ = href.partition("#")
        anchor = f"#{hash_}" if hash_ else ""
        page = re.fullmatch(r"docs/([\w.-]+)\.md", path)
        if page:
            return f"]({SITE}/docs/{page.group(1)}/{anchor})"
        return f"](https://github.com/{REPO}/blob/master/{path}{anchor})"

    return re.sub(r"\]\(([^)\s]+)\)", fix, text)


def pypi_readme(readme, dre_version):
    """The `dre-cli` wheel's long description (Markdown)."""
    return f"""# dre-cli

{absolute_links(intro(readme))}

```bash
pip install dre-cli        # or: uv tool install dre-cli
dre --help
```

This package holds the `dre` {dre_version} executable. Plugins (databases, file formats,
destinations) are installed by `dre` on demand, for the ones a project declares. From Python:
`dre_cli.run(["run", "-s", "daily"])`.

In a Databricks job, add `dre-cli` to the job's environment dependencies and run `dre` (or
`python -m dre_cli`) from a script or notebook.

Documentation: [getdre.com/docs]({SITE}/docs/). Source and issues: [github.com/{REPO}](https://github.com/{REPO}).
"""


def crate_readme(readme):
    """crates/dre-cli/README.md, which crates.io shows."""
    return f"""<!-- Generated from README.md by .github/scripts/package_readme.py sync: edit the README. -->
# dre-cli

{absolute_links(intro(readme))}

```bash
cargo install dre-cli --locked
dre --help
```

Sources, formats and destinations are plugins that `dre` downloads on demand for the projects that
declare them. Other ways to install DRE (install.sh, pip, Homebrew, Scoop) are in the
[install guide]({SITE}/docs/install/).

Documentation: [getdre.com/docs]({SITE}/docs/). Source and issues: [github.com/{REPO}](https://github.com/{REPO}).
"""


def cargo_field(toml, key):
    """A string or list-of-strings field of Cargo.toml's [package] table."""
    package = re.search(r"^\[package\]\n(.*?)(?=^\[|\Z)", toml, re.S | re.M)
    m = re.search(rf'^{key}\s*=\s*(.+)$', package.group(1) if package else "", re.M)
    if not m:
        return None
    return json.loads(m.group(1))


def problems(readme, crate_readme_text, cargo_toml, meta_):
    found = []
    if crate_readme_text != crate_readme(readme):
        found.append("crates/dre-cli/README.md is stale: run .github/scripts/package_readme.py sync")
    if cargo_field(cargo_toml, "description") != meta_["summary"]:
        found.append("crates/dre-cli/Cargo.toml description differs from .github/package-description.json summary")
    if cargo_field(cargo_toml, "keywords") != meta_["keywords"]:
        found.append("crates/dre-cli/Cargo.toml keywords differ from .github/package-description.json keywords")
    if len(meta_["keywords"]) > 5:
        found.append("crates.io takes at most 5 keywords")
    return found


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    readme = (ROOT / "README.md").read_text()
    if cmd == "sync":
        CRATE_README.write_text(crate_readme(readme))
        print(f"wrote {CRATE_README.relative_to(ROOT)}")
    elif cmd == "check":
        found = problems(readme, CRATE_README.read_text(), CRATE_TOML.read_text(), meta())
        for p in found:
            print(f"error: {p}", file=sys.stderr)
        if found:
            sys.exit(1)
        print("package pages ok")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
