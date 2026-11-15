#!/usr/bin/env python3
"""Validate every public example and assert local output, without remote deliveries."""
import argparse
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from xml.etree import ElementTree as ET
from zipfile import ZipFile
import zlib

ROOT = Path(__file__).resolve().parents[2]
NAMES = ("tutorial", "monthly-finance", "bank-file", "regional-reports",
         "slack-headline", "sftp-delivery", "password-statements", "multilingual",
         "data-tests", "mcp", "cloud-delivery", "messages")
# Validated in every target, never run: they need a database service or an account.
EXTERNAL = {"overdue-loans/oracle": "requires Oracle and user-supplied JDBC drivers",
            "overdue-loans/sqlserver": "requires SQL Server and user-supplied JDBC drivers",
            "warehouses": "requires Postgres, ClickHouse, Databricks, BigQuery and Snowflake"}
# Minimum versions for callers checking examples with an older installed CLI. CI uses the
# latest published CLI below 1.0, or the source build for mixed engine changes.
NEEDS = {"monthly-finance": (0, 11, 0), "password-statements": (0, 7, 0), "multilingual": (0, 9, 0), "data-tests": (0, 11, 0),
         "cloud-delivery": (0, 13, 0), "messages": (0, 13, 0), "warehouses": (0, 13, 0)}
PASSPHRASE = "example-passphrase"
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
    characters. Returns the shown text in drawing order, a space after each run; shaped scripts (Arabic,
    Devanagari) come back in visual order with `?` for glyphs the map doesn't cover.
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


def pdf_font_names(path):
    """The embedded font names, with or without a six-letter subset prefix."""
    fonts = re.findall(rb"/BaseFont /(?:[A-Z]{6}\+)?([\w-]+)", Path(path).read_bytes())
    return {font.decode().split("-")[0] for font in fonts}


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


def selected_examples(names):
    discovered = {p.parent.relative_to(ROOT / "examples").as_posix()
                  for p in (ROOT / "examples").rglob("dre_project.yml")
                  if not {".steps", "target", "out", "dre_deps"}.intersection(p.relative_to(ROOT / "examples").parts)}
    registered = set(NAMES) | EXTERNAL.keys()
    if missing := discovered - registered:
        raise ValueError("register new examples and their checks: " + ", ".join(sorted(missing)))
    if stale := registered - discovered:
        raise ValueError("registered examples missing from tree: " + ", ".join(sorted(stale)))
    if not names or "*" in names:
        return sorted(discovered)
    selected = {p for p in discovered if any(p == n or p.startswith(n + "/") for n in names)}
    unknown = [n for n in names if not any(p == n or p.startswith(n + "/") for p in discovered)]
    if unknown:
        raise ValueError("unknown example: " + ", ".join(unknown))
    return sorted(selected)


def placeholders(names, scratch):
    """Inert values for every `env_var('NAME')` without a default in the selected examples: key
    files and key text are throwaway SSH keys (files encrypted with the placeholder passphrase),
    other files are empty, fingerprints and URLs are well-formed, the rest is plain text."""
    empty = scratch / "placeholder"
    empty.touch()

    def ssh_key(name, passphrase):
        path = scratch / name
        if not path.exists():
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", passphrase, "-f", str(path)],
                           check=True, capture_output=True)
        return path

    values = {}
    for name in names:
        for path in (ROOT / "examples" / name).rglob("*.yml"):
            # Suffixes are checked in order: `_KEY_FILE` before `_KEY` (any `_KEY`, an access
            # key too, gets an SSH key's text, which a plain secret accepts as well).
            for var in re.findall(r"env_var\('([A-Z0-9_]+)'\)", path.read_text(encoding="utf-8")):
                if var.endswith("_KEY_FILE"):
                    values[var] = str(ssh_key("key_file", PASSPHRASE))
                elif var.endswith("_KEY"):
                    values[var] = ssh_key("key_text", "").read_text(encoding="utf-8")
                elif var.endswith("_PASSPHRASE"):
                    values[var] = PASSPHRASE
                elif var.endswith(("_FILE", "_KNOWN_HOSTS", "_JAR", "_TRUSTSTORE")):
                    values[var] = str(empty)
                elif var.endswith("_FINGERPRINT"):
                    values[var] = "SHA256:" + "A" * 43
                elif var.endswith("JDBC_URL"):
                    values[var] = "jdbc:example://localhost"
                elif var.endswith("_URL"):
                    values[var] = "https://example.invalid/hook"
                elif var.endswith("_JSON"):
                    values[var] = "{}"
                else:
                    values[var] = "not-a-real-" + var.lower().replace("_", "-")
    return values


def locations(project):
    profiles = project / "profiles" if (project / "profiles").is_dir() else project
    return ["--project-dir", str(project), "--profiles-dir", str(profiles)]


def check(dre, plugins, names):
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
        for var, value in placeholders(names, scratch).items():
            environment.setdefault(var, value)

        def run(project, *args, target="dev", success=True):
            command = [dre, *args, *locations(project)]
            if args[0] not in ("schedule", "deps"):
                command.extend(["--target", target])
            result = subprocess.run(command, env=environment, capture_output=True, text=True)
            if (result.returncode == 0) != success:
                raise AssertionError(" ".join(command) + "\n" + result.stdout + result.stderr)
            return result.stdout + result.stderr

        version = subprocess.run([dre, "--version"], capture_output=True, text=True).stdout
        have = tuple(int(x) for x in version.split()[-1].split("-")[0].split(".")[:3])
        def validate(project):
            run(project, "validate")
            # Every target's settings; unset or unknown keys are mistakes, not warnings.
            command = [dre, "validate", "--all-targets", *locations(project)]
            result = subprocess.run(command, env=environment, capture_output=True, text=True)
            output = result.stdout + result.stderr
            if result.returncode or re.search(r"warning\[(unknown-|unset-env-var)", output):
                raise AssertionError(" ".join(command) + "\n" + output)

        projects = {}
        for name in names:
            if have < NEEDS.get(name, (0, 0, 0)):
                print(f"{name}: skipped, it needs DRE {'.'.join(map(str, NEEDS[name]))} (this is {have})")
                continue
            project = scratch / name
            # A variant (overdue-loans/oracle) is copied with its parent's shared files.
            parent, _, variant = name.partition("/")
            shutil.copytree(ROOT / "examples" / parent, scratch / parent, dirs_exist_ok=True,
                            ignore=shutil.ignore_patterns("target", "out", "dre_deps",
                                                        "dre.lock", "__pycache__"))
            if name == "warehouses":
                # Validate alternate plugin sources with real descriptors already installed;
                # this checks their configuration, not the GitHub download path.
                if not plugins:
                    bootstrap = scratch / "plugin-bootstrap"
                    bootstrap.mkdir()
                    (bootstrap / "dre_project.yml").write_text("name: plugin_bootstrap\n", encoding="utf-8")
                    (bootstrap / "dependencies.yml").write_text("plugins: [duckdb, csv]\n", encoding="utf-8")
                    run(bootstrap, "deps")
                    environment["DRE_PLUGINS_DIR"] = str(bootstrap / "dre_deps" / "plugins")
                directory = Path(environment["DRE_PLUGINS_DIR"])
                executable = "dre-plugin-csv" + (".exe" if os.name == "nt" else "")
                binaries = sorted(directory.rglob(executable))
                if not binaries:
                    raise AssertionError(f"CSV executable missing from {directory}")
                local_binary = project / ".plugins/dre-plugin-csv.exe"
                local_binary.parent.mkdir()
                shutil.copy2(binaries[0], local_binary)
            if name in EXTERNAL:
                if (scratch / parent / "prepare.py").exists() and variant:
                    # Its seed.sql is generated from the CSV inputs, without a database.
                    subprocess.run([sys.executable, str(scratch / parent / "prepare.py"), variant],
                                   check=True, capture_output=True)
                validate(project)
                print(f"{name}: validated in every target, not run; {EXTERNAL[name]}")
                continue
            projects[name] = project
            validate(project)
            if name == "data-tests":
                run(project, "test", "checked")
                run(project, "build", "--schedule", "checked_daily")
                assert (project / "out/checked.csv").read_text(encoding="utf-8").startswith("execution,amount")
                run(project, "run", "checked")
            elif name == "monthly-finance":
                run(project, "test", "finance")
                tested = current(project, "finance")
                assert {t["test"] for t in tested.get("tests", [])} == {"nonnegative", "reconcile_revenue"}
                assert all(t["status"] == "pass" for t in tested["tests"])
                assert not tested["outputs"] and not tested["deliveries"]
                assert not (project / "out").exists(), "test must not publish report files"
                run(project, "build", "finance")
                built = current(project, "finance")
                assert built["outputs"] and all(t["status"] == "pass" for t in built["tests"])

                # An incomplete account mapping drops Services from Detail, but not Summary.
                bad = scratch / "unmapped-finance"
                shutil.copytree(project, bad, ignore=shutil.ignore_patterns("target", "out", "dre_deps", "dre.lock"))
                lookup = bad / "lookups/accounts.yml"
                lookup.write_text(lookup.read_text(encoding="utf-8").replace(
                    "  - {category: Services, account_code: '4100'}\n", ""), encoding="utf-8")
                run(bad, "build", "finance", success=False)
                failed = current(bad, "finance")
                assert any(t["test"] == "reconcile_revenue" and t["status"] == "fail"
                           and t["failures"] == 1 for t in failed["tests"]), failed
                assert not failed["outputs"] and not failed["deliveries"]
                assert not (bad / "out").exists(), "failed build must not publish the finance pack"
                print("monthly-finance: test and build passed; bad account mapping blocked both workbooks")
            elif name == "cloud-delivery":
                run(project, "run")
                # Hold renders real delivery paths without contacting external stores.
                run(project, "run", "orders", "--hold", target="local")
                planned = current(project, "orders")
                assert len(planned["outputs"]) == 3
                assert any(d["status"] == "held" for d in planned["deliveries"])
            elif name == "mcp":
                run(project, "run", "setup", "--no-deliver")
                run(project, "run", "revenue", "--no-deliver")
                result = current(project, "revenue")
                sheet = workbook(project / result["outputs"][0]["path"])["revenue"]
                assert_cell(sheet, "B2", 30.75)
                assert_cell(sheet, "B3", 5)
            elif name in ("tutorial", "regional-reports"):
                run(project, "run", "--set", "all")
            else:
                run(project, "run")
            expected_status = "succeeded" if have >= (0, 12, 0) else "success"
            assert all(r["status"] == expected_status for r in records(project))
            print(name + ": validated and ran locally")

        for name in ("tutorial", "regional-reports"):
            if name not in projects:
                continue
            for region, revenue in (("North", 1200), ("South", 900)):
                book = workbook(projects[name] / "out" / (region + ".xlsx"))
                assert_cell(book["Sales"], "A2", region)
                assert_cell(book["Sales"], "D2", revenue)
                assert book["Sales"]["D2"][1] == "#,##0.00", "Revenue lost its number format"
            assert_cell(workbook(projects[name] / "out/North.xlsx")["Sales"], "D3", 800)

        regional = projects.get("regional-reports")
        if regional:
            combined = workbook(regional / "out/All.xlsx")
            assert_cell(combined["Sales"], "D6", 3500)
            assert "All targets" in combined, "the combined Set renames the targets tab"
            assert (regional / "out/East.xlsx").exists(), "an empty East workbook is still written"
            assert not (regional / "out/West.xlsx").exists(), "an empty West run writes nothing"

        if "monthly-finance" in projects:
            finance = workbook(projects["monthly-finance"] / "out/finance-2026-01.xlsx")
            # Exceptions is empty, so its tab is left out; Calendar starts at B3 with no header.
            assert set(finance) == {"Detail", "Summary", "Targets", "Calendar"}
            assert_cell(finance["Targets"], "D2", 2000)
            assert_cell(finance["Targets"], "E2", 240)
            assert finance["Calendar"]["B3"][1] == "dd/mm/yyyy" and "A1" not in finance["Calendar"]
            assert_cell(finance["Detail"], "D6", 3500)
            assert_cell(finance["Detail"], "F2", 120)
            assert finance["Detail"]["F2"][2] == "ROUND(D2*0.1,2)"
            assert_cell(finance["Detail"], "E2", "4000")
            assert_cell(finance["Summary"], "B4", 3500)
            presentation = workbook(projects["monthly-finance"] / "out/presentation-2026-01.xlsx")
            assert_cell(presentation["Summary"], "A1", "Acme Corp - 2026-01")
            assert_cell(presentation["Summary"], "B2", 3500)
            assert_cell(presentation["Summary"], "B5", 2000)
            assert_cell(presentation["Summary"], "B6", 1500)
            assert presentation["Summary"]["B7"][2] == "SUM(B5:B6)", "Template total failed to expand"

        if "bank-file" in projects:
            expected = (
                "0000000042CLIENT A            000001234520260131\r\n"
                "0000000099CLIENT B            000000678920260131\r\n")
            assert (projects["bank-file"] / "out/payments.txt").read_bytes() == expected.encode()
        for name in ("slack-headline", "sftp-delivery"):
            if name not in projects:
                continue
            record = latest(records(projects[name]))
            assert record["deliveries"] and all(
                d["status"] == "not_delivered" for d in record["deliveries"])
        cloud = projects.get("cloud-delivery")
        if cloud:
            files = current(cloud, "partner_files")
            folder = cloud / "target/run/partner_files/default/runs" / files["run_id"]
            assert (folder / "bank.csv").read_bytes().startswith(
                b'\xef\xbb\xbf"order_id","region","amount","note"\r\n'), "bank file layout"
            assert (folder / "legacy.txt").read_text(encoding="latin-1") == (
                "5001|North|1200.00\n5002|North|800.00\n5003|South|900.00\n")
            assert (folder / "insurer.csv").read_text(encoding="utf-8").splitlines()[2] == \
                '5002,"North",800.00,NULL'
            for report in ("orders", "partner_files"):
                assert {d["status"] for d in current(cloud, report)["deliveries"]} <= {
                    "not_delivered", "held"}, report
        messages = projects.get("messages")
        if messages:
            sales = current(messages, "daily_sales")
            assert sales["deliveries"] and all(d["status"] == "not_delivered" for d in sales["deliveries"])
            headline = next(o for o in sales["output_results"] if o.get("message"))
            assert headline["message"]["text"].startswith("Revenue: **A$3,500**"), headline["message"]
        if "slack-headline" in projects:
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
                names = pdf_font_names(pdf)
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
        if "sftp-delivery" in projects:
            sftp_record = latest(records(projects["sftp-delivery"]))
            output = projects["sftp-delivery"] / sftp_record["outputs"][0]["path"]
            assert output.read_text(encoding="utf-8") == "client,revenue\nclient_a,3500.00\n"

        if "tutorial" in projects:
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
    parser.add_argument("--example", action="append", help="Run a named example (repeatable; default: all)")
    parser.add_argument("--list", action="store_true", help="List selected projects without running DRE")
    parser.add_argument("--selection-json", help="JSON array of selected example names from CI")
    args = parser.parse_args()
    try:
        names = selected_examples(json.loads(args.selection_json) if args.selection_json else args.example)
    except (ValueError, TypeError) as error:
        parser.error(str(error))
    if args.list:
        print(json.dumps(names))
    else:
        check(args.dre, args.plugins_dir, names)
