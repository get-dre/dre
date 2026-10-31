#!/usr/bin/env python3
"""Build one reproducible JDBC plugin archive from its complete version directory.

The caller stages the launcher, JAR, runtime and legal files. This script only writes the
archive, so release jobs can reuse the same deterministic layout for every platform.
"""

import argparse
import gzip
import io
import os
from pathlib import Path
import tarfile

ROOT = Path(__file__).resolve().parents[2]


def add_tree(archive, source, destination):
    root = source.resolve()
    for path in sorted(source.rglob("*")):
        if path.is_symlink():
            target = path.resolve()
            target.relative_to(root)
            if not target.is_file():
                raise ValueError(f"unsafe runtime link: {path} -> {path.readlink()}")
            add_file(archive, target, destination / path.relative_to(source), executable=os.access(target, os.X_OK))
        elif path.is_file():
            add_file(archive, path, destination / path.relative_to(source), executable=os.access(path, os.X_OK))


def add_file(archive, source, destination, executable=False):
    info = tarfile.TarInfo(destination.as_posix())
    data = source.read_bytes()
    info.size = len(data)
    info.mode = 0o755 if executable else 0o644
    info.mtime = 0
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.type = tarfile.REGTYPE
    archive.addfile(info, io.BytesIO(data))


def package(output, *, version, platform, jar, runtime, launcher, license, notice, third_party):
    root = Path("jdbc")
    launcher_name = "dre-plugin-jdbc.exe" if platform.startswith("windows-") else "dre-plugin-jdbc"
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode="w", format=tarfile.PAX_FORMAT) as archive:
            add_file(archive, Path(launcher), root / launcher_name, executable=True)
            add_file(archive, Path(jar), root / "dre-plugin-jdbc.jar")
            add_tree(archive, Path(runtime), root / "runtime")
            for name in ("LICENSE", "NOTICE"):
                source = Path(runtime) / name
                if source.is_file():
                    add_file(archive, source, root / "runtime" / name)
            add_file(archive, Path(license), root / "LICENSE")
            add_file(archive, Path(notice), root / "NOTICE")
            add_file(archive, Path(third_party), root / "THIRD_PARTY_NOTICES.md")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--jar", required=True, type=Path)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--launcher", required=True, type=Path)
    parser.add_argument("--license", default=ROOT / "LICENSE", type=Path)
    parser.add_argument("--notice", default=ROOT / "NOTICE", type=Path)
    parser.add_argument("--third-party", default=ROOT / "THIRD_PARTY_NOTICES.md", type=Path)
    package(**vars(parser.parse_args()))


if __name__ == "__main__":
    main()
