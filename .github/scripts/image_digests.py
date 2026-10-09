#!/usr/bin/env python3
"""Keep the container images CI uses pinned by digest, and up to date.

    image_digests.py check     every image reference is pinned (`name:tag@sha256:…`)
    image_digests.py update    re-resolve each pinned tag and rewrite the digests that moved

Images are referenced as `<name>:<tag>@sha256:<digest>`: the tag says what it is, the digest is
what runs. Dependabot doesn't read images in workflow files, so the weekly `Image digests`
workflow runs `update` and proposes the changes as a pull request.

`update` resolves digests with `docker buildx imagetools inspect` (the multi-platform index
digest, as `docker pull` uses).
"""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
FILES = [*sorted((ROOT / ".github/workflows").glob("*.yml")), ROOT / ".github/scripts/ssh_bastion.sh"]
NAME = r"[a-z0-9][a-z0-9._/-]*"
PINNED = re.compile(rf"(?P<ref>{NAME}:[\w][\w.-]*)@(?P<digest>sha256:[0-9a-f]{{64}})")
# An image reference in a workflow `image:` key or after `docker run`'s options.
IMAGE_KEY = re.compile(r"^\s*image:\s*(?P<image>\S+)", re.M)
DOCKER_RUN = re.compile(r"docker run\b(?P<args>(?:\\\n|[^\n])*)")


def pinned(text):
    """Every pinned reference: [(name:tag, digest)]."""
    return [(m["ref"], m["digest"]) for m in PINNED.finditer(text)]


def unpinned(text):
    """Image references with no digest: in `image:` keys, and the image of each `docker run`
    (comments aside)."""
    text = re.sub(r"^\s*#.*$", "", text, flags=re.M)
    out = [m["image"] for m in IMAGE_KEY.finditer(text) if "@sha256:" not in m["image"]]
    for m in DOCKER_RUN.finditer(text):
        image = run_image(m["args"].replace("\\\n", " ").split())
        if image and "@sha256:" not in image:
            out.append(image)
    return out


def run_image(args):
    """The image of a `docker run` command line (the first argument that isn't an option)."""
    takes_value = {"-p", "-v", "-e", "--name", "--network", "--network-alias", "--entrypoint", "-w",
                   "--user", "-u", "--env-file", "--mount", "--platform"}
    i = 0
    while i < len(args):
        a = args[i]
        if a in takes_value:
            i += 2
        elif a.startswith("-"):
            i += 1
        else:
            return a
    return None


def replace_digests(text, digests):
    """`text` with each pinned reference's digest replaced by digests[name:tag], when it has one."""
    return PINNED.sub(lambda m: f"{m['ref']}@{digests.get(m['ref'], m['digest'])}", text)


def resolve(ref):
    out = subprocess.run(["docker", "buildx", "imagetools", "inspect", ref, "--format",
                          "{{.Manifest.Digest}}"], capture_output=True, text=True, check=True)
    return out.stdout.strip()


def main():
    cmd = sys.argv[1] if len(sys.argv) == 2 else ""
    if cmd == "check":
        bad = [(f, image) for f in FILES for image in unpinned(f.read_text())]
        for f, image in bad:
            print(f"::error file={f.relative_to(ROOT)}::{image} isn't pinned by digest "
                  "(name:tag@sha256:…; see .github/scripts/image_digests.py)")
        sys.exit(1 if bad else 0)
    if cmd == "update":
        refs = sorted({ref for f in FILES for ref, _ in pinned(f.read_text())})
        digests = {ref: resolve(ref) for ref in refs}
        for f in FILES:
            text = f.read_text()
            new = replace_digests(text, digests)
            if new != text:
                f.write_text(new)
                print(f"updated {f.relative_to(ROOT)}")
        return
    sys.exit(__doc__)


if __name__ == "__main__":
    main()
