#!/usr/bin/env python3
"""Clean-run walkthrough and independent keyed workbook checks; Python standard library only."""

import argparse
from decimal import Decimal
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from xml.etree import ElementTree as ET
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parent
# Downloads include the unchanged shared CSV/XLSX reader; checkouts use its canonical source.
READER = ROOT / "parity.py"
if not READER.exists():
    READER = ROOT.parents[1] / "skills/dre-port/scripts/parity.py"
spec = importlib.util.spec_from_file_location("parity", READER)
parity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parity)
NS = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
HEADERS = {
    "detail": ["loan_id", "customer_id", "customer_name", "branch", "product", "due_date",
               "days_overdue", "bucket", "balance"],
    "summary": ["branch_product_bucket", "accounts", "balance"],
    "exceptions": ["loan_id", "exception"],
}


def verify(project, date, copies):
    expected = json.loads((ROOT / "expected" / f"{date}.json").read_text(), parse_float=Decimal)
    for kind in ("simple", "presentation"):
        path = project / "out" / f"{kind}-{date}.xlsx"
        for query, columns in HEADERS.items():
            templated = kind == "presentation" and query == "summary"
            table = parity.load(path, sheet=query.title())
            if templated:
                grid = [table.columns, *table.rows]
                headers = [i for i, row in enumerate(grid) if row[:len(columns)] == columns]
                assert len(headers) == 1, (path, query, "missing or duplicate header")
                table = parity.Table(columns, grid[headers[0] + 1:])
            rows = table.rows
            if templated:
                # The Excel-authored template keeps its illustrative formula and total footer.
                rows = rows[:len(expected[query])]
                assert len(table.rows) == len(rows) + 2, (path, query, "unexpected template rows")
            assert table.columns[:len(columns)] == columns, (path, table.columns)
            wanted = []
            for copy in range(copies if query != "summary" else 1):
                for record in expected[query]:
                    record = list(record)
                    if query != "summary" and copy:
                        record[0] += f"-{copy:04d}"
                        if query == "detail":
                            record[1] += f"-{copy:04d}"
                    if query == "summary":
                        record[1] *= copies
                        record[2] *= copies
                    wanted.append(record)
            def keyed(records):
                result = {}
                for record in records:
                    key = record[0]
                    assert key not in result, (path, query, "duplicate key", key)
                    result[key] = record[:len(columns)]
                return result
            actual, target = keyed(rows), keyed(wanted)
            assert actual.keys() == target.keys(), (path, query, "record keys", actual.keys(), target.keys())
            for key, record in target.items():
                for column, value, got in zip(columns, record, actual[key]):
                    if isinstance(value, (int, Decimal)):
                        assert Decimal(got) == value, (path, key, column, got, value)
                    else:
                        assert got == (value or ""), (path, key, column, got, value)
            if query == "detail":
                total = sum(Decimal(row[8]) for row in rows)
                assert len(rows) == expected["count"] * copies
                assert total == expected["balance"] * copies, (path, total)
        with ZipFile(path) as archive:
            # Strings must survive as strings; a numeric identifier formatted with zeros fails.
            workbook = ET.fromstring(archive.read("xl/workbook.xml"))
            rels = ET.fromstring(archive.read("xl/_rels/workbook.xml.rels"))
            targets = {r.get("Id"): r.get("Target") for r in rels}
            for sheet in workbook.find("s:sheets", NS):
                if sheet.get("name") != "Detail":
                    continue
                rid = sheet.get("{http://schemas.openxmlformats.org/officeDocument/2006/relationships}id")
                target = targets[rid]
                target = target.lstrip("/") if target.startswith("/") else "xl/" + target
                xml = ET.fromstring(archive.read(target))
                for row in xml.findall("s:sheetData/s:row", NS)[1:]:
                    cells = {c.get("r").rstrip("0123456789"): c for c in row}
                    for column in ("A", "B", "F", "H"):
                        assert cells[column].get("t") in ("s", "inlineStr"), (path, column, "expected text")
                    for column in ("G", "I"):
                        assert cells[column].get("t") in (None, "n"), (path, column, "expected number")
    print(f"Verified {date}: {expected['count'] * copies} detail records, "
          f"{expected['balance'] * copies:.2f} balance; both workbooks")


def check(database, dre, large_copies):
    with tempfile.TemporaryDirectory(prefix="dre-overdue-") as directory:
        example = Path(directory) / "overdue-loans"
        shutil.copytree(ROOT, example, ignore=shutil.ignore_patterns("out", "target", "dre.lock", "__pycache__"))
        # Keep source-checkout dependency resolution valid in the clean scratch copy.
        shutil.copy2(READER, example / "parity.py")
        project = example / database
        def run(*args, success=True):
            started = time.monotonic()
            result = subprocess.run([dre, *args], cwd=project, capture_output=True, text=True, timeout=180)
            assert (result.returncode == 0) == success, result.stdout + result.stderr
            return time.monotonic() - started
        def seed(copies):
            subprocess.run([sys.executable, str(example / "prepare.py"), database,
                            "--copies", str(copies)], check=True, capture_output=True)
        seed(1)
        run("validate")
        for date in ("2026-01-31", "2026-02-28"):
            elapsed = run("run", "overdue", "--var", f"report_date={date}")
            verify(project, date, 1)
            print(f"Observed {database}: 16 source loans, {elapsed:.2f}s CLI wall time")
        # A malformed date fails in source SQL. A new CLI run recreates its private tables.
        run("run", "overdue", "--var", "report_date=invalid-date", success=False)
        run("run", "overdue")
        verify(project, "2026-01-31", 1)
        seed(large_copies)
        elapsed = run("run", "overdue")
        verify(project, "2026-01-31", large_copies)
        print(f"Observed {database}: {16 * large_copies} source loans, {elapsed:.2f}s CLI wall time")
        print("Clean-run checks passed; scratch project removed on exit. SAS was not executed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", choices=("oracle", "sqlserver"))
    parser.add_argument("--dre", default=os.environ.get("DRE_BIN", "dre"))
    parser.add_argument("--large-copies", type=int, default=100)
    parser.add_argument("--verify", type=Path, help="Check this existing project without executing SQL")
    parser.add_argument("--date", choices=("2026-01-31", "2026-02-28"), default="2026-01-31")
    parser.add_argument("--copies", type=int, default=1)
    args = parser.parse_args()
    if args.verify:
        verify(args.verify, args.date, args.copies)
    else:
        check(args.database, args.dre, args.large_copies)
