#!/usr/bin/env python3
"""Run an ordinary Oracle-backed DRE report and inspect its saved workbook and diagnostics."""

import json
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('check_examples', ROOT.parents[1] / '.github/scripts/check_examples.py')
check_examples = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_examples)


class OracleReport(unittest.TestCase):
    def test_oracle_report_and_failed_credentials_through_the_cli(self):
        env = os.environ.copy()
        cli = env['DRE_BIN']
        xlsx = Path(env['DRE_TEST_XLSX_BIN'])
        with tempfile.TemporaryDirectory(prefix='dre-jdbc-report-') as temporary:
            base = Path(temporary)
            plugins = base / 'plugins with spaces'
            plugins.mkdir()
            shutil.copy2(ROOT / 'dre-plugin-jdbc', plugins)
            (plugins / 'target').symlink_to(ROOT / 'target', target_is_directory=True)
            (plugins / 'dre-plugin-xlsx').symlink_to(xlsx)
            project = base / 'project'
            shutil.copytree(ROOT / 'tests/report', project)
            env['DRE_PLUGINS_DIR'] = str(plugins)
            run = subprocess.run([cli, 'run', 'accounts', '--preview'], cwd=project,
                                 env=env, capture_output=True, text=True, timeout=60)
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            outputs = list(project.glob('target/run/**/accounts.xlsx'))
            self.assertEqual(len(outputs), 1)
            cells = check_examples.workbook(outputs[0])['Accounts']
            self.assertEqual(cells['A2'][0], '000019')
            self.assertEqual(cells['A3'][0], '12345678901234567890')
            self.assertEqual(cells['B2'][0], 12.34)
            self.assertNotIn('B3', cells)
            self.assertEqual(cells['D2'][0], '2026/01/31')
            self.assertEqual(cells['D3'][0], '2026/01/31')
            self.assertIsInstance(cells['C2'][0], float)
            self.assertEqual(cells['C2'][0], 46053.0)
            self.assertEqual(cells['E2'][0].lower(), '002aff')
            self.assertEqual(cells['F2'][0], 46053.5)
            env['DRE_TEST_JDBC_PASSWORD'] = 'CLI_JDBC_PASSWORD_SENTINEL'
            env['DRE_TEST_JDBC_VENDOR_SECRET'] = 'VENDOR_PROPERTY_SENTINEL'
            for complete_url in [False, True]:
                if complete_url:
                    profile = project / 'profiles.yml'
                    profile.write_text(profile.read_text().split('        properties:')[0])
                    user = env.get('DRE_TEST_JDBC_USER', 'dre_fixture')
                    env['DRE_TEST_JDBC_URL'] = env['DRE_TEST_JDBC_URL'].replace(
                        'jdbc:oracle:thin:@', f'jdbc:oracle:thin:{user}/CLI_URL_SECRET_SENTINEL@')
                failed = subprocess.run([cli, 'run', 'accounts', '--preview'], cwd=project,
                                        env=env, capture_output=True, text=True, timeout=60)
                self.assertNotEqual(failed.returncode, 0)
                self.assertNotIn('SENTINEL', failed.stdout + failed.stderr)
                for path in (project / 'target').rglob('*'):
                    if path.is_file() and path.suffix in ('.json', '.log', '.yml'):
                        self.assertNotIn('SENTINEL', path.read_text())
            manifests = list((project / 'target').rglob('run_results.json'))
            self.assertTrue(manifests)
            self.assertTrue(any(json.loads(path.read_text()) for path in manifests))
            shutil.copy2(ROOT / 'tests/report/profiles.yml', project / 'profiles.yml')
            env['DRE_TEST_JDBC_URL'] = os.environ['DRE_TEST_JDBC_URL']
            env['DRE_TEST_JDBC_PASSWORD'] = os.environ['DRE_TEST_JDBC_PASSWORD']
            (project / 'reports/accounts/accounts.sql').write_text(
                "select timestamp '2026-01-31 01:02:03.123456' as precise from dual;\n")
            unsupported = subprocess.run([cli, 'run', 'accounts', '--preview'], cwd=project,
                                         env=env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(unsupported.returncode, 0)
            self.assertIn('cast', unsupported.stdout + unsupported.stderr)


if __name__ == '__main__':
    unittest.main()
