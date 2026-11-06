"""JDBC's resolved jars and declared licences must match the reviewed lock."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('jdbc_dependencies', Path(__file__).with_name('jdbc_dependencies.py'))
jdbc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(jdbc)


class JdbcDependencies(unittest.TestCase):
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
