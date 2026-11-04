#!/usr/bin/env python3
"""Published snippets stay copyable, and publication failures leave docs untouched."""
import json
import re
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

import published_docs as docs


class PublishedDocs(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        shutil.copytree(docs.ROOT / ".github/doc-snippets", self.root / ".github/doc-snippets")
        (self.root / "docs").mkdir()
        for name in ("install", "orchestration", "editor-setup"):
            shutil.copy2(docs.ROOT / f"docs/{name}.md", self.root / f"docs/{name}.md")

    def test_committed_pages_match_templates_and_release(self):
        self.assertEqual(docs.check(self.root), [])

    def test_stale_snippet_and_schema_are_detected(self):
        for name, old, new in (("install", "DRE_VERSION=v", "DRE_VERSION=v99."),
                               ("editor-setup", "/schemas/v", "/schemas/v99.")):
            with self.subTest(name=name):
                path = self.root / f"docs/{name}.md"
                original = path.read_text()
                path.write_text(re.sub(r"/schemas/v\d+\.\d+/", "/schemas/v99.0/", original)
                                if name == "editor-setup" else original.replace(old, new))
                self.assertTrue(any(name in problem for problem in docs.check(self.root)))
                path.write_text(original)

    def test_unresolved_placeholder_is_detected(self):
        (self.root / "docs/other.md").write_text("DRE_VERSION={{DRE_RELEASE}}")
        self.assertIn("docs/other.md: unresolved release placeholder", docs.check(self.root))

    @patch("published_docs.latest_release", return_value="v0.99.2")
    @patch("published_docs.read_json", return_value={"x-dre-schema-version": "0.99"})
    def test_sync_verifies_all_schemas_then_renders(self, read, latest):
        docs.sync(self.root)
        self.assertEqual(read.call_count, 7)
        self.assertEqual(docs.check(self.root), [])
        self.assertEqual(json.loads((self.root / docs.METADATA).read_text()), {"tag": "v0.99.2"})
        self.assertIn("DRE_VERSION=v0.99.2", (self.root / "docs/orchestration.md").read_text())
        self.assertIn("${{ secrets.DRE_SCHEDULER_DB }}", (self.root / "docs/orchestration.md").read_text())

    @patch("published_docs.latest_release", return_value="v0.99.2")
    def test_unpublished_or_wrong_schema_changes_nothing(self, latest):
        before = {p: p.read_bytes() for p in self.root.rglob("*") if p.is_file()}
        for failure in (OSError("404"), {"x-dre-schema-version": "0.98"}):
            with self.subTest(failure=failure):
                options = {"side_effect": failure} if isinstance(failure, Exception) else {"return_value": failure}
                with patch("published_docs.read_json", **options):
                    with self.assertRaises((OSError, ValueError)):
                        docs.sync(self.root)
                self.assertEqual({p: p.read_bytes() for p in before}, before)

    def test_plugin_or_prerelease_cannot_be_selected(self):
        for tag in ("skills-v5.2.0", "v0.12.0-rc.1"):
            with self.subTest(tag=tag), patch("published_docs.read_json", return_value={
                "tag_name": tag, "draft": False, "prerelease": False,
            }):
                with self.assertRaises(ValueError):
                    docs.latest_release()


if __name__ == "__main__":
    unittest.main()
