#!/usr/bin/env python3
"""Tests for security_scan.py: python3 .github/scripts/test_security_scan.py"""

import datetime
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import security_scan as ss  # noqa: E402

TODAY = datetime.date(2026, 10, 10)

EXCEPTIONS = """\
[[exception]]
id = "RUSTSEC-2023-0071"
ecosystem = "rust"
reason = "No fix yet; \\"rsa\\" only."
owner = "someone"
expires = 2027-01-10

[[exception]]
id = "GO-2026-0001"
ecosystem = "go"
reason = "Not reachable."
owner = "someone"
expires = 2026-12-01
"""

DENY = """\
[graph]
all-features = true

[advisories]
version = 2
yanked = "deny"

[licenses]
version = 2
allow = ["MIT", "Apache-2.0"]
"""


def entry(**fields):
    base = {"id": "RUSTSEC-2023-0071", "ecosystem": "rust", "reason": "r", "owner": "o",
            "expires": datetime.date(2027, 1, 10)}
    base.update(fields)
    return base


class Exceptions(unittest.TestCase):
    def test_parses_the_file(self):
        entries = ss.parse_exceptions(EXCEPTIONS)
        self.assertEqual([e["id"] for e in entries], ["RUSTSEC-2023-0071", "GO-2026-0001"])
        self.assertEqual(entries[0]["expires"], datetime.date(2027, 1, 10))

    def test_a_valid_entry_has_no_problems(self):
        self.assertEqual(ss.problems([entry()], TODAY), [])

    def test_an_expired_entry_fails(self):
        out = ss.problems([entry(expires=datetime.date(2026, 10, 10))], TODAY)
        self.assertEqual(len(out), 1)
        self.assertIn("expired on 2026-10-10", out[0])

    def test_missing_fields_fail(self):
        e = entry()
        del e["owner"]
        self.assertIn("owner", ss.problems([e], TODAY)[0])

    def test_expires_must_be_a_date(self):
        self.assertIn("date", ss.problems([entry(expires="2027-01-10")], TODAY)[0])

    def test_an_unknown_ecosystem_fails(self):
        self.assertIn("ecosystem", ss.problems([entry(ecosystem="npm")], TODAY)[0])

    def test_only_exact_advisory_ids(self):
        for bad in ["rsa", "RUSTSEC-*", "high", "GO-2026"]:
            self.assertTrue(ss.problems([entry(id=bad)], TODAY), bad)
        self.assertTrue(ss.problems([entry(id="GO-2026-0001", ecosystem="rust")], TODAY))

    def test_duplicates_fail(self):
        self.assertIn("twice", ss.problems([entry(), entry()], TODAY)[0])

    def test_soon_expiring_entries(self):
        self.assertEqual(ss.expiring_soon([entry(expires=datetime.date(2026, 10, 20))], TODAY),
                         ["RUSTSEC-2023-0071 expires on 2026-10-20"])
        self.assertEqual(ss.expiring_soon([entry()], TODAY), [])


class DenyConfig(unittest.TestCase):
    def test_rust_entries_become_the_ignore_list(self):
        config = ss.deny_config(DENY, ss.parse_exceptions(EXCEPTIONS))
        self.assertIn('[advisories]\nignore = [\n  { id = "RUSTSEC-2023-0071", reason = '
                      '"No fix yet; \\"rsa\\" only." },\n]\nversion = 2\n', config)
        self.assertNotIn("GO-2026-0001", config)
        self.assertIn('allow = ["MIT", "Apache-2.0"]', config)

    def test_no_rust_entries_gives_an_empty_list(self):
        self.assertIn("[advisories]\nignore = [\n]\n", ss.deny_config(DENY, []))

    def test_a_hand_written_ignore_list_fails(self):
        with self.assertRaises(SystemExit):
            ss.deny_config(DENY.replace("version = 2\nyanked", 'ignore = ["x"]\nyanked'), [])

    def test_allowed_licences_come_from_deny_toml(self):
        self.assertEqual(ss.allowed_licences(DENY), ["MIT", "Apache-2.0"])


def finding(osv, function=None):
    frame = {"module": "golang.org/x/net", "version": "v0.59.0", "package": "golang.org/x/net/http2"}
    if function:
        frame["function"] = function
    return {"finding": {"osv": osv, "fixed_version": "v0.60.0", "trace": [frame]}}


OSVS = [
    {"osv": {"id": "GO-2026-0001", "aliases": ["CVE-2026-1"], "summary": "One"}},
    {"osv": {"id": "GO-2026-0002", "aliases": [], "summary": "Two"}},
]


class Govulncheck(unittest.TestCase):
    def test_called_findings_not_listed_fail(self):
        messages = OSVS + [finding("GO-2026-0002", "Read"), finding("GO-2026-0002", "Write")]
        called, other = ss.go_findings(messages, set())
        self.assertEqual(called, {"GO-2026-0002": "Two (golang.org/x/net@v0.59.0, fixed in v0.60.0)"})
        self.assertEqual(other, {})

    def test_listed_findings_are_accepted(self):
        called, _ = ss.go_findings(OSVS + [finding("GO-2026-0001", "Read")], {"GO-2026-0001"})
        self.assertEqual(called, {})

    def test_an_alias_matches_too(self):
        called, _ = ss.go_findings(OSVS + [finding("GO-2026-0001", "Read")], {"CVE-2026-1"})
        self.assertEqual(called, {})

    def test_findings_the_code_doesnt_call_only_inform(self):
        called, other = ss.go_findings(OSVS + [finding("GO-2026-0002")], set())
        self.assertEqual(called, {})
        self.assertEqual(list(other), ["GO-2026-0002"])

    def test_a_called_finding_isnt_also_reported_as_uncalled(self):
        messages = OSVS + [finding("GO-2026-0002"), finding("GO-2026-0002", "Read")]
        called, other = ss.go_findings(messages, set())
        self.assertEqual((list(called), other), (["GO-2026-0002"], {}))


class JsonStream(unittest.TestCase):
    def test_reads_concatenated_pretty_printed_objects(self):
        text = '{\n  "config": {}\n}\n{\n  "osv": {"id": "GO-1"}\n}\n'
        self.assertEqual(ss.json_stream(text), [{"config": {}}, {"osv": {"id": "GO-1"}}])


class GoLicenceArgs(unittest.TestCase):
    def test_own_modules_and_exceptions_are_ignored(self):
        args = ss.go_licence_args(["MIT", "Apache-2.0"], [{"module": "github.com/a/b", "reason": "r"}])
        self.assertEqual(args, ["check", "./...", "--include_tests", "--allowed_licenses=MIT,Apache-2.0",
                                "--ignore=github.com/get-dre/dre", "--ignore=github.com/a/b"])

    def test_an_exception_needs_a_reason(self):
        with self.assertRaises(SystemExit):
            ss.go_licence_args(["MIT"], [{"module": "github.com/a/b"}])


if __name__ == "__main__":
    unittest.main()
