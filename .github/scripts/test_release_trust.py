"""Release policy at the GitHub API/attestation boundary; no live publication."""
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('release_trust', Path(__file__).with_name('release_trust.py'))
trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trust)


class ReleaseTrust(unittest.TestCase):
    def test_only_master_commits_with_green_push_ci_are_releasable(self):
        def api(path):
            if '/commits/' in path:
                return {'sha': 'a' * 40}
            if '/compare/' in path:
                return {'status': status}
            return {'workflow_runs': runs}
        good = {'head_sha': 'a' * 40, 'head_branch': 'master', 'event': 'push', 'conclusion': 'success'}
        with patch.object(trust, 'api', side_effect=api):
            for status, runs, accepted in [
                ('ahead', [good], True), ('identical', [good], True),
                ('diverged', [good], False), ('behind', [good], False),
                ('ahead', [], False), ('ahead', [dict(good, conclusion='failure')], False),
                ('ahead', [dict(good, event='pull_request')], False),
                ('ahead', [dict(good, head_branch='feature')], False),
            ]:
                with self.subTest(status=status, runs=runs):
                    trust.release_commit.cache_clear()
                    if accepted:
                        self.assertEqual(trust.release_commit('get-dre/dre', 'csv-v1.0.0'), 'a' * 40)
                    else:
                        with self.assertRaises(ValueError):
                            trust.release_commit('get-dre/dre', 'csv-v1.0.0')

    def test_archive_requires_matching_workflow_tag_commit_and_checksum(self):
        with patch.object(trust, 'release_commit', return_value='a' * 40), \
             patch.object(trust.subprocess, 'run') as run:
            with self.assertRaises(ValueError):
                trust.verify_archive('get-dre/dre', 'csv-v1.0.0', 'archive', b'changed', 'b' * 64)
            run.assert_not_called()
            import hashlib
            trust.verify_archive('get-dre/dre', 'csv-v1.0.0', 'archive', b'content', hashlib.sha256(b'content').hexdigest())
            args = run.call_args.args[0]
            for flag, expected in [('--signer-workflow', 'get-dre/dre/.github/workflows/release.yml'),
                                   ('--source-ref', 'refs/tags/csv-v1.0.0'), ('--source-digest', 'a' * 40)]:
                self.assertEqual(args[args.index(flag) + 1], expected)
            run.side_effect = subprocess.CalledProcessError(1, 'gh')
            with self.assertRaises(subprocess.CalledProcessError):
                trust.verify_archive('get-dre/dre', 'csv-v1.0.0', 'archive', b'content', hashlib.sha256(b'content').hexdigest())


if __name__ == '__main__':
    unittest.main()
