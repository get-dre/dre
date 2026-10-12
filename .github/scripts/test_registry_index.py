"""Public extension releases use plugin registry checksums without exposing private source."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('registry_index', Path(__file__).with_name('registry_index.py'))
registry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(registry)

class RegistryExtensions(unittest.TestCase):
    def test_companion_release_is_separate_and_preserves_plugins(self):
        archive = 'dre-mcp-0.1.0-rc.1-linux-x86_64.tar.gz'
        release = {'tag_name': 'dre-mcp-v0.1.0-rc.1', 'assets': [
            {'name': archive, 'browser_download_url': 'https://example.test/' + archive},
            {'name': 'SHA256SUMS', 'browser_download_url': 'https://example.test/SHA256SUMS'}]}
        original = {'schema': 2, 'plugins': [{'name': 'custom', 'description': 'Existing',
                    'provides': [], 'versions': []}]}
        with tempfile.TemporaryDirectory() as temporary:
            old = Path(temporary) / 'packages.json'
            old.write_text(json.dumps(original))
            for published, expected in [([release], 1), ([dict(release, draft=True)], 0)]:
                output = io.StringIO()
                with patch.object(registry, 'releases', return_value=iter(published)), \
                     patch.object(registry, 'get', return_value=('a' * 64 + '  ' + archive + '\n').encode()), \
                     contextlib.redirect_stdout(output):
                    registry.main(old, 'get-dre/dre')
                result = json.loads(output.getvalue())
                self.assertEqual(result['plugins'], original['plugins'])
                self.assertEqual(len(result['extensions'][0]['versions']), expected)
                if expected:
                    self.assertEqual(result['extensions'][0]['versions'][0]['artifacts']['linux-x86_64']['sha256'], 'a' * 64)

    def test_archive_without_its_checksum_is_not_indexed(self):
        release = {'assets': [{'name': 'dre-mcp-0.1.0-rc.1-macos-aarch64.tar.gz',
                              'browser_download_url': 'https://example.test/archive'},
                             {'name': 'SHA256SUMS', 'browser_download_url': 'https://example.test/sums'}]}
        with patch.object(registry, 'get', return_value=b''):
            self.assertIsNone(registry.artifacts(release, 'dre-mcp', '0.1.0-rc.1', extension=True))

if __name__ == '__main__':
    unittest.main()
