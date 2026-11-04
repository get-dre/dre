#!/usr/bin/env python3
"""Root policy links must be checked alongside user documentation."""

import pathlib
import tempfile
import unittest
from unittest import mock

import check_links


class RootLinks(unittest.TestCase):
    def test_policy_and_contributor_anchors_are_checked(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            (root / "docs").mkdir()
            (root / "docs" / "guide.md").write_text("# Guide\n\n## RSA keys\n")
            names = ("README.md", "SECURITY.md", "CONTRIBUTING.md", "CLA.md")
            for name in names:
                (root / name).write_text("[RSA keys](docs/guide.md#rsa-keys)\n")
            with mock.patch.object(check_links, "ROOT", root):
                self.assertEqual(check_links.problems(), [])
                for name in names:
                    with self.subTest(file=name):
                        path = root / name
                        good = path.read_text()
                        path.write_text("[RSA keys](docs/guide.md#missing)\n")
                        self.assertEqual(check_links.problems(), [
                            f"{name}: `docs/guide.md#missing`: no heading makes `#missing`"])
                        path.write_text(good)


if __name__ == "__main__":
    unittest.main()
