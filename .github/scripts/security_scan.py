#!/usr/bin/env python3
"""Scan the dependencies for known vulnerabilities and licences DRE can't ship.

    security_scan.py rust [--report-dir DIR]   cargo-deny: advisories and licences
    security_scan.py go [--report-dir DIR]     govulncheck and go-licenses, for every module in go/

Needs Python 3.11 or later, and cargo-deny, govulncheck and go-licenses on the PATH (or in
$(go env GOPATH)/bin).

Accepted vulnerabilities are listed once, for both languages, in
.github/vulnerability-exceptions.toml, each with an owner and an expiry date. An expired or
malformed entry fails both commands.

- Rust: the Rust entries become cargo-deny's advisory ignore list, in a config generated from
  deny.toml (which never lists them). Vulnerabilities, unsound code and yanked crates fail;
  unmaintained crates only warn.
- Go: govulncheck has no ignore list, so its findings are filtered here. A vulnerability the code
  calls fails unless it's listed; one in a module or package the code doesn't call is printed but
  passes, as govulncheck itself does.

Licences: deny.toml's `licenses.allow` (compatible with GPL-3.0-only) applies to the Rust crates
and, through go-licenses, to the Go modules; the Go exceptions are in
.github/go-license-exceptions.toml.

With --report-dir, the machine-readable output (cargo-deny's and govulncheck's JSON, go-licenses'
CSV report) is saved there; CI keeps it as an artifact.
"""

import argparse
import datetime
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
EXCEPTIONS_FILE = ROOT / ".github/vulnerability-exceptions.toml"
GO_LICENCE_EXCEPTIONS_FILE = ROOT / ".github/go-license-exceptions.toml"
DENY_FILE = ROOT / "deny.toml"
OWN_GO_MODULES = "github.com/get-dre/dre"
FIELDS = ("id", "ecosystem", "reason", "owner", "expires")
ID_PATTERNS = {"rust": r"RUSTSEC-\d{4}-\d{4}", "go": r"GO-\d{4}-\d{4,}"}
# An exception this close to its expiry is pointed out, so it's renewed or fixed in time.
WARN_DAYS = 14


def toml():
    try:
        import tomllib
    except ModuleNotFoundError:
        sys.exit("security_scan.py needs Python 3.11 or later (for tomllib)")
    return tomllib


def parse_exceptions(text):
    return toml().loads(text).get("exception", [])


def problems(entries, today):
    """Everything wrong with the exception entries: each a message; none means they're fine."""
    out, seen = [], set()
    for e in entries:
        name = e.get("id", "an entry without an id")
        missing = [f for f in FIELDS if not e.get(f)]
        if missing:
            out.append(f"{name}: missing {', '.join(missing)}")
            continue
        pattern = ID_PATTERNS.get(e["ecosystem"])
        if pattern is None:
            out.append(f"{name}: ecosystem must be rust or go, not {e['ecosystem']!r}")
            continue
        if not re.fullmatch(pattern, e["id"]):
            out.append(f"{name}: not an exact {e['ecosystem']} advisory id")
        if not isinstance(e["expires"], datetime.date):
            out.append(f"{name}: expires must be a date (expires = 2027-01-31), not a string")
        elif e["expires"] <= today:
            out.append(f"{name}: expired on {e['expires']} (owner: {e['owner']}); fix it, or renew "
                       "the entry with a new reason and date")
        if e["id"] in seen:
            out.append(f"{name}: listed twice")
        seen.add(e["id"])
    return out


def expiring_soon(entries, today):
    return [f"{e['id']} expires on {e['expires']}" for e in entries
            if (e["expires"] - today).days <= WARN_DAYS]


def load_exceptions():
    entries = parse_exceptions(EXCEPTIONS_FILE.read_text())
    today = datetime.date.today()
    errors = problems(entries, today)
    if errors:
        for e in errors:
            print(f"::error file={EXCEPTIONS_FILE.relative_to(ROOT)}::{e}")
        sys.exit(1)
    for w in expiring_soon(entries, today):
        print(f"::warning file={EXCEPTIONS_FILE.relative_to(ROOT)}::{w}")
    return entries


def deny_config(deny_toml, entries):
    """deny.toml with the Rust exceptions as `[advisories] ignore`."""
    advisories = toml().loads(deny_toml).get("advisories", {})
    if "ignore" in advisories:
        sys.exit("deny.toml: remove [advisories] ignore; it's generated from "
                 ".github/vulnerability-exceptions.toml")
    lines = [f"  {{ id = {json.dumps(e['id'])}, reason = {json.dumps(' '.join(e['reason'].split()))} }},"
             for e in entries if e["ecosystem"] == "rust"]
    ignore = "ignore = [\n" + "".join(line + "\n" for line in lines) + "]\n"
    config, n = re.subn(r"^\[advisories\]\n", lambda m: m.group(0) + ignore, deny_toml, flags=re.M)
    if n != 1:
        sys.exit("deny.toml needs one [advisories] table")
    return config


def allowed_licences(deny_toml):
    return toml().loads(deny_toml)["licenses"]["allow"]


def json_stream(text):
    """The objects of a stream of concatenated JSON values (govulncheck -format json)."""
    decoder, out, i = json.JSONDecoder(), [], 0
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if i == len(text):
            return out
        value, i = decoder.raw_decode(text, i)
        out.append(value)


def go_findings(messages, accepted):
    """govulncheck's findings, as ({id: summary} the code calls and isn't listed, {id: summary}
    of the rest). `accepted` holds the listed ids; an OSV alias (a CVE) matches too."""
    osvs = {m["osv"]["id"]: m["osv"] for m in messages if "osv" in m}
    called, other = {}, {}
    for m in messages:
        f = m.get("finding")
        if not f:
            continue
        osv = osvs.get(f["osv"], {"id": f["osv"]})
        if {osv["id"], *osv.get("aliases", [])} & accepted:
            continue
        frame = f["trace"][0]
        where = f"{frame.get('module')}@{frame.get('version')}"
        fixed = f", fixed in {f['fixed_version']}" if f.get("fixed_version") else ", no fix yet"
        summary = f"{osv.get('summary', '')} ({where}{fixed})".strip()
        if frame.get("function"):
            called[osv["id"]] = summary
            other.pop(osv["id"], None)
        elif osv["id"] not in called:
            other[osv["id"]] = summary
    return called, other


def go_licence_args(allowed, exceptions):
    args = ["check", "./...", "--include_tests", f"--allowed_licenses={','.join(allowed)}",
            f"--ignore={OWN_GO_MODULES}"]
    for e in exceptions:
        if not e.get("module") or not e.get("reason"):
            sys.exit(f"{GO_LICENCE_EXCEPTIONS_FILE.relative_to(ROOT)}: every exception needs a "
                     "module and a reason")
        args.append(f"--ignore={e['module']}")
    return args


def tool(name):
    found = shutil.which(name)
    if not found:
        gobin = pathlib.Path(subprocess.run(["go", "env", "GOPATH"], capture_output=True, text=True,
                                            check=False).stdout.strip() or "~/go") / "bin" / name
        found = str(gobin) if gobin.exists() else None
    if not found:
        sys.exit(f"{name} isn't installed (see CONTRIBUTING.md, \"Security scans\")")
    return found


def rust(report_dir):
    entries = load_exceptions()
    config = deny_config(DENY_FILE.read_text(), entries)
    with tempfile.TemporaryDirectory() as tmp:
        path = pathlib.Path(tmp) / "deny.toml"
        path.write_text(config)
        cmd = [tool("cargo-deny"), "--manifest-path", str(ROOT / "Cargo.toml"), "--config", str(path),
               "check", "--warn", "unmaintained", "advisories", "licenses"]
        code = subprocess.run(cmd, check=False).returncode
        if report_dir:
            with open(report_dir / "cargo-deny.json", "w") as out:
                subprocess.run(cmd[:5] + ["--format", "json"] + cmd[5:], stdout=out, stderr=out,
                               check=False)
    return code


def go(report_dir):
    entries = load_exceptions()
    accepted = {e["id"] for e in entries if e["ecosystem"] == "go"}
    allowed = allowed_licences(DENY_FILE.read_text())
    licence_args = go_licence_args(allowed, parse_licence_exceptions())
    govulncheck, go_licenses = tool("govulncheck"), tool("go-licenses")
    failed = False
    for module in sorted(p.parent for p in (ROOT / "go").glob("*/go.mod")):
        name = module.name
        print(f"\n== go/{name}: govulncheck", flush=True)
        result = subprocess.run([govulncheck, "-format", "json", "./..."], cwd=module,
                                capture_output=True, text=True, check=False)
        if result.returncode != 0:
            print(result.stderr)
            failed = True
            continue
        if report_dir:
            (report_dir / f"govulncheck-{name}.json").write_text(result.stdout)
        called, other = go_findings(json_stream(result.stdout), accepted)
        for osv, summary in called.items():
            print(f"::error title=go/{name}: {osv}::called by the code: {summary}")
        for osv, summary in other.items():
            print(f"{osv}: in a dependency, but not called: {summary}")
        if not called:
            print("No vulnerabilities the code calls.")
        failed |= bool(called)

        print(f"== go/{name}: go-licenses", flush=True)
        check = subprocess.run([go_licenses] + licence_args, cwd=module, capture_output=True,
                               text=True, check=False)
        rejected = [line for line in check.stderr.splitlines() if line.startswith("Not allowed")]
        if check.returncode != 0:
            print("\n".join(rejected) or check.stderr)
            failed = True
        else:
            print("Every licence is allowed.")
        if report_dir:
            report = subprocess.run([go_licenses, "report", "./...", "--include_tests",
                                     f"--ignore={OWN_GO_MODULES}"], cwd=module, capture_output=True,
                                    text=True, check=False)
            (report_dir / f"go-licenses-{name}.csv").write_text(report.stdout)
    return 1 if failed else 0


def parse_licence_exceptions():
    return toml().loads(GO_LICENCE_EXCEPTIONS_FILE.read_text()).get("exception", [])


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("ecosystem", choices=["rust", "go"])
    parser.add_argument("--report-dir", type=pathlib.Path)
    args = parser.parse_args()
    if args.report_dir:
        args.report_dir.mkdir(parents=True, exist_ok=True)
    sys.exit((rust if args.ecosystem == "rust" else go)(args.report_dir))


if __name__ == "__main__":
    main()
