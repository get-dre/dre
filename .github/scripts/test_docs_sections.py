#!/usr/bin/env python3
"""Tests for docs_sections.py: python3 .github/scripts/test_docs_sections.py"""

import json
import tempfile
import unittest
from pathlib import Path

import docs_sections as ds


def page(title, section, position, body="Text.\n"):
    return f'---\ntitle: "{title}"\ndescription: "About {title}."\nsection: {section}\nposition: {position}\n---\n\n# {title}\n\n{body}'


class DocsSections(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.docs = Path(self.tmp.name)
        (self.docs / "sections.json").write_text(json.dumps({"sections": [
            {"id": "start", "title": "Start", "description": "First."},
            {"id": "ref", "title": "Reference", "description": "Last."},
        ]}))

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, text):
        (self.docs / f"{name}.md").write_text(text)

    def test_orders_by_section_then_position_and_links_neighbours(self):
        self.write("b", page("B", "start", 2))
        self.write("a", page("A", "start", 1))
        self.write("z", page("Z", "ref", 1))
        files, errors = ds.expected(self.docs)
        self.assertEqual(errors, [])
        a, b, z = (files[self.docs / f"{n}.md"] for n in "abz")
        self.assertTrue(a.endswith("---\n\n**Next:** [B](b.md)\n"), a)
        self.assertIn("**Previous:** [A](a.md) · **Next:** [Z](z.md)", b)
        self.assertTrue(z.endswith("**Previous:** [B](b.md)\n"), z)
        readme = files[self.docs / "README.md"]
        self.assertLess(readme.index("[A](a.md): About A."), readme.index("[B](b.md)"))
        self.assertLess(readme.index("## Start"), readme.index("## Reference"))

    def test_resyncing_replaces_the_old_links(self):
        self.write("a", page("A", "start", 1))
        self.write("b", page("B", "start", 2))
        files, _ = ds.expected(self.docs)
        for p, text in files.items():
            p.write_text(text)
        self.write("c", page("C", "start", 3))
        files, _ = ds.expected(self.docs)
        b = files[self.docs / "b.md"]
        self.assertEqual(b.count("docs-nav"), 1)
        self.assertIn("**Next:** [C](c.md)", b)
        self.assertEqual(ds.strip_nav(b).rstrip("\n"), page("B", "start", 2).rstrip("\n"))

    def test_problems_are_reported(self):
        self.write("a", page("A", "nowhere", 1))
        self.write("b", page("B", "start", 1))
        self.write("c", page("C", "start", 1))
        self.write("d", '---\ntitle: "D"\ndescription: "D."\nsidebar:\n  order: 3\nsection: start\nposition: x\n---\n')
        _, errors = ds.expected(self.docs)
        text = "\n".join(errors)
        self.assertIn("docs/a.md: `section:` must be one of start, ref, got `nowhere`", text)
        self.assertIn("both have position 1", text)
        self.assertIn("docs/d.md: `sidebar:` is replaced", text)
        self.assertIn("docs/d.md: `position:` must be a whole number", text)

    def test_readme_is_not_a_page(self):
        self.write("a", page("A", "start", 1))
        (self.docs / "README.md").write_text("old")
        _, errors = ds.expected(self.docs)
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
