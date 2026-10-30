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
import re
import tempfile
from xml.etree import ElementTree as ET
from zipfile import ZipFile
import zlib

ROOT = Path(__file__).resolve().parents[2]
NAMES = ("tutorial", "monthly-finance", "bank-file", "regional-reports",
         "slack-headline", "sftp-delivery", "password-statements", "multilingual")
# Examples that need a newer DRE than the released one the examples workflow installs: skipped
# there (the CI job against this build runs them all).
NEEDS = {"password-statements": (0, 7, 0), "multilingual": (0, 9, 0)}
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


def pdf_strings(source):
    """Split PDF literal strings `(...)` (with `\\ddd` escapes) out of a TJ array body."""
    escapes = {ord("n"): 10, ord("r"): 13, ord("t"): 9, ord("b"): 8, ord("f"): 12}
    result, current, depth, i = [], None, 0, 0
    while i < len(source):
        char = source[i]
        if current is None:
            if char == ord("("):
                current, depth = bytearray(), 1
        elif char == ord("\\"):
            i += 1
            if source[i:i + 1].isdigit():
                digits = re.match(rb"[0-7]{1,3}", source[i:]).group()
                current.append(int(digits, 8) & 255)
                i += len(digits) - 1
            else:
                current.append(escapes.get(source[i], source[i]))
        elif char == ord("(") or char == ord(")"):
            depth += 1 if char == ord("(") else -1
            if depth:
                current.append(char)
            else:
                result.append(bytes(current))
                current = None
        else:
            current.append(char)
        i += 1
    return result


def pdf_text(path):
    """Read the text of a PDF made by DRE with the standard library.

    Fonts are subset Type0 fonts with a ToUnicode map, so each shown glyph id maps back to its
    characters. Returns one string per page-content stream; line order is the drawing order.
    """
    data = Path(path).read_bytes()
    streams = {}
    for number, body in re.findall(rb"(\d+) 0 obj(.*?)endobj", data, re.S):
        found = re.search(rb"stream\r?\n(.*)\r?\nendstream", body, re.S)
        head = body if not found else body[:found.start()]
        content = None if not found else found.group(1)
        if content is not None and b"FlateDecode" in head:
            content = zlib.decompress(content)
        streams[int(number)] = (head, content)

    def cmap(number):
        table = {}
        text = streams[number][1].decode("latin-1")
        for block in re.findall(r"beginbfchar(.*?)endbfchar", text, re.S):
            for src, dst in re.findall(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", block):
                table[int(src, 16)] = bytes.fromhex(dst).decode("utf-16-be")
        for block in re.findall(r"beginbfrange(.*?)endbfrange", text, re.S):
            for lo, hi, dst in re.findall(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", block):
                start = int(dst, 16)
                for offset in range(int(hi, 16) - int(lo, 16) + 1):
                    table[int(lo, 16) + offset] = chr(start + offset)
        return table

    fonts = {}
    for number, (head, _) in streams.items():
        unicode_map = re.search(rb"/ToUnicode (\d+) 0 R", head)
        if unicode_map and b"/Type0" in head:
            fonts[number] = cmap(int(unicode_map.group(1)))
    names = {}
    for head, _ in streams.values():
        for name, number in re.findall(rb"/(f\d+) (\d+) 0 R", head):
            if int(number) in fonts:
                names[name.decode()] = fonts[int(number)]
    pages = []
    for head, content in streams.values():
        if content is None or b"BT" not in content:
            continue
        table, out = {}, []
        for match in re.finditer(rb"/(f\d+) [\d.]+ Tf|\[(.*?)\]\s*TJ|(\(.*?\))\s*Tj", content, re.S):
            if match.group(1):
                table = names.get(match.group(1).decode(), {})
                continue
            for chunk in pdf_strings(match.group(2) or match.group(3)):
                out.extend(table.get(int.from_bytes(chunk[i:i + 2], "big"), "?")
                           for i in range(0, len(chunk), 2))
            out.append(" ")
        pages.append("".join(out))
    return "\n".join(pages)


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
        "PACK_PASSWORD": "example-pack-password",
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

        version = subprocess.run([dre, "--version"], capture_output=True, text=True).stdout
        have = tuple(int(x) for x in version.split()[-1].split("-")[0].split(".")[:3])
        projects = {}
        for name in NAMES:
            if have < NEEDS.get(name, (0, 0, 0)):
                print(f"{name}: skipped, it needs DRE {'.'.join(map(str, NEEDS[name]))} (this is {have})")
                continue
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
        statements = projects.get("password-statements")
        if statements:
            for member in ("1001", "1002"):
                pdf = (statements / "out" / "statements" / (member + ".pdf")).read_bytes()
                assert pdf.startswith(b"%PDF-") and b"/Encrypt" in pdf, "statement not encrypted"
            keys = next((statements / "out" / "keys").glob("password-list-*.csv"))
            assert keys.read_text(encoding="utf-8") == (
                "member_id,pdf_password\n1001,09031984-1001\n1002,23111979-1002\n")
            with ZipFile(next(statements.glob("out/pack-*.zip"))) as pack:
                assert sorted(pack.namelist()) == ["fees.csv", "pack.csv"]
                assert all(i.flag_bits & 1 for i in pack.infolist()), "pack not encrypted"
            reported = "".join(p.read_text(encoding="utf-8", errors="replace")
                               for d in ("target", "logs") for p in (statements / d).rglob("*")
                               if p.is_file() and p.suffix in (".json", ".jsonl", ".log"))
            assert "09031984" not in reported and "example-pack-password" not in reported, \
                "a password leaked into DRE's records"
        multilingual = projects.get("multilingual")
        if multilingual:
            cities = ("東京", "北京", "서울", "Москва", "القاهرة", "Αθήνα", "मुंबई", "São Paulo", "Zürich")
            offices = multilingual / "out"
            everyone = workbook(offices / "all-offices.xlsx")["offices"]
            for row, city in enumerate(cities, start=2):
                assert_cell(everyone, f"A{row}", city)
                staff = workbook(offices / "offices" / (city + ".xlsx"))["staff"]
                assert_cell(staff, "A2", city)
                assert_cell(staff, "A3", city)
                assert "A4" not in staff, (city, "another office's staff in the workbook")
            mumbai = workbook(offices / "offices/मुंबई.xlsx")["staff"]
            assert_cell(mumbai, "B2", "प्रिया पटेल")
            assert_cell(mumbai, "C2", "लेखाकार")
            # One PDF letter per office. Han text follows the template's `lang`, so each CJK
            # office embeds its own Noto font; the rest is checked from the PDF's own text.
            letters = multilingual / "out" / "letters"
            assert sorted(p.name for p in letters.iterdir()) == sorted(c + ".pdf" for c in cities)
            noto = {"東京": "NotoSansCJKjp", "北京": "NotoSansCJKsc", "서울": "NotoSansCJKkr",
                    "القاهرة": "NotoSansArabic", "मुंबई": "NotoSansDevanagari"}
            shaped = ("القاهرة", "मुंबई")  # joined or reordered glyphs: the text map is partial
            for row, city in enumerate(cities, start=2):
                pdf = letters / (city + ".pdf")
                fonts = set(re.findall(rb"/BaseFont /[A-Z]{6}\+([\w-]+)", pdf.read_bytes()))
                names = {f.decode().split("-")[0] for f in fonts}
                assert (noto[city] in names) if city in noto else not any(
                    n.startswith("Noto") for n in names), (city, names)
                staff = workbook(offices / "offices" / (city + ".xlsx"))["staff"]
                expected = [everyone[f"{column}{row}"][0] for column in "ABE"] + [
                    staff[f"{column}{line}"][0] for line in (2, 3) for column in "BC"]
                text = "".join(pdf_text(pdf).split())
                if city in shaped:
                    # Minus the table's English headers, the "·" separator and unmapped glyphs.
                    found = {c for c in text if c.isalpha() and not c.isascii()}
                    assert 5 < len(found) and found <= set("".join(expected)), (city, text)
                else:
                    for word in expected:
                        assert "".join(word.split()) in text, (city, word, text)
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
