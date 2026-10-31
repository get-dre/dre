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
    def test_jdbc_archives_enter_the_plugin_registry(self):
        archive = 'dre-plugin-jdbc-1.0.0-linux-x86_64.tar.gz'
        release = {'tag_name': 'jdbc-v1.0.0', 'assets': [
            {'name': archive, 'browser_download_url': 'https://example.test/' + archive},
            {'name': 'SHA256SUMS', 'browser_download_url': 'https://example.test/SHA256SUMS'}]}
        with tempfile.TemporaryDirectory() as temporary:
            old = Path(temporary) / 'packages.json'
            output = io.StringIO()
            with patch.object(registry, 'releases', return_value=iter([release])), \
                 patch.object(registry, 'get', return_value=('a' * 64 + '  ' + archive + '\n').encode()), \
                 contextlib.redirect_stdout(output):
                registry.main(old, 'get-dre/dre')
        jdbc = next(p for p in json.loads(output.getvalue())['plugins'] if p['name'] == 'jdbc')
        self.assertEqual(jdbc['provides'], ['source/jdbc'])
        self.assertEqual(jdbc['versions'][0]['artifacts']['linux-x86_64']['sha256'], 'a' * 64)

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

    def test_registry_rebuild_preserves_font_packages(self):
        fonts = [{'name': 'noto_sans_jp', 'scripts': ['ja'], 'license': 'OFL-1.1',
                  'versions': [{'version': '2.4.0', 'regular': {'url': 'font.otf', 'sha256': 'a' * 64}}]}]
        with tempfile.TemporaryDirectory() as temporary:
            old = Path(temporary) / 'packages.json'
            old.write_text(json.dumps({'schema': 2, 'plugins': [], 'fonts': fonts}))
            output = io.StringIO()
            with patch.object(registry, 'releases', return_value=iter([])), contextlib.redirect_stdout(output):
                registry.main(old, 'get-dre/dre')
        self.assertEqual(json.loads(output.getvalue())['fonts'], fonts)

    def test_signatures_and_catalog_settings_reach_the_index(self):
        archive = 'dre-mcp-0.1.0-rc.2-linux-x86_64.tar.gz'
        release = {'tag_name': 'dre-mcp-v0.1.0-rc.2', 'assets': [
            {'name': archive, 'browser_download_url': 'https://example.test/' + archive},
            {'name': archive + '.minisig', 'browser_download_url': 'https://example.test/sig'},
            {'name': 'SHA256SUMS', 'browser_download_url': 'https://example.test/SHA256SUMS'}]}
        replies = {'https://example.test/SHA256SUMS': ('b' * 64 + '  ' + archive + '\n').encode(),
                   'https://example.test/sig': b'untrusted comment: test\nRUQ...\n'}
        # An entry from before `command` existed is brought up to date.
        original = {'schema': 2, 'plugins': [], 'extensions': [{'name': 'dre-mcp', 'kind': 'extension',
                    'license': 'LicenseRef-Proprietary', 'proprietary_license': True, 'versions': []}]}
        with tempfile.TemporaryDirectory() as temporary:
            old = Path(temporary) / 'packages.json'
            old.write_text(json.dumps(original))
            output = io.StringIO()
            with patch.object(registry, 'releases', return_value=iter([release])), \
                 patch.object(registry, 'get', side_effect=replies.__getitem__), \
                 contextlib.redirect_stdout(output):
                registry.main(old, 'get-dre/dre')
        entry = json.loads(output.getvalue())['extensions'][0]
        self.assertEqual(entry['command'], 'mcp')
        artifact = entry['versions'][0]['artifacts']['linux-x86_64']
        self.assertEqual(artifact['minisig'], 'untrusted comment: test\nRUQ...\n')

    def test_archive_without_its_checksum_is_not_indexed(self):
        release = {'assets': [{'name': 'dre-mcp-0.1.0-rc.1-macos-aarch64.tar.gz',
                              'browser_download_url': 'https://example.test/archive'},
                             {'name': 'SHA256SUMS', 'browser_download_url': 'https://example.test/sums'}]}
        with patch.object(registry, 'get', return_value=b''):
            self.assertIsNone(registry.artifacts(release, 'dre-mcp', '0.1.0-rc.1', extension=True))

if __name__ == '__main__':
    unittest.main()
