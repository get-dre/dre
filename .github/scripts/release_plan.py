#!/usr/bin/env python3
"""What a release tag releases, for the release workflow.

    release_plan.py <tag>

`v<version>` releases DRE itself (the `dre` CLI); `<package>-v<version>` releases one plugin
package, e.g. `duckdb-v1.0.0` or `object_store-v1.2.0-rc.1`. The tag's version must be the one
in the source: the workspace version for DRE, the package's own `Cargo.toml` version for a Rust
plugin, the VERSION file in its folder for a Go one (packages.json names it under `go`), or
`pom.xml` for a Java one (names it under `java`).

Prints `key=value` lines for $GITHUB_OUTPUT: kind (core or plugin), package, version, prerelease
(true or false), lang (rust, go or java), and the package folder for go and java (empty for
the others).
"""

import io
import json
import pathlib
import re
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
PACKAGES = json.loads((ROOT / ".github/scripts/packages.json").read_text())
SEMVER = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?")
SCHEMA_FILES = (
    "project.schema.json",
    "report.schema.json",
    "sets.schema.json",
    "schedules.schema.json",
    "timings.schema.json",
    "profiles.schema.json",
    "dependencies.schema.json",
    "lookup.schema.json",
    "sources.schema.json",
)


def metadata(crate):
    """A workspace crate's metadata, as Cargo resolves it."""
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
    )
    return next(p for p in meta["packages"] if p["name"] == crate)


def cargo_version(crate):
    return metadata(crate)["version"]


def pom_version(path):
    text = (ROOT / path / "pom.xml").read_text()
    m = re.search(r"<version>([^<]+)</version>", text)
    if not m:
        sys.exit(f"{path}/pom.xml has no version")
    return m.group(1)


def crates_io(path):
    """A crates.io API response's body, or None for a 404."""
    req = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{path}",
        headers={"User-Agent": "dre-release (github.com/get-dre/dre)"},
    )
    try:
        with urllib.request.urlopen(req) as r:
            return r.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def protocol_changes():
    """How dre-protocol differs from the crates.io release of the same version, if there is one.

    The release publishes dre-protocol only when its version is new, and dre-core is then
    verified against the crates.io copy. So a change to its sources or dependencies (an arrow
    bump: arrow types are in its API) without a version bump fails the release at crates.io,
    after the builds. This catches it first.
    """
    meta = metadata("dre-protocol")
    crate, version = meta["name"], meta["version"]
    if crates_io(f"{crate}/{version}") is None:
        return []
    changes = []
    published = {
        d["crate_id"]: d["req"]
        for d in json.loads(crates_io(f"{crate}/{version}/dependencies"))["dependencies"]
        if d["kind"] == "normal"
    }
    local = {d["name"]: d["req"] for d in meta["dependencies"] if d["kind"] is None}
    for name in sorted(published.keys() | local.keys()):
        if published.get(name) != local.get(name):
            changes.append(f"dependency {name}: {published.get(name)} on crates.io, {local.get(name)} here")
    root = pathlib.Path(meta["manifest_path"]).parent
    with tarfile.open(fileobj=io.BytesIO(crates_io(f"{crate}/{version}/download"))) as tar:
        prefix = f"{crate}-{version}/"
        files = {
            m.name.removeprefix(prefix): tar.extractfile(m).read()
            for m in tar.getmembers()
            if m.isfile() and m.name.removeprefix(prefix).startswith("src/")
        }
    for rel in sorted(files.keys() | {str(f.relative_to(root)) for f in (root / "src").rglob("*") if f.is_file()}):
        here = root / rel
        if files.get(rel) != (here.read_bytes() if here.is_file() else None):
            changes.append(f"{rel} differs")
    return changes


def schema_version_problems(version, directory=None):
    """Check every committed YAML schema carries the candidate core minor version."""
    minor = ".".join(version.split(".")[:2])
    directory = directory or ROOT / "docs" / "schemas"
    problems = []
    for name in SCHEMA_FILES:
        path = directory / name
        if not path.is_file():
            problems.append(f"{name} is missing")
            continue
        try:
            document = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            problems.append(f"{name} is invalid JSON: {error}")
            continue
        actual = document.get("x-dre-schema-version") if isinstance(document, dict) else None
        if actual != minor:
            rendered = json.dumps(actual) if actual is not None else "missing"
            problems.append(
                f'{name} has x-dre-schema-version {rendered}; expected "{minor}"'
            )
    return problems


def plan(tag):
    if tag.startswith("v"):
        kind, package, version = "core", "dre", tag[1:]
        cli = metadata("dre-cli")
        source, where = cli["version"], "the workspace version"
    else:
        m = re.fullmatch(r"([a-z0-9_]+)-v(.+)", tag)
        if not m:
            sys.exit(f"tag {tag}: expected v<version> (DRE) or <package>-v<version> (a plugin)")
        kind, (package, version) = "plugin", m.groups()
        if package not in PACKAGES:
            sys.exit(f"tag {tag}: `{package}` isn't a plugin package (.github/scripts/packages.json)")
        about = PACKAGES[package]
        go, java = about.get("go"), about.get("java")
        if go:
            source, where = (ROOT / go / "VERSION").read_text().strip(), f"{go}/VERSION"
        elif java:
            source, where = pom_version(java), f"{java}/pom.xml"
        else:
            source = cargo_version(f"dre-plugin-{package}")
            where = f"plugins/{package}/Cargo.toml"
    if not SEMVER.fullmatch(version):
        sys.exit(f"tag {tag}: `{version}` isn't a version")
    if version != source:
        sys.exit(f"tag {tag} is version {version}, but {where} says {source}")
    if kind == "core":
        schema_problems = schema_version_problems(version)
        if schema_problems:
            sys.exit(
                f"tag {tag}: committed JSON schemas do not match DRE {'.'.join(version.split('.')[:2])}:\n  "
                + "\n  ".join(schema_problems)
                + "\nRegenerate the schemas and their reference copies with:\n"
                "  DRE_UPDATE_SCHEMAS=1 cargo test -p dre-cli --test schemas the_generated_schemas_are_committed -- --exact\n"
                "  python3 .github/scripts/schema_docs.py generate\n"
                "  python3 .github/scripts/skills.py sync\n"
                "  python3 .github/scripts/skills.py generate\n"
                "Then verify both focused schema tests before release CI."
            )
        # dre-cli, as published to crates.io, must depend on the dre-core of the same release.
        req = next(d["req"] for d in cli["dependencies"] if d["name"] == "dre-core")
        if req != f"={version}":
            sys.exit(f"tag {tag}: the workspace's dre-core dependency is `{req}`; make it `={version}` in Cargo.toml")
        changes = protocol_changes()
        if changes:
            v = cargo_version("dre-protocol")
            sys.exit(
                f"tag {tag}: dre-protocol {v} is already on crates.io, but it has changed since:\n  "
                + "\n  ".join(changes)
                + "\nBump its version in crates/dre-protocol/Cargo.toml and the workspace's Cargo.toml."
            )
    return {
        "kind": kind,
        "package": package,
        "version": version,
        "prerelease": str("-" in version).lower(),
        "go": PACKAGES.get(package, {}).get("go", ""),
        "lang": ("go" if PACKAGES.get(package, {}).get("go")
                 else "java" if PACKAGES.get(package, {}).get("java") else "rust"),
        "java": PACKAGES.get(package, {}).get("java", ""),
    }


if __name__ == "__main__":
    for k, v in plan(sys.argv[1]).items():
        print(f"{k}={v}")
