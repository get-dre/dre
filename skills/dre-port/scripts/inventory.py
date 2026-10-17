#!/usr/bin/env python3
"""List a folder of reports to port to DRE: SSRS / Power BI paginated `.rdl` files and Crystal
Reports XML exports.

    python3 inventory.py <folder or file> [--subscriptions FILE] [--json]

For each report: its data sources, datasets (query text or stored procedure), parameters,
groups, expressions, page header/footer, subscriptions and an effort score (S, M, L, XL, with
the reasons). Then near-duplicates grouped into candidate Sets, and a suggested order: the
easiest first, each group together.

SSRS keeps subscriptions in its catalog database, not in the `.rdl`. Export them to a csv with
the columns `report,description,delivery,schedule,data_driven,recipients` (`report` is the
report's path or name) and pass it with --subscriptions. Binary Crystal `.rpt` files are listed
but not read: export them to XML first. Python 3.8+ standard library only.
"""

import argparse
import csv
import json
import pathlib
import re
import sys
import xml.etree.ElementTree as ET

REPORT_SUFFIXES = {".rdl", ".rdlc", ".xml", ".rpt"}
HEAVY_FUNCTIONS = {"Lookup", "LookupSet", "MultiLookup", "RunningValue", "Previous", "Switch", "Choose",
                   "InScope", "Level", "Aggregate"}
FUNCTIONS = re.compile(
    r"\b(IIf|Switch|Choose|Format|FormatNumber|FormatCurrency|FormatPercent|FormatDateTime|Lookup|LookupSet|"
    r"MultiLookup|RunningValue|Previous|RowNumber|CountDistinct|CountRows|Sum|Avg|Count|Min|Max|First|Last|"
    r"DateAdd|DateDiff|DatePart|InScope|Level|Aggregate|Join|Split|Code\.\w+)\s*\(")


def local(tag):
    return tag.rsplit("}", 1)[-1]


def children(el, name):
    return [c for c in el if local(c.tag) == name]


def child(el, name):
    found = children(el, name) if el is not None else []
    return found[0] if found else None


def text(el, name, default=""):
    c = child(el, name)
    return (c.text or "").strip() if c is not None and c.text else default


def walk(el, name):
    return [e for e in el.iter() if local(e.tag) == name]


class Dataset:
    def __init__(self, name, source="", command_type="Text", command="", fields=()):
        self.name, self.source, self.command_type, self.command = name, source, command_type, command
        self.fields = list(fields)

    def as_dict(self):
        return {"name": self.name, "source": self.source, "command_type": self.command_type,
                "command": self.command, "fields": self.fields}


class Parameter:
    def __init__(self, name, type="String", multi_value=False, default="", available="", hidden=False):
        self.name, self.type, self.multi_value = name, type, multi_value
        self.default, self.available, self.hidden = default, available, hidden

    def as_dict(self):
        return {"name": self.name, "type": self.type, "multi_value": self.multi_value,
                "default": self.default, "available": self.available, "hidden": self.hidden}


class Report:
    def __init__(self, path, kind):
        self.path = pathlib.Path(path)
        self.name = self.path.stem
        self.kind = kind
        self.data_sources, self.datasets, self.parameters, self.groups = [], [], [], []
        self.subscriptions = []
        self.expressions = set()
        self.tables, self.formulas = [], []  # Crystal
        self.selection = ""
        self.page_header = self.page_footer = False
        self.custom_code = False
        self.subreports = self.charts = self.data_regions = 0
        self.problem = ""

    def effort(self):
        """(score, size, reasons)."""
        if self.problem:
            return 0, "?", [self.problem]
        score, why = 1, []
        n = len(self.datasets) or (1 if self.tables else 0)
        if n > 1:
            score += n - 1
            why.append(f"{n} datasets")
        score += 1
        if self.parameters:
            score += len(self.parameters) * 0.5
            why.append(f"{len(self.parameters)} parameter(s)")
        multi = [p.name for p in self.parameters if p.multi_value]
        if multi:
            score += len(multi)
            why.append("multi-value: " + ", ".join(multi))
        procs = [d.name for d in self.datasets if d.command_type == "StoredProcedure"]
        if procs:
            score += 2 * len(procs)
            why.append("stored procedure: " + ", ".join(procs))
        shared = [d.name for d in self.datasets if d.command_type == "SharedDataSet"]
        if shared:
            score += len(shared)
            why.append("shared dataset (export its query): " + ", ".join(shared))
        if self.groups:
            score += len(self.groups)
            why.append(f"{len(self.groups)} group level(s)")
        heavy = sorted(f for f in self.expressions if f in HEAVY_FUNCTIONS or f.startswith("Code."))
        if heavy:
            score += 2 * len(heavy)
            why.append("expressions: " + ", ".join(heavy))
        light = sorted(self.expressions - set(heavy))
        if light:
            score += 0.25 * len(light)
        if self.formulas:
            score += len(self.formulas)
            why.append(f"{len(self.formulas)} Crystal formula(s)")
        if self.custom_code:
            score += 3
            why.append("custom code")
        if self.subreports:
            score += 2 * self.subreports
            why.append(f"{self.subreports} subreport(s)")
        if self.charts:
            score += 2 * self.charts
            why.append(f"{self.charts} chart(s)/gauge(s)/map(s): no DRE equivalent; SVG in a pdf template")
        if self.page_header or self.page_footer:
            score += 1
            why.append("page header/footer: a pdf template")
        dd = [s for s in self.subscriptions if s.get("data_driven")]
        if dd:
            score += 2
            why.append("data-driven subscription: for_each")
        size = "S" if score <= 4 else "M" if score <= 8 else "L" if score <= 13 else "XL"
        return round(score, 2) if score % 1 else int(score), size, why

    def signature(self):
        """Tokens that describe the report's shape, for finding near-duplicates."""
        tokens = set()
        for d in self.datasets:
            tokens |= {"ds:" + t for t in re.findall(r"\w+", (d.command or "").lower())}
            tokens |= {"f:" + f.lower() for f in d.fields}
        tokens |= {"p:" + p.name.lower() for p in self.parameters}
        tokens |= {"g:" + g.lower() for g in self.groups}
        tokens |= {"t:" + t.lower() for t in self.tables}
        tokens |= {"x:" + f.lower() for f in self.formulas}
        return tokens

    def as_dict(self):
        score, size, why = self.effort()
        return {
            "name": self.name, "path": str(self.path), "kind": self.kind, "problem": self.problem,
            "data_sources": self.data_sources, "datasets": [d.as_dict() for d in self.datasets],
            "parameters": [p.as_dict() for p in self.parameters], "groups": self.groups,
            "expressions": sorted(self.expressions), "tables": self.tables, "formulas": self.formulas,
            "selection": self.selection, "page_header": self.page_header, "page_footer": self.page_footer,
            "custom_code": self.custom_code, "subreports": self.subreports, "charts": self.charts,
            "subscriptions": self.subscriptions,
            "effort": {"score": score, "size": size, "reasons": why},
        }


# ---------------------------------------------------------------------------------------------
# Reading


def read_report(path):
    path = pathlib.Path(path)
    if path.suffix.lower() == ".rpt":
        r = Report(path, "rpt")
        r.problem = ("binary Crystal .rpt: not readable without SAP's SDK; export it to XML (or send "
                     "its SQL and a PDF of its output) and list that instead")
        return r
    try:
        root = ET.parse(path).getroot()
    except ET.ParseError as e:
        r = Report(path, "unknown")
        r.problem = f"not readable XML: {e}"
        return r
    ns = root.tag.split("}")[0].lstrip("{") if "}" in root.tag else ""
    if local(root.tag) == "Report" and "reportdefinition" in ns.lower():
        return _rdl(path, root)
    if local(root.tag) == "CrystalReport" or "crystal-reports" in ns:
        r = Report(path, "crystal-data")
        r.problem = ("a Crystal data export (the report's rows): keep it for the parity check, and "
                     "export the report definition to port it")
        return r
    if local(root.tag) == "Report":
        return _crystal(path, root)
    r = Report(path, "unknown")
    r.problem = f"not a report definition (root element {local(root.tag)})"
    return r


def _rdl(path, root):
    r = Report(path, "rdl")
    for ds in walk(root, "DataSource"):
        r.data_sources.append(ds.get("Name", ""))
    for ds in walk(root, "DataSet"):
        q = child(ds, "Query")
        shared = child(ds, "SharedDataSet")
        if shared is not None:
            d = Dataset(ds.get("Name", ""), command_type="SharedDataSet",
                        command=text(shared, "SharedDataSetReference"))
        else:
            d = Dataset(ds.get("Name", ""), text(q, "DataSourceName"), text(q, "CommandType", "Text"),
                        text(q, "CommandText"))
        d.fields = [f.get("Name", "") for f in walk(ds, "Field")]
        r.datasets.append(d)
    for p in walk(root, "ReportParameter"):
        default = child(p, "DefaultValue")
        dvalue = ""
        if default is not None:
            ref = child(default, "DataSetReference")
            dvalue = f"query:{text(ref, 'DataSetName')}" if ref is not None else \
                ", ".join((v.text or "").strip() for v in walk(default, "Value"))
        valid = child(p, "ValidValues")
        available = ""
        if valid is not None:
            ref = child(valid, "DataSetReference")
            if ref is not None:
                available = f"query:{text(ref, 'DataSetName')}"
            else:
                available = "list:" + ", ".join(text(v, "Value") for v in walk(valid, "ParameterValue"))
        r.parameters.append(Parameter(p.get("Name", ""), text(p, "DataType", "String"),
                                      text(p, "MultiValue").lower() == "true", dvalue, available,
                                      text(p, "Hidden").lower() == "true"))
    for g in walk(root, "Group"):
        if child(g, "GroupExpressions") is not None:
            r.groups.append(g.get("Name", ""))
    for e in root.iter():
        if e.text and e.text.strip().startswith("="):
            r.expressions |= set(FUNCTIONS.findall(e.text))
    r.custom_code = any((c.text or "").strip() for c in walk(root, "Code"))
    r.subreports = len(walk(root, "Subreport"))
    r.charts = sum(len(walk(root, k)) for k in ("Chart", "GaugePanel", "Map"))
    r.data_regions = sum(len(walk(root, k)) for k in ("Tablix", "Table", "Matrix", "List"))
    r.page_header = bool(walk(root, "PageHeader"))
    r.page_footer = bool(walk(root, "PageFooter"))
    return r


def _crystal(path, root):
    r = Report(path, "crystal")
    r.name = root.get("Name") or path.stem
    for t in walk(root, "Table"):
        r.tables.append(t.get("Name", ""))
        loc = t.get("Location")
        if loc and loc not in r.data_sources:
            r.data_sources.append(loc)
    for c in walk(root, "Command"):  # a Crystal "SQL command" table
        r.datasets.append(Dataset(c.get("Name", "Command"), command=(c.text or "").strip()))
    for p in walk(root, "ParameterFieldDefinition"):
        r.parameters.append(Parameter(p.get("Name", ""), p.get("ParameterValueKind") or p.get("ValueType", ""),
                                      (p.get("AllowMultipleValue") or "").lower() == "true"))
    for g in walk(root, "Group"):
        field = g.get("ConditionField") or text(g, "ConditionField")
        if field:
            r.groups.append(field.strip("{}"))
    for f in walk(root, "FormulaFieldDefinition"):
        r.formulas.append(f.get("Name") or (f.get("FormulaName") or "").strip("{}@"))
    for s in walk(root, "RecordSelectionFormula"):
        r.selection = (s.text or "").strip()
    r.subreports = len(walk(root, "Subreport")) + len(walk(root, "SubreportObject"))
    r.charts = len(walk(root, "ChartObject"))
    kinds = {a.get("Kind") for a in walk(root, "Area")}
    r.page_header, r.page_footer = "PageHeader" in kinds, "PageFooter" in kinds
    return r


def read_subscriptions(path):
    out = {}
    with open(path, newline="", encoding="utf-8-sig") as f:
        for row in csv.DictReader(f):
            name = pathlib.PurePosixPath((row.get("report") or "").replace("\\", "/")).stem
            out.setdefault(name.lower(), []).append({
                "description": row.get("description", ""),
                "delivery": (row.get("delivery") or "").strip().lower(),
                "schedule": row.get("schedule", ""),
                "data_driven": (row.get("data_driven") or "").strip().lower() in ("true", "1", "yes"),
                "recipients": row.get("recipients", ""),
            })
    return out


def scan(folder, subscriptions=None):
    """Every report under `folder` (or `folder` itself when it's one file)."""
    folder = pathlib.Path(folder)
    reports = []
    for path in [folder] if folder.is_file() else sorted(folder.rglob("*")):
        if path.is_file() and path.suffix.lower() in REPORT_SUFFIXES:
            r = read_report(path)
            if path.suffix.lower() == ".xml" and r.kind == "unknown":
                continue  # some other XML file
            reports.append(r)
    if subscriptions:
        subs = read_subscriptions(subscriptions)
        for r in reports:
            r.subscriptions = subs.get(r.name.lower(), [])
    return reports


# ---------------------------------------------------------------------------------------------
# Estate


def similarity(a, b):
    sa, sb = a.signature(), b.signature()
    if not sa or not sb:
        return 0.0
    return len(sa & sb) / len(sa | sb)


def duplicate_groups(reports, threshold=0.8):
    """Reports whose shape is nearly the same: candidates for one DRE report with Sets."""
    readable = [r for r in reports if not r.problem]
    parent = {r.name: r.name for r in readable}

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for i, a in enumerate(readable):
        for b in readable[i + 1:]:
            if a.kind == b.kind and similarity(a, b) >= threshold:
                parent[find(a.name)] = find(b.name)
    groups = {}
    for r in readable:
        groups.setdefault(find(r.name), []).append(r.name)
    return [sorted(g) for g in groups.values() if len(g) > 1]


def suggest_order(reports):
    """Easiest first; a group of near-duplicates together, at its easiest member's place."""
    by_name = {r.name: r for r in reports}
    group_of = {}
    for g in duplicate_groups(reports):
        for name in g:
            group_of[name] = g

    def rank(r):
        return (bool(r.problem), r.effort()[0], r.name)

    out, done = [], set()
    for r in sorted(reports, key=rank):
        if r.name in done:
            continue
        members = sorted((by_name[n] for n in group_of.get(r.name, [r.name])), key=rank)
        for m in members:
            out.append(m)
            done.add(m.name)
    return out


def markdown(reports):
    lines = ["| report | kind | sources | datasets | parameters | groups | subscriptions | effort | why |",
             "|---|---|---|---|---|---|---|---|---|"]
    for r in reports:
        score, size, why = r.effort()
        datasets = "; ".join(f"{d.name} ({'proc' if d.command_type == 'StoredProcedure' else d.command_type.lower()})"
                             for d in r.datasets) or ", ".join(r.tables)
        params = ", ".join(p.name + ("[]" if p.multi_value else "") for p in r.parameters)
        subs = ", ".join(s["delivery"] + (" (data-driven)" if s["data_driven"] else "") for s in r.subscriptions)
        lines.append(f"| {r.name} | {r.kind} | {', '.join(r.data_sources)} | {datasets} | {params} | "
                     f"{', '.join(r.groups)} | {subs} | {size} ({score}) | {'; '.join(why)} |")
    groups = duplicate_groups(reports)
    lines += ["", "## Candidate Sets", ""]
    if groups:
        for g in groups:
            lines.append(f"- {', '.join(g)}: nearly the same shape; one DRE report with a Set each")
    else:
        lines.append("None found.")
    lines += ["", "## Suggested order", ""]
    for n, r in enumerate(suggest_order(reports), 1):
        lines.append(f"{n}. {r.name} ({r.effort()[1]})" + (f": {r.problem}" if r.problem else ""))
    return "\n".join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("folder")
    ap.add_argument("--subscriptions", help="a csv of the reports' subscriptions (see above)")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    folder = pathlib.Path(a.folder)
    if not folder.exists():
        print(f"inventory.py: {folder}: no such file or folder", file=sys.stderr)
        return 2
    reports = scan(folder, a.subscriptions)
    if a.json:
        print(json.dumps({"reports": [r.as_dict() for r in suggest_order(reports)],
                          "candidate_sets": duplicate_groups(reports)}, indent=2))
    else:
        print(markdown(suggest_order(reports)) if reports else "No .rdl, .rdlc, .rpt or Crystal XML files found.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
