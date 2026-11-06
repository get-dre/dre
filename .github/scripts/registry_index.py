#!/usr/bin/env python3
"""Bring the registry index (docs/registry.md) up to date with the plugin releases and print it.

    registry_index.py <old packages.json, may be missing> <owner/repo>

Every published plugin release (tag `<package>-v<version>`) that the index doesn't list yet is
added: each `dre-plugin-<package>-<version>-<platform>.tar.gz` asset becomes an artifact of that
version, with the checksum from the release's SHA256SUMS. Everything already in the index is
kept as it is, including the versions older DRE releases published.

Adding every missing release, not only the one just made, is what makes concurrent releases safe:
when several plugin tags are pushed at once, GitHub cancels all but the last queued index update,
and the last one adds them all.

What each package provides, and its description, come from packages.json next to this script.
GH_TOKEN (or GITHUB_TOKEN) is sent to the GitHub API when set.
"""

import json
import os
import pathlib
import re
import sys
import urllib.request
import urllib.error
from release_trust import verify_archive

YANKS = json.loads(pathlib.Path(__file__).with_name("registry_yanks.json").read_text())

PROTOCOL = 0
SCHEMA = 2
API = os.environ.get("GITHUB_API_URL", "https://api.github.com")
TAG = re.compile(r"^([a-z0-9_]+)-v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$")


def get(url):
    req = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json", "User-Agent": "dre-registry"})
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token and url.startswith(API):
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def releases(repo):
    page = 1
    while True:
        batch = json.loads(get(f"{API}/repos/{repo}/releases?per_page=100&page={page}"))
        yield from batch
        if len(batch) < 100:
            return
        page += 1


def artifacts(release, package, version, extension=False, repo=None):
    """The release's archives with their SHA256SUMS checksums; None if it isn't complete. An
    extension archive's `<archive>.minisig` asset, when published, goes in as its `minisig`."""
    assets = {a["name"]: a["browser_download_url"] for a in release.get("assets", [])}
    if "SHA256SUMS" not in assets:
        return None
    sums = {}
    for line in get(assets["SHA256SUMS"]).decode().splitlines():
        parts = line.split()
        if len(parts) == 2:
            sums[parts[1].lstrip("*")] = parts[0].lower()
    prefix = package if extension else "dre-plugin-" + package
    pattern = re.compile("^" + re.escape(prefix) + "-" + re.escape(version) + r"-([a-z0-9_]+-[a-z0-9_]+)\.tar\.gz$")
    out = {}
    for name, url in sorted(assets.items()):
        m = pattern.match(name)
        if m:
            if name not in sums:
                return None
            if not extension:
                verify_archive(repo, release['tag_name'], name, get(url), sums[name])
            out[m.group(1)] = {"url": url, "sha256": sums[name]}
            if extension and name + ".minisig" in assets:
                out[m.group(1)]["minisig"] = get(assets[name + ".minisig"]).decode()
    return out or None


def download_existing(repo, path):
    """Only a missing release/asset permits a fresh index, never a failed download."""
    try:
        release = json.loads(get(f'{API}/repos/{repo}/releases/tags/registry'))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return
        raise
    asset = next((a for a in release['assets'] if a['name'] == 'packages.json'), None)
    if asset is None:
        raise ValueError('registry release exists without packages.json; recover it explicitly')
    body = get(asset['browser_download_url'])
    json.loads(body)  # Never write a partial or invalid index.
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)


def main(old_path, repo):
    packages = json.loads((pathlib.Path(__file__).parent / "packages.json").read_text())
    old = pathlib.Path(old_path)
    index = json.loads(old.read_text()) if old.exists() else {"schema": SCHEMA, "plugins": []}
    index["schema"] = SCHEMA
    by_name = {p["name"]: p for p in index["plugins"]}

    published = list(releases(repo))
    for release in published:
        m = TAG.match(release["tag_name"])
        if not m or release.get("draft"):
            continue
        package, version = m.groups()
        if package not in packages:
            print(f"skipping {release['tag_name']}: `{package}` isn't in packages.json", file=sys.stderr)
            continue
        p = by_name.setdefault(package, {"name": package, "versions": []})
        if any(v["version"] == version for v in p["versions"]):
            continue  # Already indexed (and verified then).
        found = artifacts(release, package, version, repo=repo)
        if not found:
            print(f"skipping {release['tag_name']}: no complete set of archives and checksums", file=sys.stderr)
            continue
        p["versions"].append({"version": version, "protocol": PROTOCOL, "artifacts": found})
        print(f"added {package} {version} ({', '.join(found)})", file=sys.stderr)

    for name, p in by_name.items():
        for version in p['versions']:
            reason = YANKS.get(name, {}).get(version['version'])
            if reason:
                version.update(yanked=True, yank_reason=reason)
            else:
                version.pop('yanked', None)
                version.pop('yank_reason', None)
        if name in packages:
            p["description"] = packages[name]["description"]
            p["provides"] = packages[name]["provides"]
    index["plugins"] = [
        {k: p[k] for k in ("name", "description", "provides", "versions")}
        for p in sorted(by_name.values(), key=lambda p: p["name"])
    ]
    extension_catalog = json.loads((pathlib.Path(__file__).parent / "extensions.json").read_text())
    by_extension = {e["name"]: e for e in index.get("extensions", [])}
    for name, settings in extension_catalog.items():
        entry = by_extension.setdefault(name, {"name": name, "versions": []})
        entry.update(settings)
        for release in published:
            prefix = name + "-v"
            if release.get("draft") or not release["tag_name"].startswith(prefix):
                continue
            version = release["tag_name"][len(prefix):]
            if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
                continue
            if any(v["version"] == version for v in entry["versions"]):
                continue
            found = artifacts(release, name, version, extension=True)
            if found:
                entry["versions"].append({"version": version, "protocol": PROTOCOL, "artifacts": found})
                print(f"added extension {name} {version}", file=sys.stderr)
    index["extensions"] = sorted(by_extension.values(), key=lambda e: e["name"])
    json.dump(index, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    if sys.argv[1] == '--download':
        download_existing(*sys.argv[2:])
    else:
        main(*sys.argv[1:])
