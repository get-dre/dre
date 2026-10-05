#!/usr/bin/env python3
"""Tests for package_readme.py: python3 .github/scripts/test_package_readme.py"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import package_readme as pr  # noqa: E402

README = """\
<p>logo</p>

# DRE

<!-- package-description:start -->
**DRE** is reports as code. See [messages](docs/messages.md#a-daily-headline),
[the licence](LICENSE) and [the site](https://getdre.com/).
<!-- package-description:end -->

## Install
"""

META = {"summary": "Reports as code.", "keywords": ["reporting", "sql"]}


class Intro(unittest.TestCase):
    def test_the_text_between_the_markers(self):
        self.assertTrue(pr.intro(README).startswith("**DRE** is reports as code."))
        self.assertNotIn("Install", pr.intro(README))

    def test_missing_markers_fail(self):
        with self.assertRaises(SystemExit):
            pr.intro("# DRE\n\nno markers\n")


class Links(unittest.TestCase):
    def test_docs_pages_point_at_the_site(self):
        out = pr.absolute_links("[m](docs/messages.md#a-daily-headline)")
        self.assertEqual(out, "[m](https://getdre.com/docs/messages/#a-daily-headline)")

    def test_other_relative_paths_point_at_github(self):
        self.assertEqual(pr.absolute_links("[l](LICENSE)"), "[l](https://github.com/get-dre/dre/blob/master/LICENSE)")

    def test_absolute_links_are_kept(self):
        self.assertEqual(pr.absolute_links("[s](https://getdre.com/)"), "[s](https://getdre.com/)")


class Pages(unittest.TestCase):
    def test_pypi_page_has_the_intro_with_absolute_links_and_the_version(self):
        page = pr.pypi_readme(README, "0.3.1")
        self.assertTrue(page.startswith("# dre-cli\n"))
        self.assertIn("https://getdre.com/docs/messages/#a-daily-headline", page)
        self.assertIn("pip install dre-cli", page)
        self.assertIn("`dre` 0.3.1", page)
        self.assertNotIn("](docs/", page)

    def test_crate_page_has_the_intro_and_cargo_install(self):
        page = pr.crate_readme(README)
        self.assertIn("**DRE** is reports as code.", page)
        self.assertIn("cargo install dre-cli --locked", page)


class Check(unittest.TestCase):
    CARGO = '[package]\nname = "dre-cli"\ndescription = "Reports as code."\nkeywords = ["reporting", "sql"]\n'

    def test_matching_files_pass(self):
        self.assertEqual(pr.problems(README, pr.crate_readme(README), self.CARGO, META), [])

    def test_stale_crate_readme_fails(self):
        self.assertTrue(any("README" in p for p in pr.problems(README, "old", self.CARGO, META)))

    def test_cargo_description_and_keywords_must_match(self):
        cargo = '[package]\ndescription = "Something else"\nkeywords = ["sql"]\n'
        found = pr.problems(README, pr.crate_readme(README), cargo, META)
        self.assertTrue(any("description" in p for p in found))
        self.assertTrue(any("keywords" in p for p in found))

    def test_crates_io_limits(self):
        meta = {"summary": "x", "keywords": ["a", "b", "c", "d", "e", "f"]}
        cargo = '[package]\ndescription = "x"\nkeywords = ["a", "b", "c", "d", "e", "f"]\n'
        self.assertTrue(any("at most 5" in p for p in pr.problems(README, pr.crate_readme(README), cargo, meta)))


if __name__ == "__main__":
    unittest.main()
