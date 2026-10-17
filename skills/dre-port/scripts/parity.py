#!/usr/bin/env python3
"""Compare a DRE report's output with an export of the report it replaces.

    python3 parity.py --dre <query.json | dir | csv | xlsx> --old <csv | xlsx> [options]

--dre      DRE's side: a query's typed rows from `dre run --no-deliver --output-json DIR`
           (`DIR/<report>/<binding>/<query>.json`), the DIR itself with --query, or a csv/xlsx.
--old      The old report's export for the same parameters: csv (any delimiter) or xlsx.
--query    The query to read when --dre is a folder.
--sheet    The sheet to read from an xlsx (default: the first). Applies to both sides.
--key      The column(s) that identify a row, comma-separated (DRE's names). Without a key, rows
           are compared in order, and as a set when only the order differs.
--map      Rename old columns to DRE's: `Textbox12=region,Total Net=net`. `old=` ignores one.
--only COL=VALUE Compare only the rows where a column (DRE's name) has this value, on both
                 sides: one store of a per-row run, one client. Repeat for several columns.
--skip-rows N    Lines before the old export's header row (titles, run dates).
--skip-footer N  Lines after its last data row (grand totals, "Page 1 of 1").
--tolerance X    The largest numeric difference counted as rounding (default 0.005).
--json     Print the result as JSON, for agents.

Prints row counts, column totals and a keyed cell diff, each difference explained as rounding,
formatting (the same value written another way: a date, case, spaces, thousands separators) or
value (a real difference). Exits 0 when only rounding and formatting differ, 1 when anything
else does, 2 on a usage error. Python 3.8+ standard library only.
"""

import argparse
import csv
import datetime as dt
import io
import json
import pathlib
import re
import sys
import xml.etree.ElementTree as ET
import zipfile


class Usage(Exception):
    """Bad arguments or unreadable input: exit 2."""


class Table:
    def __init__(self, columns, rows):
        self.columns = list(columns)
        self.rows = [list(r) for r in rows]


# ---------------------------------------------------------------------------------------------
# Reading


def load(path, query=None, sheet=None, skip_rows=0, skip_footer=0):
    path = pathlib.Path(path)
    if not path.exists():
        raise Usage(f"{path}: no such file or folder")
    if path.is_dir():
        if not query:
            raise Usage(f"{path} is a folder: name the query with --query")
        found = sorted(path.rglob(f"{query}.json"))
        if not found:
            raise Usage(f"no {query}.json under {path}")
        if len(found) > 1:
            raise Usage(f"several {query}.json under {path} (one per Set?): pass one file: "
                        + ", ".join(str(f) for f in found))
        path = found[0]
    suffix = path.suffix.lower()
    if suffix == ".json":
        table = _load_json(path)
    elif suffix in (".xlsx", ".xlsm"):
        table = _load_xlsx(path, sheet, skip_rows)
    elif suffix in (".csv", ".txt", ".tsv"):
        table = _load_csv(path, skip_rows)
    else:
        raise Usage(f"{path}: expected .json, .csv or .xlsx")
    if skip_footer:
        table.rows = table.rows[:-skip_footer] if skip_footer < len(table.rows) else []
    return table


def _load_json(path):
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except ValueError as e:
        raise Usage(f"{path}: not JSON: {e}")
    if not isinstance(data, dict) or "columns" not in data or "rows" not in data:
        raise Usage(f"{path}: not a `dre run --output-json` rows file (dre/rows/v1)")
    columns = [c["name"] if isinstance(c, dict) else str(c) for c in data["columns"]]
    rows = []
    for r in data["rows"]:
        if isinstance(r, dict):
            r = [r.get(c) for c in columns]
        rows.append([_text(v) for v in r])
    return Table(columns, rows)


def _text(v):
    if v is None:
        return ""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, float) and v.is_integer():
        return str(int(v))
    if isinstance(v, (dict, list)):
        return json.dumps(v)
    return str(v)


def _load_csv(path, skip_rows):
    raw = path.read_bytes()
    if raw[:2] in (b"\xff\xfe", b"\xfe\xff"):
        text = raw.decode("utf-16")
    else:
        try:
            text = raw.decode("utf-8-sig")
        except UnicodeDecodeError:
            text = raw.decode("cp1252", errors="replace")  # Windows exports
    lines = text.splitlines(keepends=True)[skip_rows:]
    body = "".join(lines)
    if not body.strip():
        raise Usage(f"{path}: no header row (after --skip-rows {skip_rows})")
    try:
        dialect = csv.Sniffer().sniff(lines[0], delimiters=",;\t|")
    except csv.Error:
        dialect = csv.excel
    rows = [r for r in csv.reader(io.StringIO(body), dialect) if any(c.strip() for c in r)]
    return Table([c.strip() for c in rows[0]], rows[1:])


_NS = {"m": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
_REL = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}id"
_DATE_FORMATS = set(range(14, 23)) | set(range(45, 48))


def _load_xlsx(path, sheet, skip_rows):
    try:
        z = zipfile.ZipFile(path)
    except zipfile.BadZipFile:
        raise Usage(f"{path}: not an xlsx file (an old .xls? save it as .xlsx or .csv)")
    with z:
        wb = ET.fromstring(z.read("xl/workbook.xml"))
        sheets = [(s.get("name"), s.get(_REL)) for s in wb.find("m:sheets", _NS)]
        rels = ET.fromstring(z.read("xl/_rels/workbook.xml.rels"))
        targets = {r.get("Id"): r.get("Target") for r in rels}
        if sheet is None:
            name, rid = sheets[0]
        else:
            match = [s for s in sheets if s[0] == sheet]
            if not match:
                raise Usage(f"{path}: no sheet {sheet!r}; it has " + ", ".join(repr(s[0]) for s in sheets))
            name, rid = match[0]
        target = targets[rid].lstrip("/")
        target = target if target.startswith("xl/") else "xl/" + target
        shared = []
        if "xl/sharedStrings.xml" in z.namelist():
            for si in ET.fromstring(z.read("xl/sharedStrings.xml")).findall("m:si", _NS):
                shared.append("".join(t.text or "" for t in si.iter(f"{{{_NS['m']}}}t")))
        date_styles = _date_styles(z)
        grid = []
        for row in ET.fromstring(z.read(target)).iter(f"{{{_NS['m']}}}row"):
            cells = {}
            for c in row.findall("m:c", _NS):
                col = _col_index(c.get("r"))
                kind = c.get("t")
                v = c.find("m:v", _NS)
                if kind == "s" and v is not None:
                    value = shared[int(v.text)]
                elif kind == "inlineStr":
                    value = "".join(t.text or "" for t in c.iter(f"{{{_NS['m']}}}t"))
                elif kind == "b" and v is not None:
                    value = "true" if v.text == "1" else "false"
                elif v is not None and v.text is not None:
                    value = v.text
                    if c.get("s") and int(c.get("s")) in date_styles:
                        value = _serial_date(value)
                    else:
                        value = _text(float(value)) if re.fullmatch(r"-?[\d.]+(E[-+]?\d+)?", value, re.I) else value
                else:
                    value = ""
                cells[col] = value
            if cells:
                grid.append([cells.get(i, "") for i in range(max(cells) + 1)])
    grid = [r for r in grid[skip_rows:] if any(str(c).strip() for c in r)]
    if not grid:
        raise Usage(f"{path}: sheet {name!r} has no header row")
    width = max(len(r) for r in grid)
    grid = [r + [""] * (width - len(r)) for r in grid]
    return Table([str(c).strip() for c in grid[0]], grid[1:])


def _date_styles(z):
    if "xl/styles.xml" not in z.namelist():
        return set()
    styles = ET.fromstring(z.read("xl/styles.xml"))
    custom = set()
    fmts = styles.find("m:numFmts", _NS)
    for f in fmts if fmts is not None else []:
        code = re.sub(r'"[^"]*"|\[[^\]]*\]', "", f.get("formatCode", "")).lower()
        if re.search(r"[dy]", code) or re.search(r"m{3,}", code):
            custom.add(int(f.get("numFmtId")))
    xfs = styles.find("m:cellXfs", _NS)
    out = set()
    for i, xf in enumerate(xfs if xfs is not None else []):
        fid = int(xf.get("numFmtId", "0"))
        if fid in _DATE_FORMATS or fid in custom:
            out.add(i)
    return out


def _serial_date(text):
    days = float(text)
    when = dt.datetime(1899, 12, 30) + dt.timedelta(days=days)
    return when.date().isoformat() if days.is_integer() else when.isoformat(sep=" ")


def _col_index(ref):
    letters = re.match(r"[A-Z]+", ref or "A").group(0)
    n = 0
    for ch in letters:
        n = n * 26 + ord(ch) - 64
    return n - 1


# ---------------------------------------------------------------------------------------------
# Values


def number(text):
    """A number written the way exports write them, or None."""
    if text is None:
        return None
    s = str(text).strip()
    if not s:
        return None
    negative = s.startswith("(") and s.endswith(")")
    s = s.strip("()").strip()
    if s.startswith("-") or s.endswith("-"):  # -$1,234.00 and 1,234.00- (trailing minus)
        negative = not negative
        s = s.strip("-").strip()
    percent = s.endswith("%")
    s = s.rstrip("%").strip()
    s = re.sub(r"^[^\d\-+.,]+|[^\d.,]+$", "", s).strip()  # currency signs and codes
    s = s.replace("\u00a0", " ").replace("'", "")  # no-break spaces and Swiss apostrophes
    if not re.fullmatch(r"[-+]?[\d][\d ,.]*|[-+]?[.,]\d+", s):
        return None
    if " " in s:
        s = s.replace(" ", "")
    if "," in s and "." in s:
        # The later one is the decimal mark.
        s = s.replace(",", "") if s.rfind(".") > s.rfind(",") else s.replace(".", "").replace(",", ".")
    elif "," in s:
        # 1,234 / 1,234,567 are thousands; 1,5 / 12,34 are decimals.
        s = s.replace(",", "") if re.fullmatch(r"[-+]?\d{1,3}(,\d{3})+", s) else s.replace(",", ".")
    elif s.count(".") > 1:
        s = s.replace(".", "")
    try:
        value = float(s)
    except ValueError:
        return None
    if percent:
        value /= 100
    return -value if negative else value


_DATE_PATTERNS = ["%Y-%m-%d", "%d/%m/%Y", "%m/%d/%Y", "%d.%m.%Y", "%d-%m-%Y", "%d %b %Y", "%d %B %Y",
                  "%d-%b-%Y", "%d-%b-%y", "%b %d, %Y", "%B %d, %Y", "%Y/%m/%d", "%d/%m/%y", "%m/%d/%y"]


def dates(text):
    """Every date or timestamp `text` could mean (both, for 10/01/2026), as ISO strings:
    `2026-10-01`, or `2026-10-01 13:00:00` (with its offset, if any) when there's a time of day."""
    s = str(text or "").strip()
    if not s or not re.search(r"\d", s):
        return set()
    time = ""
    m = re.search(r"[T ](\d{1,2}):(\d{2})(?::(\d{2})(\.\d+)?)?(?: ?([AP]M))?\s*(Z|[+-]\d{2}:?\d{2})?$", s, re.I)
    if m:
        s = s[:m.start()]
        hour, minute, second = int(m.group(1)), int(m.group(2)), int(m.group(3) or 0)
        ampm = (m.group(5) or "").upper()
        if ampm == "PM" and hour < 12:
            hour += 12
        elif ampm == "AM" and hour == 12:
            hour = 0
        offset = (m.group(6) or "").upper().replace("Z", "+00:00")
        if offset and ":" not in offset:
            offset = offset[:3] + ":" + offset[3:]
        if (hour, minute, second, m.group(4)) != (0, 0, 0, None) or offset:
            time = f" {hour:02d}:{minute:02d}:{second:02d}{(m.group(4) or '')}{offset}"
    out = set()
    for p in _DATE_PATTERNS:
        try:
            out.add(dt.datetime.strptime(s, p).date().isoformat() + time)
        except ValueError:
            pass
    return out


def _norm_text(s):
    return re.sub(r"\s+", " ", str(s or "")).strip().lower()


def _norm_column(name):
    return re.sub(r"[\s_\-.]+", "", str(name)).lower()


def _key_value(s):
    n = number(s)
    if n is not None and str(s).strip() and not dates(s):
        return repr(n)
    return _norm_text(s)


# ---------------------------------------------------------------------------------------------
# Comparing


class Diff:
    def __init__(self, key, column, old, new, kind):
        self.key, self.column, self.old, self.new, self.kind = key, column, old, new, kind

    def as_dict(self):
        return {"key": list(self.key), "column": self.column, "old": self.old, "dre": self.new, "kind": self.kind}


def classify(old, new, tolerance):
    """None when the two cells are the same value written the same way; else the kind."""
    if old == new:
        return None
    a, b = number(old), number(new)
    if a is not None and b is not None and not dates(old) and not dates(new):
        if a == b:
            return "formatting"
        return "rounding" if abs(a - b) <= tolerance + 1e-9 else "value"
    da, db = dates(old), dates(new)
    if da and db:
        return "formatting" if da & db else "value"
    if _norm_text(old) == _norm_text(new):
        return "formatting"
    return "value"


class Result:
    def __init__(self):
        self.row_counts = (0, 0)
        self.only_old, self.only_dre, self.ignored = [], [], []
        self.columns = []  # (old name, dre name)
        self.totals = {}  # dre column: (old total, dre total)
        self.diffs = []
        self.missing_keys, self.extra_keys = [], []
        self.duplicate_keys = {"old": [], "dre": []}
        self.order_differs = False
        self.key = []
        self.tolerance = 0.005

    @property
    def ok(self):
        return (self.row_counts[0] == self.row_counts[1] and not self.only_old and not self.only_dre
                and not self.missing_keys and not self.extra_keys
                and not self.duplicate_keys["old"] and not self.duplicate_keys["dre"]
                and not any(d.kind == "value" for d in self.diffs)
                and not any(abs(a - b) > self.tolerance * max(1, self.row_counts[1]) + 1e-9
                            for a, b in self.totals.values()))

    def as_dict(self):
        return {
            "ok": self.ok,
            "row_counts": {"old": self.row_counts[0], "dre": self.row_counts[1]},
            "key": self.key,
            "columns": [{"old": o, "dre": n} for o, n in self.columns],
            "only_old_columns": self.only_old,
            "only_dre_columns": self.only_dre,
            "ignored_old_columns": self.ignored,
            "totals": {c: {"old": a, "dre": b, "difference": round(b - a, 10)} for c, (a, b) in self.totals.items()},
            "missing_keys": [list(k) for k in self.missing_keys],
            "extra_keys": [list(k) for k in self.extra_keys],
            "duplicate_keys": {s: [list(k) for k in v] for s, v in self.duplicate_keys.items()},
            "order_differs": self.order_differs,
            "differences": {kind: sum(1 for d in self.diffs if d.kind == kind)
                            for kind in ("value", "rounding", "formatting")},
            "diffs": [d.as_dict() for d in self.diffs],
        }

    def text(self, limit=20):
        out = [("MATCH" if self.ok else "DIFFERENT")
               + f": old {self.row_counts[0]} row(s), DRE {self.row_counts[1]} row(s)"
               + (f", keyed on {', '.join(self.key)}" if self.key else ", compared in order")]
        if self.only_old:
            out.append("Only in the old export: " + ", ".join(self.only_old) + " (rename with --map old=dre, or ignore with old=)")
        if self.only_dre:
            out.append("Only in DRE: " + ", ".join(self.only_dre))
        if self.ignored:
            out.append("Ignored old columns: " + ", ".join(self.ignored))
        if self.order_differs:
            out.append("The same rows in another order: order isn't compared without --key (add an ORDER BY to match)")
        for side, keys in self.duplicate_keys.items():
            if keys:
                out.append(f"Duplicate keys in {side}: " + "; ".join(", ".join(k) for k in keys[:limit])
                           + " (the key doesn't identify one row: add a column to --key)")
        if self.missing_keys:
            out.append(f"{len(self.missing_keys)} key(s) only in the old export: "
                       + "; ".join(", ".join(k) for k in self.missing_keys[:limit]))
        if self.extra_keys:
            out.append(f"{len(self.extra_keys)} key(s) only in DRE: "
                       + "; ".join(", ".join(k) for k in self.extra_keys[:limit]))
        if self.totals:
            out += ["", "Column totals:", "| column | old | DRE | difference |", "|---|---:|---:|---:|"]
            for c, (a, b) in self.totals.items():
                out.append(f"| {c} | {_fmt(a)} | {_fmt(b)} | {_fmt(b - a)} |")
        if self.diffs:
            counts = {k: sum(1 for d in self.diffs if d.kind == k) for k in ("value", "rounding", "formatting")}
            out += ["", "Cell differences: " + ", ".join(f"{n} {k}" for k, n in counts.items() if n),
                    "| key | column | old | DRE | kind |", "|---|---|---|---|---|"]
            ordered = sorted(self.diffs, key=lambda d: ("value", "rounding", "formatting").index(d.kind))
            for d in ordered[:limit]:
                out.append(f"| {', '.join(d.key)} | {d.column} | {d.old} | {d.new} | {d.kind} |")
            if len(self.diffs) > limit:
                out.append(f"... and {len(self.diffs) - limit} more")
            if counts["rounding"]:
                out.append(f"rounding: numbers within {self.tolerance} of each other (round in SQL, or set the column's format)")
            if counts["formatting"]:
                out.append("formatting: the same value written another way (dates, case, spaces, separators); "
                           "fix it in the output's column formats, not in SQL")
        return "\n".join(out)


def _fmt(x):
    return f"{x:,.2f}" if abs(x - round(x)) > 1e-9 or abs(x) >= 1000 else f"{x:,.0f}"


def compare(old, new, key=None, column_map=None, tolerance=0.005, only=None):
    r = Result()
    r.tolerance = tolerance
    column_map = column_map or {}

    # Pair the columns: --map first, then by normalised name.
    dre_by_norm = {_norm_column(c): i for i, c in enumerate(new.columns)}
    pairs, used = [], set()
    for i, c in enumerate(old.columns):
        target = column_map.get(c, c)
        if target == "":
            r.ignored.append(c)
            continue
        j = dre_by_norm.get(_norm_column(target))
        if j is None or j in used:
            r.only_old.append(c)
        else:
            pairs.append((i, j))
            used.add(j)
    r.only_dre = [c for j, c in enumerate(new.columns) if j not in used]
    r.columns = [(old.columns[i], new.columns[j]) for i, j in pairs]

    def cell(row, i):
        return row[i] if i < len(row) else ""

    # Only some rows: one store or client of a run that made them all.
    for column, value in (only or {}).items():
        pair = [p for p in pairs if _norm_column(new.columns[p[1]]) == _norm_column(column)]
        if not pair:
            raise Usage(f"--only {column}: no such column on both sides (DRE has: {', '.join(new.columns)})")
        i, j = pair[0]
        want = _key_value(value)
        old = Table(old.columns, [row for row in old.rows if _key_value(cell(row, i)) == want])
        new = Table(new.columns, [row for row in new.rows if _key_value(cell(row, j)) == want])
    r.row_counts = (len(old.rows), len(new.rows))

    # Totals of the columns numeric on both sides.
    for i, j in pairs:
        a = [number(cell(row, i)) for row in old.rows if cell(row, i).strip()]
        b = [number(cell(row, j)) for row in new.rows if cell(row, j).strip()]
        if a and b and None not in a and None not in b \
                and not any(dates(cell(row, j)) for row in new.rows[:50]):
            r.totals[new.columns[j]] = (sum(a), sum(b))

    # Cells.
    key = key or []
    r.key = key
    if key:
        old_key = []
        for k in key:
            pair = [p for p in pairs if _norm_column(new.columns[p[1]]) == _norm_column(k)]
            if not pair:
                raise Usage(f"--key {k}: no such column on both sides (DRE has: {', '.join(new.columns)})")
            old_key.append(pair[0])

        def index(table, side):
            out, dup = {}, []
            for row in table.rows:
                k = tuple(_key_value(cell(row, p[side])) for p in old_key)
                if k in out:
                    dup.append(tuple(cell(row, p[side]) for p in old_key))
                out[k] = row
            return out, dup

        old_idx, r.duplicate_keys["old"] = index(old, 0)
        new_idx, r.duplicate_keys["dre"] = index(new, 1)
        shown = {k: tuple(cell(row, p[0]) for p in old_key) for k, row in old_idx.items()}
        shown_new = {k: tuple(cell(row, p[1]) for p in old_key) for k, row in new_idx.items()}
        r.missing_keys = [shown[k] for k in old_idx if k not in new_idx]
        r.extra_keys = [shown_new[k] for k in new_idx if k not in old_idx]
        for k, orow in old_idx.items():
            nrow = new_idx.get(k)
            if nrow is None:
                continue
            for i, j in pairs:
                kind = classify(cell(orow, i), cell(nrow, j), tolerance)
                if kind:
                    r.diffs.append(Diff(shown[k], new.columns[j], cell(orow, i), cell(nrow, j), kind))
    else:
        def diffs(orows, nrows):
            out = []
            for n, (orow, nrow) in enumerate(zip(orows, nrows), 1):
                for i, j in pairs:
                    kind = classify(cell(orow, i), cell(nrow, j), tolerance)
                    if kind:
                        out.append(Diff((f"row {n}",), new.columns[j], cell(orow, i), cell(nrow, j), kind))
            return out

        in_order = diffs(old.rows, new.rows)
        if any(d.kind == "value" for d in in_order) and len(old.rows) == len(new.rows):
            def sort_key(row, side):
                return [_key_value(cell(row, p[side])) for p in pairs]
            sorted_diffs = diffs(sorted(old.rows, key=lambda x: sort_key(x, 0)),
                                 sorted(new.rows, key=lambda x: sort_key(x, 1)))
            if not any(d.kind == "value" for d in sorted_diffs):
                r.order_differs = True
                in_order = sorted_diffs
        r.diffs = in_order
    return r


# ---------------------------------------------------------------------------------------------


def parse_map(text):
    out = {}
    for part in filter(None, (p.strip() for p in (text or "").split(","))):
        if "=" not in part:
            raise Usage(f"{part!r}: expected name=value")
        a, b = part.split("=", 1)
        out[a.strip()] = b.strip()
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--dre", required=True)
    ap.add_argument("--old", required=True)
    ap.add_argument("--query")
    ap.add_argument("--sheet")
    ap.add_argument("--key", default="")
    ap.add_argument("--map", default="")
    ap.add_argument("--only", action="append", default=[])
    ap.add_argument("--skip-rows", type=int, default=0)
    ap.add_argument("--skip-footer", type=int, default=0)
    ap.add_argument("--tolerance", type=float, default=0.005)
    ap.add_argument("--limit", type=int, default=20, help="the most differences to print")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    try:
        new = load(a.dre, query=a.query, sheet=a.sheet)
        old = load(a.old, sheet=a.sheet, skip_rows=a.skip_rows, skip_footer=a.skip_footer)
        result = compare(old, new, key=[k.strip() for k in a.key.split(",") if k.strip()],
                         column_map=parse_map(a.map), tolerance=a.tolerance, only=parse_map(",".join(a.only)))
    except Usage as e:
        print(f"parity.py: {e}", file=sys.stderr)
        return 2
    print(json.dumps(result.as_dict(), indent=2) if a.json else result.text(limit=a.limit))
    return 0 if result.ok else 1


if __name__ == "__main__":
    sys.exit(main())
