#!/usr/bin/env python3
"""Validate every public example and assert local output, without remote deliveries."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from xml.etree import ElementTree as ET
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[2]
NAMES = ("tutorial", "monthly-finance", "bank-file", "regional-reports",
         "slack-headline", "sftp-delivery")
NS = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
sys.dont_write_bytecode = True


def workbook(path):
    """Read saved XLSX values and formats with the standard OOXML parser."""
    with ZipFile(path) as archive:
        if "xl/theme/theme1.xml" in archive.namelist():
            theme = ET.fromstring(archive.read("xl/theme/theme1.xml"))
            drawing = {"a": "http://schemas.openxmlformats.org/drawingml/2006/main"}
            colors = theme.find("a:themeElements/a:clrScheme", drawing)
            assert colors is not None and len(colors) == 12 and all(len(c) for c in colors), (
                path, "Theme must define all twelve Office colors")
        styles = ET.fromstring(archive.read("xl/styles.xml"))
        formats = {0: "General", 4: "#,##0.00"}
        custom = styles.find("s:numFmts", NS)
        if custom is not None:
            formats.update({int(f.attrib["numFmtId"]): f.attrib["formatCode"] for f in custom})
        cell_styles = styles.find("s:cellXfs", NS)
        strings = []
        if "xl/sharedStrings.xml" in archive.namelist():
            root = ET.fromstring(archive.read("xl/sharedStrings.xml"))
            strings = ["".join(node.itertext()) for node in root]
        sheets = ET.fromstring(archive.read("xl/workbook.xml")).find("s:sheets", NS)
        rels = ET.fromstring(archive.read("xl/_rels/workbook.xml.rels"))
        targets = {r.attrib["Id"]: r.attrib["Target"] for r in rels}
        result = {}
        for sheet in sheets:
            rid = sheet.attrib["{http://schemas.openxmlformats.org/officeDocument/2006/relationships}id"]
            target = targets[rid]
            target = target.lstrip("/") if target.startswith("/") else "xl/" + target
            root = ET.fromstring(archive.read(target))
            cells = {}
            for cell in root.findall(".//s:sheetData/s:row/s:c", NS):
                value = cell.find("s:v", NS)
                text = None if value is None else value.text
                if cell.attrib.get("t") == "s" and text is not None:
                    text = strings[int(text)]
                elif cell.attrib.get("t") == "inlineStr":
                    text = "".join(cell.find("s:is", NS).itertext())
                elif text is not None:
                    text = float(text)
                formula = cell.find("s:f", NS)
                style = cell_styles[int(cell.attrib.get("s", 0))]
                number_format = formats.get(int(style.attrib.get("numFmtId", 0)))
                cells[cell.attrib["r"]] = (text, number_format,
                                         None if formula is None else formula.text)
            result[sheet.attrib["name"]] = cells
        return result


def assert_cell(sheet, cell, expected):
    actual = sheet[cell][0]
    assert actual == expected, (cell, actual, expected)


def records(project):
    return [json.loads(p.read_text(encoding="utf-8"))
            for p in sorted((project / "target" / "run").glob("*/*/runs/*/run_results.json"))]


def latest(records_):
    return max(records_, key=lambda r: (r["started_at"], r["run_id"]))


def current(project, report, binding="default"):
    directory = project / "target" / "run" / report / binding
    run_id = (directory / "current").read_text(encoding="utf-8").strip()
    return json.loads((directory / "runs" / run_id / "run_results.json").read_text(encoding="utf-8"))


def check(dre, plugins):
    environment = dict(os.environ)
    environment.pop("DRE_TARGET", None)
    environment.pop("DRE_PROFILES_DIR", None)
    if plugins:
        environment["DRE_PLUGINS_DIR"] = str(plugins.resolve())
    # Only production validation sees these inert placeholders; never run prod.
    environment.update({
        "DRE_SECRET_SLACK_TOKEN": "not-a-real-token", "SLACK_CHANNEL": "CEXAMPLE",
        "SFTP_HOST": "sftp.example.invalid", "SFTP_USERNAME": "client_a",
        "DRE_SECRET_SFTP_PASSWORD": "not-a-real-password",
        "SFTP_HOST_KEY_FINGERPRINT": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    })
    with tempfile.TemporaryDirectory(prefix="dre-examples-") as scratch:
        scratch = Path(scratch)

        def run(project, *args, target="dev"):
            command = [dre, *args, "--project-dir", str(project),
                       "--profiles-dir", str(project)]
            if args[0] != "schedule":
                command.extend(["--target", target])
            result = subprocess.run(command, env=environment, capture_output=True, text=True)
            if result.returncode:
                raise AssertionError(" ".join(command) + "\n" + result.stdout + result.stderr)
            return result.stdout + result.stderr

        projects = {}
        for name in NAMES:
            project = scratch / name
            shutil.copytree(ROOT / "examples" / name, project,
                            ignore=shutil.ignore_patterns("target", "out", "dre_deps",
                                                        "dre.lock", "__pycache__"))
            projects[name] = project
            run(project, "validate")
            if name in ("slack-headline", "sftp-delivery"):
                run(project, "validate", target="prod")
            if name in ("tutorial", "regional-reports"):
                run(project, "run", "--set", "all")
            else:
                run(project, "run")
            assert all(r["status"] == "success" for r in records(project))
            print(name + ": validated and ran locally")

        for name in ("tutorial", "regional-reports"):
            for region, revenue in (("North", 1200), ("South", 900)):
                book = workbook(projects[name] / "out" / (region + ".xlsx"))
                assert_cell(book["Sales"], "A2", region)
                assert_cell(book["Sales"], "D2", revenue)
                assert book["Sales"]["D2"][1] == "#,##0.00", "Revenue lost its number format"
            assert_cell(workbook(projects[name] / "out/North.xlsx")["Sales"], "D3", 800)

        finance = workbook(projects["monthly-finance"] / "out/finance-2026-01.xlsx")
        assert set(finance) == {"Detail", "Summary"}
        assert_cell(finance["Detail"], "D6", 3500)
        assert_cell(finance["Detail"], "F2", 120)
        assert finance["Detail"]["F2"][2] == "ROUND(D2*0.1,2)"
        assert_cell(finance["Detail"], "E2", "4000")
        assert_cell(finance["Summary"], "B4", 3500)
        presentation = workbook(projects["monthly-finance"] / "out/presentation-2026-01.xlsx")
        assert_cell(presentation["Summary"], "A1", "Acme Corp - 2026-01")
        assert_cell(presentation["Summary"], "B5", 2000)
        assert_cell(presentation["Summary"], "B6", 1500)
        assert presentation["Summary"]["B7"][2] == "SUM(B5:B6)", "Template total failed to expand"

        expected = (
            "0000000042CLIENT A            000001234520260131\r\n"
            "0000000099CLIENT B            000000678920260131\r\n")
        assert (projects["bank-file"] / "out/payments.txt").read_bytes() == expected.encode()
        for name in ("slack-headline", "sftp-delivery"):
            record = latest(records(projects[name]))
            assert record["deliveries"] and all(
                d["status"] == "not_delivered" for d in record["deliveries"])
        headline = latest(records(projects["slack-headline"]))["output_results"][0]
        assert headline["message"]["text"] == "Acme Corp revenue: $3,500"
        run(projects["slack-headline"], "run", "--var", "threshold=4000")
        skipped = current(projects["slack-headline"], "headline")
        assert skipped["output_results"][0]["status"] == "skipped"
        assert skipped["outputs"] == []
        sftp_record = latest(records(projects["sftp-delivery"]))
        output = projects["sftp-delivery"] / sftp_record["outputs"][0]["path"]
        assert output.read_text(encoding="utf-8") == "client,revenue\nclient_a,3500.00\n"

        spec = importlib.util.spec_from_file_location(
            "tutorial_prepare", ROOT / "examples/tutorial/prepare.py")
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        tutorial = scratch / "first-report"
        subprocess.run([dre, "new", str(tutorial), "--profile", "sample"],
                       env=environment, check=True, capture_output=True)
        for step in range(1, 8):
            helper.prepare(step, tutorial)
            run(tutorial, "validate")
            options = ["--set", "all"] if step >= 5 else []
            if step == 3:
                options.append("--accept-schema-change")
            run(tutorial, "run", "sales", *options)
            default = current(tutorial, "sales", "north" if step >= 5 else "default")
            book = workbook(tutorial / default["outputs"][0]["path"])
            detail = book["Sales" if step >= 3 else "sales"]
            assert_cell(detail, "D2", 1200)
            assert_cell(detail, "D3", 800)
            if step < 5:
                assert_cell(detail, "D4", 900)
                assert_cell(detail, "D5", 600)
            if step >= 3:
                assert_cell(book["Summary"], "B2", 2000)
            if step >= 4:
                assert detail["D2"][1] == "#,##0.00"
            if step == 2:
                run(tutorial, "run", "sales", "--var", "month=2026-02")
                override = current(tutorial, "sales")
                overridden = workbook(tutorial / override["outputs"][0]["path"])
                assert_cell(overridden["sales"], "D2", 1400)
                assert_cell(overridden["sales"], "D3", 1100)
            if step >= 6:
                schedule = run(tutorial, "schedule", "ls")
                assert "weekday_sales" in schedule
            if step == 7:
                assert (tutorial / "out/North.xlsx").exists()
                assert (tutorial / "out/South.xlsx").exists()
            print("tutorial step " + str(step) + ": output verified")
        print("All example outputs verified; temporary projects removed on exit.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dre", default="dre", help="CLI executable (built or installed)")
    parser.add_argument("--plugins-dir", type=Path, help="Prebuilt plugin directory")
    args = parser.parse_args()
    check(args.dre, args.plugins_dir)
