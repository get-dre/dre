#!/usr/bin/env python3
"""Tests for the dre-port skill's scripts: python3 .github/scripts/test_dre_port.py"""

import json
import pathlib
import subprocess
import sys
import tempfile
import textwrap
import unittest
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "skills" / "dre-port" / "scripts"
FIXTURES = pathlib.Path(__file__).parent / "fixtures" / "dre-port"
sys.path.insert(0, str(SCRIPTS))
import inventory  # noqa: E402
import parity  # noqa: E402


def dre_json(path, columns, rows):
    """A `dre run --output-json` file (`dre/rows/v1`)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({
        "schema_version": "dre/rows/v1",
        "columns": [{"name": c, "type": t} for c, t in columns],
        "row_count": len(rows),
        "rows": rows,
    }))
    return path


def xlsx(path, sheets, date_cells=()):
    """A minimal workbook: {sheet name: [[cell, ...], ...]}; strings go in sharedStrings.
    `date_cells` are (sheet, row, col) holding an Excel serial with a date style."""
    strings = []

    def col(i):
        s = ""
        i += 1
        while i:
            i, r = divmod(i - 1, 26)
            s = chr(65 + r) + s
        return s

    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", "<Types/>")
        names = list(sheets)
        z.writestr("xl/workbook.xml",
                   '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
                   'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>'
                   + "".join(f'<sheet name="{n}" sheetId="{i + 1}" r:id="rId{i + 1}"/>' for i, n in enumerate(names))
                   + "</sheets></workbook>")
        z.writestr("xl/_rels/workbook.xml.rels",
                   '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                   + "".join(f'<Relationship Id="rId{i + 1}" Target="worksheets/sheet{i + 1}.xml" '
                             'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/>'
                             for i in range(len(names)))
                   + "</Relationships>")
        z.writestr("xl/styles.xml",
                   '<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
                   '<cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>')
        for i, name in enumerate(names):
            rows_xml = []
            for r, row in enumerate(sheets[name]):
                cells = []
                for c, v in enumerate(row):
                    ref = f"{col(c)}{r + 1}"
                    if v is None:
                        continue
                    if isinstance(v, str):
                        strings.append(v)
                        cells.append(f'<c r="{ref}" t="s"><v>{len(strings) - 1}</v></c>')
                    else:
                        style = ' s="1"' if (name, r, c) in date_cells else ""
                        cells.append(f'<c r="{ref}"{style}><v>{v}</v></c>')
                rows_xml.append(f'<row r="{r + 1}">{"".join(cells)}</row>')
            z.writestr(f"xl/worksheets/sheet{i + 1}.xml",
                       '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>'
                       + "".join(rows_xml) + "</sheetData></worksheet>")
        z.writestr("xl/sharedStrings.xml",
                   '<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
                   + "".join(f"<si><t>{s}</t></si>" for s in strings) + "</sst>")
    return path


class Tmp(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = pathlib.Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def write(self, name, text):
        p = self.tmp / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(text))
        return p


# ---------------------------------------------------------------------------------------------
# parity.py


class ParityLoad(Tmp):
    def test_reads_dre_json(self):
        p = dre_json(self.tmp / "sales.json", [("region", "Utf8"), ("net", "Decimal128(18, 2)")],
                     [["North", "10.50"], ["South", "4.00"]])
        t = parity.load(p)
        self.assertEqual(t.columns, ["region", "net"])
        self.assertEqual(t.rows, [["North", "10.50"], ["South", "4.00"]])

    def test_reads_a_dre_output_json_folder_by_query(self):
        dre_json(self.tmp / "out" / "sales" / "default" / "sales.json", [("a", "Int64")], [[1]])
        dre_json(self.tmp / "out" / "sales" / "default" / "other.json", [("b", "Int64")], [[2]])
        t = parity.load(self.tmp / "out", query="sales")
        self.assertEqual(t.columns, ["a"])

    def test_a_folder_needs_the_query(self):
        dre_json(self.tmp / "out" / "r" / "default" / "x.json", [("a", "Int64")], [[1]])
        with self.assertRaises(parity.Usage):
            parity.load(self.tmp / "out")

    def test_reads_csv_with_skipped_title_rows(self):
        p = self.write("old.csv", """\
            Monthly sales,,
            Printed 01/10/2026,,
            Region,Net,Orders
            North,"1,234.50",3
            South,4.00,1
            """)
        t = parity.load(p, skip_rows=2)
        self.assertEqual(t.columns, ["Region", "Net", "Orders"])
        self.assertEqual(t.rows[0], ["North", "1,234.50", "3"])

    def test_skip_footer_drops_grand_total_rows(self):
        p = self.write("old.csv", "Region,Net\nNorth,1\nSouth,2\nTotal,3\n")
        self.assertEqual(len(parity.load(p, skip_footer=1).rows), 2)

    def test_reads_csv_with_a_bom_and_semicolons(self):
        p = self.tmp / "old.csv"
        p.write_bytes("﻿Region;Net\nNorth;1,5\n".encode("utf-8"))
        t = parity.load(p)
        self.assertEqual(t.columns, ["Region", "Net"])
        self.assertEqual(t.rows, [["North", "1,5"]])

    def test_reads_cp1252_and_never_crashes_on_odd_bytes(self):
        p = self.tmp / "old.csv"
        p.write_bytes("Name,Net\nCaf\u00e9,1\n".encode("cp1252"))
        self.assertEqual(parity.load(p).rows, [["Caf\u00e9", "1"]])
        p.write_bytes(b"Name,Net\nA\x81,1\n")
        self.assertEqual(parity.load(p).columns, ["Name", "Net"])

    def test_reads_utf16_with_a_bom(self):
        p = self.tmp / "old.csv"
        p.write_bytes("Name,Net\nA,1\n".encode("utf-16"))
        self.assertEqual(parity.load(p).rows, [["A", "1"]])

    def test_reads_xlsx_first_sheet_or_named(self):
        p = xlsx(self.tmp / "old.xlsx", {"Summary": [["Region", "Net"], ["North", 10.5]],
                                         "Detail": [["Order", "Date"], [1, 46296]]},
                 date_cells={("Detail", 1, 1)})
        self.assertEqual(parity.load(p).rows, [["North", "10.5"]])
        t = parity.load(p, sheet="Detail")
        self.assertEqual(t.rows, [["1", "2026-10-01"]])

    def test_unknown_sheet_names_the_sheets(self):
        p = xlsx(self.tmp / "old.xlsx", {"Summary": [["a"]]})
        with self.assertRaisesRegex(parity.Usage, "Summary"):
            parity.load(p, sheet="Nope")


class ParityValues(unittest.TestCase):
    def test_numbers_in_export_styles(self):
        for text, want in [("1,234.50", 1234.5), ("$1,234.50", 1234.5), ("(12.00)", -12.0),
                           ("-12", -12.0), ("12%", 0.12), ("€ 7", 7.0), ("1 234,5", 1234.5),
                           ("1.234,5", 1234.5), (" 3 ", 3.0), ("", None), ("abc", None),
                           ("2026-10-01", None)]:
            self.assertEqual(parity.number(text), want, text)
        self.assertEqual(parity.number("5-"), -5.0)
        self.assertEqual(parity.number("-$1,234.00"), -1234.0)
        self.assertEqual(parity.number("$-1,234.00"), -1234.0)

    def test_dates_in_export_styles(self):
        for text in ["2026-10-01", "01/10/2026", "1 Oct 2026", "01-Oct-2026", "2026-10-01 00:00:00",
                     "2026-10-01T00:00:00", "October 1, 2026"]:
            self.assertIn("2026-10-01", parity.dates(text), text)
        self.assertEqual(parity.dates("10/01/2026"), {"2026-10-01", "2026-01-10"})
        self.assertEqual(parity.dates("North"), set())


class ParityCompare(Tmp):
    def tables(self, old, new):
        return (parity.Table(old[0], [list(map(str, r)) for r in old[1:]]),
                parity.Table(new[0], [list(map(str, r)) for r in new[1:]]))

    def test_identical_is_a_match(self):
        old, new = self.tables([["Region", "Net"], ["North", "1,234.50"], ["South", "4"]],
                               [["region", "net"], ["North", "1234.50"], ["South", "4.00"]])
        r = parity.compare(old, new, key=["region"])
        self.assertTrue(r.ok, r.text())
        self.assertEqual(r.row_counts, (2, 2))
        self.assertIn("net", r.totals)

    def test_columns_match_ignoring_case_spaces_and_underscores(self):
        old, new = self.tables([["Net Sales", "Order Count"], ["1", "2"]],
                               [["net_sales", "order_count"], ["1", "2"]])
        self.assertTrue(parity.compare(old, new).ok)

    def test_column_map_renames_old_textbox_names(self):
        old, new = self.tables([["Textbox12", "Net"], ["North", "1"]],
                               [["region", "net"], ["North", "1"]])
        r = parity.compare(old, new, key=["region"], column_map={"Textbox12": "region"})
        self.assertTrue(r.ok, r.text())

    def test_missing_and_extra_columns_are_reported(self):
        old, new = self.tables([["a", "b"], ["1", "2"]], [["a", "c"], ["1", "3"]])
        r = parity.compare(old, new)
        self.assertFalse(r.ok)
        self.assertEqual(r.only_old, ["b"])
        self.assertEqual(r.only_dre, ["c"])

    def test_row_count_difference(self):
        old, new = self.tables([["k", "v"], ["1", "1"], ["2", "2"]], [["k", "v"], ["1", "1"]])
        r = parity.compare(old, new, key=["k"])
        self.assertFalse(r.ok)
        self.assertEqual(r.row_counts, (2, 1))
        self.assertEqual(r.missing_keys, [("2",)])

    def test_keyed_cell_diff_explains_rounding(self):
        old, new = self.tables([["k", "v"], ["1", "10.00"], ["2", "3.33"]],
                               [["k", "v"], ["1", "10"], ["2", "3.3333"]])
        r = parity.compare(old, new, key=["k"])
        self.assertTrue(r.ok, r.text())  # rounding only
        self.assertEqual([(d.key, d.kind) for d in r.diffs], [(("1",), "formatting"), (("2",), "rounding")])

    def test_keyed_cell_diff_explains_formatting(self):
        old, new = self.tables([["k", "d", "name"], ["1", "01/10/2026", "ACME  "]],
                               [["k", "d", "name"], ["1", "2026-10-01", "acme"]])
        r = parity.compare(old, new, key=["k"])
        self.assertTrue(r.ok, r.text())
        self.assertEqual(sorted(d.kind for d in r.diffs), ["formatting", "formatting"])

    def test_times_that_differ_are_values(self):
        self.assertEqual(parity.classify("2026-10-01 13:00", "2026-10-01 09:00", 0.005), "value")
        self.assertEqual(parity.classify("01/10/2026 13:00:00", "2026-10-01T13:00:00", 0.005), "formatting")
        self.assertEqual(parity.classify("01/10/2026", "2026-10-01 00:00:00", 0.005), "formatting")

    def test_real_value_difference_fails(self):
        old, new = self.tables([["k", "v"], ["1", "10"]], [["k", "v"], ["1", "11"]])
        r = parity.compare(old, new, key=["k"])
        self.assertFalse(r.ok)
        self.assertEqual(r.diffs[0].kind, "value")
        self.assertIn("v", r.text())

    def test_tolerance_is_configurable(self):
        old, new = self.tables([["k", "v"], ["1", "10.00"]], [["k", "v"], ["1", "10.04"]])
        self.assertFalse(parity.compare(old, new, key=["k"]).ok)
        self.assertTrue(parity.compare(old, new, key=["k"], tolerance=0.05).ok)

    def test_no_key_same_rows_other_order(self):
        old, new = self.tables([["a", "b"], ["1", "x"], ["2", "y"]], [["a", "b"], ["2", "y"], ["1", "x"]])
        r = parity.compare(old, new)
        self.assertTrue(r.ok, r.text())
        self.assertTrue(r.order_differs)
        self.assertIn("order", r.text())

    def test_no_key_compares_rows_in_order(self):
        old, new = self.tables([["a", "b"], ["1", "x"]], [["a", "b"], ["1", "z"]])
        r = parity.compare(old, new)
        self.assertFalse(r.ok)
        self.assertEqual(r.diffs[0].key, ("row 1",))

    def test_duplicate_keys_are_reported(self):
        old, new = self.tables([["k", "v"], ["1", "1"], ["1", "2"]], [["k", "v"], ["1", "3"]])
        r = parity.compare(old, new, key=["k"])
        self.assertFalse(r.ok)
        self.assertEqual(r.duplicate_keys["old"], [("1",)])

    def test_unknown_key_column_is_a_usage_error(self):
        old, new = self.tables([["a"], ["1"]], [["a"], ["1"]])
        with self.assertRaisesRegex(parity.Usage, "nope"):
            parity.compare(old, new, key=["nope"])

    def test_totals_compare_numeric_columns(self):
        old, new = self.tables([["k", "v", "t"], ["1", "1,000.10", "x"], ["2", "2", "y"]],
                               [["k", "v", "t"], ["1", "1000.1", "x"], ["2", "2.5", "y"]])
        r = parity.compare(old, new, key=["k"])
        self.assertAlmostEqual(r.totals["v"][0], 1002.1)
        self.assertAlmostEqual(r.totals["v"][1], 1002.6)
        self.assertNotIn("t", r.totals)
        self.assertIn("1,002.10", r.text())

    def test_key_values_match_across_number_formats(self):
        old, new = self.tables([["id", "v"], ["0042", "1"], ["7.0", "2"]], [["id", "v"], ["42", "1"], ["7", "2"]])
        self.assertTrue(parity.compare(old, new, key=["id"]).ok)

    def test_only_keeps_matching_rows_on_both_sides(self):
        old, new = self.tables([["store", "v"], ["101", "1"]],
                               [["store_id", "v"], ["101", "1"], ["102", "5"]])
        r = parity.compare(old, new, key=["store_id"], column_map={"store": "store_id"},
                           only={"store_id": "101"})
        self.assertTrue(r.ok, r.text())
        self.assertEqual(r.row_counts, (1, 1))
        with self.assertRaisesRegex(parity.Usage, "nope"):
            parity.compare(old, new, only={"nope": "1"})

    def test_diffs_are_capped_in_text(self):
        old = [["k", "v"]] + [[str(i), "1"] for i in range(50)]
        new = [["k", "v"]] + [[str(i), "2"] for i in range(50)]
        r = parity.compare(*self.tables(old, new), key=["k"])
        self.assertEqual(len(r.diffs), 50)
        self.assertIn("and 30 more", r.text(limit=20))


class ParityCli(Tmp):
    def run_cli(self, *args):
        return subprocess.run([sys.executable, str(SCRIPTS / "parity.py"), *args],
                              capture_output=True, text=True)

    def test_match_exits_0_and_mismatch_1_and_usage_2(self):
        new = dre_json(self.tmp / "sales.json", [("region", "Utf8"), ("net", "Float64")],
                       [["North", 10.5], ["South", 4.0]])
        good = self.write("good.csv", "Region,Net\nNorth,10.50\nSouth,4.00\n")
        bad = self.write("bad.csv", "Region,Net\nNorth,10.50\nSouth,5.00\n")
        r = self.run_cli("--dre", str(new), "--old", str(good), "--key", "region")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("MATCH", r.stdout)
        r = self.run_cli("--dre", str(new), "--old", str(bad), "--key", "region")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("South", r.stdout)
        r = self.run_cli("--dre", str(new), "--old", str(self.tmp / "missing.csv"))
        self.assertEqual(r.returncode, 2)

    def test_json_output_for_agents(self):
        new = dre_json(self.tmp / "s.json", [("k", "Int64"), ("v", "Int64")], [[1, 2]])
        old = self.write("o.csv", "k,v,extra\n1,2,z\n")
        r = self.run_cli("--dre", str(new), "--old", str(old), "--key", "k", "--map", "extra=", "--json")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = json.loads(r.stdout)
        self.assertTrue(out["ok"])
        self.assertEqual(out["row_counts"], {"old": 1, "dre": 1})
        self.assertEqual(out["ignored_old_columns"], ["extra"])


# ---------------------------------------------------------------------------------------------
# inventory.py


class InventoryRdl(unittest.TestCase):
    def test_tabular_report(self):
        r = inventory.read_report(FIXTURES / "estate" / "sales_by_region.rdl")
        self.assertEqual(r.kind, "rdl")
        self.assertEqual(r.data_sources, ["Warehouse"])
        self.assertEqual([d.name for d in r.datasets], ["Sales"])
        self.assertIn("FROM dbo.Orders", r.datasets[0].command)
        self.assertEqual(r.datasets[0].command_type, "Text")
        self.assertEqual([p.name for p in r.parameters], ["StartDate", "EndDate"])
        self.assertEqual(r.parameters[0].type, "DateTime")
        self.assertEqual(r.groups, [])
        self.assertEqual(r.subscriptions, [])

    def test_grouped_parameterised_report(self):
        r = inventory.read_report(FIXTURES / "estate" / "invoice_summary.rdl")
        self.assertEqual([g for g in r.groups], ["Region", "Store"])
        p = {p.name: p for p in r.parameters}
        self.assertTrue(p["Region"].multi_value)
        self.assertEqual(p["Region"].available, "query:Regions")
        self.assertEqual(r.datasets[1].command_type, "StoredProcedure")
        self.assertIn("Format", r.expressions)
        self.assertTrue(r.page_header)

    def test_rdl_2008_namespace_is_read_too(self):
        r = inventory.read_report(FIXTURES / "estate" / "legacy" / "old_list.rdl")
        self.assertEqual([d.name for d in r.datasets], ["List"])

    def test_effort_grows_with_features(self):
        simple = inventory.read_report(FIXTURES / "estate" / "sales_by_region.rdl")
        grouped = inventory.read_report(FIXTURES / "estate" / "invoice_summary.rdl")
        self.assertLess(simple.effort()[0], grouped.effort()[0])
        self.assertIn(simple.effort()[1], {"S", "M", "L", "XL"})
        self.assertIn("stored procedure", " ".join(grouped.effort()[2]))


class InventoryCrystal(unittest.TestCase):
    def test_crystal_xml_export(self):
        r = inventory.read_report(FIXTURES / "estate" / "crystal" / "aged_debtors.xml")
        self.assertEqual(r.kind, "crystal")
        self.assertEqual([p.name for p in r.parameters], ["AsAt"])
        self.assertEqual(r.groups, ["Customer.Region"])
        self.assertIn("Customer", r.tables)
        self.assertTrue(r.formulas)

    def test_binary_rpt_is_flagged_not_read(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = pathlib.Path(tmp) / "x.rpt"
            p.write_bytes(b"\xd0\xcf\x11\xe0binary")
            r = inventory.read_report(p)
            self.assertEqual(r.kind, "rpt")
            self.assertIn("export", r.problem)


class InventorySubscriptions(unittest.TestCase):
    def test_subscriptions_csv_attaches_to_reports(self):
        reports = inventory.scan(FIXTURES / "estate", subscriptions=FIXTURES / "estate" / "subscriptions.csv")
        by_name = {r.name: r for r in reports}
        subs = by_name["invoice_summary"].subscriptions
        self.assertEqual(len(subs), 2)
        self.assertTrue(any(s["data_driven"] for s in subs))
        self.assertEqual(by_name["sales_by_region"].subscriptions[0]["delivery"], "email")


class InventoryEstate(unittest.TestCase):
    def test_scan_finds_reports_recursively(self):
        names = sorted(r.name for r in inventory.scan(FIXTURES / "estate"))
        self.assertEqual(names, ["aged_debtors", "invoice_summary", "invoice_summary_west",
                                 "old_list", "sales_by_region"])

    def test_near_duplicates_group_into_candidate_sets(self):
        groups = inventory.duplicate_groups(inventory.scan(FIXTURES / "estate"))
        self.assertIn(["invoice_summary", "invoice_summary_west"], [sorted(g) for g in groups])
        self.assertFalse(any("sales_by_region" in g and "old_list" in g for g in groups))

    def test_order_starts_with_the_easiest_and_puts_groups_together(self):
        order = [r.name for r in inventory.suggest_order(inventory.scan(FIXTURES / "estate"))]
        self.assertEqual(order[0], "old_list")
        i = order.index("invoice_summary")
        self.assertEqual(order[i + 1], "invoice_summary_west")

    def test_same_file_name_in_two_folders_stays_two_reports(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            for folder in ("finance", "sales"):
                (root / folder).mkdir()
                (root / folder / "summary.rdl").write_text(
                    (FIXTURES / "estate" / "sales_by_region.rdl").read_text())
            reports = inventory.scan(root)
            self.assertEqual(sorted(r.name for r in reports), ["finance/summary", "sales/summary"])
            self.assertEqual(len(inventory.suggest_order(reports)), 2)
            self.assertEqual(inventory.duplicate_groups(reports), [["finance/summary", "sales/summary"]])

    def test_crystal_text_export_is_listed_for_reading(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            (root / "aged.txt").write_text("Crystal Reports\nReport Definition\n\nFormula Fields\n")
            (root / "notes.txt").write_text("just notes")
            reports = inventory.scan(root)
            self.assertEqual([(r.name, r.kind) for r in reports], [("aged", "crystal-text")])
            self.assertIn("read", reports[0].problem)

    def test_missing_subscriptions_file_is_a_usage_error(self):
        r = subprocess.run([sys.executable, str(SCRIPTS / "inventory.py"), str(FIXTURES / "estate"),
                            "--subscriptions", "/nope.csv"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertNotIn("Traceback", r.stderr)

    def test_scan_takes_one_file(self):
        reports = inventory.scan(FIXTURES / "estate" / "sales_by_region.rdl")
        self.assertEqual([r.name for r in reports], ["sales_by_region"])

    def test_cli_markdown_and_json(self):
        r = subprocess.run([sys.executable, str(SCRIPTS / "inventory.py"), str(FIXTURES / "estate")],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("| sales_by_region |", r.stdout)
        self.assertIn("Candidate Sets", r.stdout)
        r = subprocess.run([sys.executable, str(SCRIPTS / "inventory.py"), str(FIXTURES / "estate"), "--json"],
                           capture_output=True, text=True)
        data = json.loads(r.stdout)
        self.assertEqual(len(data["reports"]), 5)
        self.assertIn("effort", data["reports"][0])


if __name__ == "__main__":
    unittest.main()
