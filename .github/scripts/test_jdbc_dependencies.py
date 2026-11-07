"""JDBC's resolved jars and declared licences must match the reviewed lock."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('jdbc_dependencies', Path(__file__).with_name('jdbc_dependencies.py'))
jdbc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(jdbc)


class JdbcDependencies(unittest.TestCase):
    def test_windows_runs_the_cmd_launcher_beside_a_unix_maven_path(self):
        with tempfile.TemporaryDirectory() as d:
            mvn = Path(d) / 'mvn'
            mvn.write_text('')
            (Path(d) / 'mvn.cmd').write_text('')
            self.assertEqual(jdbc.maven_command(str(mvn), windows=True), str(mvn) + '.cmd')
            self.assertEqual(jdbc.maven_command(str(mvn), windows=False), str(mvn))
            self.assertEqual(jdbc.maven_command('mvn', windows=True), 'mvn')

    def test_changed_transitive_jar_or_unapproved_license_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jar = root / 'example-1.0.jar'
            jar.write_bytes(b'reviewed')
            report = root / 'licenses.xml'
            report.write_text('''<licenseSummary><dependencies><dependency>
              <groupId>example</groupId><artifactId>example</artifactId><version>1.0</version>
              <licenses><license><name>MIT License</name></license></licenses>
              </dependency></dependencies></licenseSummary>''')
            entries = jdbc.inventory(report, [jar])
            jdbc.check(entries, entries)
            jar.write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError, 'lock'):
                jdbc.check(jdbc.inventory(report, [jar]), entries)
            report.write_text(report.read_text().replace('MIT License', 'Proprietary'))
            with self.assertRaisesRegex(ValueError, 'licen'):
                jdbc.inventory(report, [jar])
            report.write_text('<licenseSummary><dependencies/></licenseSummary>')
            with self.assertRaises(ValueError):
                jdbc.inventory(report, [jar])


if __name__ == '__main__':
    unittest.main()
