#!/usr/bin/env python3
"""Tests for image_digests.py: python3 .github/scripts/test_image_digests.py"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import image_digests as idg  # noqa: E402

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64

WORKFLOW = f"""\
services:
  postgres:
    image: postgres:17-alpine@{A}
  s3:
    image: adobe/s3mock
steps:
  - run: |
      docker run -d -p 4443:4443 fsouza/fake-gcs-server -scheme http
      docker run -d -p 2222:22 -v "$KEY:/k:ro" \\
        atmoz/sftp:latest@{B} dre:pass
"""


class Pinned(unittest.TestCase):
    def test_finds_pinned_references(self):
        self.assertEqual(idg.pinned(WORKFLOW), [("postgres:17-alpine", A), ("atmoz/sftp:latest", B)])

    def test_registry_hosts_and_paths(self):
        text = f"ghcr.io/goccy/bigquery-emulator:latest@{A}"
        self.assertEqual(idg.pinned(text), [("ghcr.io/goccy/bigquery-emulator:latest", A)])


class Unpinned(unittest.TestCase):
    def test_image_keys_and_docker_run_images_without_a_digest(self):
        self.assertEqual(idg.unpinned(WORKFLOW), ["adobe/s3mock", "fsouza/fake-gcs-server"])

    def test_docker_run_skips_options_and_their_values(self):
        args = "-d --name x --network n -p 1:1 -e A=b --entrypoint sh alpine:3 -c true".split()
        self.assertEqual(idg.run_image(args), "alpine:3")

    def test_comments_are_skipped(self):
        self.assertEqual(idg.unpinned("# the images started with `docker run`, image: x\n"), [])

    def test_everything_pinned(self):
        self.assertEqual(idg.unpinned(f"image: a:1@{A}\ndocker run -d b:2@{B} cmd\n"), [])


class Replace(unittest.TestCase):
    def test_moved_digests_are_rewritten_and_others_kept(self):
        out = idg.replace_digests(WORKFLOW, {"postgres:17-alpine": B})
        self.assertIn(f"postgres:17-alpine@{B}", out)
        self.assertIn(f"atmoz/sftp:latest@{B}", out)
        self.assertNotIn(A, out)

    def test_nothing_to_change(self):
        self.assertEqual(idg.replace_digests(WORKFLOW, {}), WORKFLOW)


if __name__ == "__main__":
    unittest.main()
