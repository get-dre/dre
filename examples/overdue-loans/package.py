#!/usr/bin/env python3
"""Build a deterministic, self-contained download, excluding local outputs and credentials."""

import argparse
import gzip
import hashlib
import io
from pathlib import Path
import tarfile

ROOT = Path(__file__).resolve().parent


def package(output):
    files = {Path(name): ROOT / name for name in
             ("README.md", "REPORT-SPEC.md", "reference.sas", "prepare.py", "check.py", "package.py")}
    for name in ("inputs", "expected"):
        suffix = "*.csv" if name == "inputs" else "*.json"
        files.update({path.relative_to(ROOT): path for path in (ROOT / name).glob(suffix)})
    for database in ("oracle", "sqlserver"):
        for name in ("dre_project.yml", "profiles.yml", "dependencies.yml", "templates/reporting.xlsx",
                     "reports/overdue/overdue.yml", "reports/overdue/prepare.sql", "reports/overdue/detail.sql",
                     "reports/overdue/summary.sql", "reports/overdue/exceptions.sql"):
            path = Path(database) / name
            files[path] = ROOT / path
    files[Path("parity.py")] = (ROOT / "parity.py" if (ROOT / "parity.py").exists()
                                 else ROOT.parents[1] / "skills/dre-port/scripts/parity.py")
    files[Path("LICENSE")] = ROOT / "LICENSE" if (ROOT / "LICENSE").exists() else ROOT.parents[1] / "LICENSE"
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw, gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode="w") as archive:
            for relative, path in sorted(files.items()):
                data = path.read_bytes()
                entry = tarfile.TarInfo(str(Path("overdue-loans") / relative))
                entry.size, entry.mode, entry.mtime = len(data), 0o644, 0
                archive.addfile(entry, io.BytesIO(data))
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    output.with_suffix(output.suffix + ".sha256").write_text(f"{digest}  {output.name}\n")
    print(f"{output.name}: {len(files)} files, SHA-256 {digest}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    package(parser.parse_args().output)
