#!/usr/bin/env python3
"""Fail when Git tracks a build artifact: a file over 1 MiB, a compiled binary, or an
executable that is not a script.

    check_tracked_artifacts.py

Build outputs belong in .gitignore. Every clone and CI checkout pays for a tracked binary.
"""

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
MAX_BYTES = 1 << 20
BINARY_SUFFIXES = {
    ".a", ".class", ".dll", ".dylib", ".exe", ".o", ".pyc", ".pyo", ".rlib", ".so", ".wasm",
}
# ELF, Mach-O (thin and fat, both byte orders), PE, WebAssembly and ar archives.
MAGIC = (
    b"\x7fELF",
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",
    b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
    b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
    b"MZ",
    b"\x00asm",
    b"!<arch>\n",
)


def tracked(root):
    """Every tracked path with its Git mode, as (mode, relative path)."""
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-s", "-z"],
        check=True, capture_output=True,
    ).stdout
    for entry in out.split(b"\0"):
        if not entry:
            continue
        meta, _, name = entry.partition(b"\t")
        yield meta.split(b" ")[0].decode(), name.decode()


def head(path, size=8):
    try:
        with path.open("rb") as f:
            return f.read(size)
    except OSError:
        return b""


def problems(root=ROOT):
    found = []
    for mode, name in tracked(root):
        if mode not in ("100644", "100755"):
            continue  # a symlink or a submodule, not a file we can inspect.
        path = root / name
        if not path.is_file():
            continue
        size = path.stat().st_size
        if size > MAX_BYTES:
            found.append(f"{name}: {size} bytes, over the 1 MiB limit")
        if path.suffix in BINARY_SUFFIXES or head(path).startswith(MAGIC):
            found.append(f"{name}: looks like a compiled binary")
        elif mode == "100755" and head(path, 2) != b"#!":
            found.append(f"{name}: executable but not a script")
    return found


if __name__ == "__main__":
    p = problems()
    for x in p:
        print(x)
    sys.exit(1 if p else 0)
