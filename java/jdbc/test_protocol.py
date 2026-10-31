#!/usr/bin/env python3
"""Public protocol checks. Build with Maven first; no database or vendor driver required."""

import json
import os
from pathlib import Path
import select
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent


class Plugin:
    def __init__(self):
        self.process = subprocess.Popen([str(ROOT / 'dre-plugin-jdbc')], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        self.send(type='hello', min_version=1, max_version=1, plugin='source/jdbc')
        self.info = self.receive()
        assert self.info['type'] == 'hello'

    def send(self, **message):
        self.send_json(json.dumps(message))

    def send_json(self, message):
        body = b'J' + message.encode()
        self.process.stdin.write(struct.pack('>I', len(body)) + body)
        self.process.stdin.flush()

    def read(self, length):
        data = b''
        while len(data) < length:
            ready, _, _ = select.select([self.process.stdout], [], [], 10)
            if not ready:
                raise AssertionError('plugin did not reply within 10 seconds')
            part = os.read(self.process.stdout.fileno(), length - len(data))
            if not part:
                raise AssertionError('plugin exited before replying')
            data += part
        return data

    def receive(self):
        length, = struct.unpack('>I', self.read(4))
        assert 0 < length <= 1 << 30
        body = self.read(length)
        assert body[0] == ord('J')
        return json.loads(body[1:])

    def close(self):
        if self.process.poll() is None:
            self.send(type='close', id=999)
            assert self.receive()['type'] == 'ok'
        self.process.wait(timeout=10)
        self.process.stdin.close()
        self.process.stdout.close()
        text = self.process.stderr.read().decode()
        self.process.stderr.close()
        return text


class ProtocolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory(prefix='dre-jdbc-driver-')
        base = Path(cls.directory.name)
        subprocess.run(['javac', '-d', str(base), str(ROOT / 'tests/FixtureDriver.java')], check=True)
        services = base / 'META-INF/services'
        services.mkdir(parents=True)
        (services / 'java.sql.Driver').write_text('fixture.FixtureDriver\n')
        cls.jar = base / 'driver with spaces.jar'
        subprocess.run(['jar', '--create', '--file', str(cls.jar), '-C', str(base), 'fixture',
                        '-C', str(base), 'META-INF'], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def setUp(self):
        self.plugin = Plugin()
        self.config = {'url': 'jdbc:fixture:scalars', 'driver_jars': [str(self.jar)],
                       'properties': {'Mixed.Case': True, 'number': 42,
                                      'decimal': '12345678901234567890.125'}}

    def tearDown(self):
        self.assertEqual(self.plugin.close(), '')

    def request(self, **message):
        self.plugin.send(id=1, **message)
        reply = self.plugin.receive()
        self.assertEqual(reply['id'], 1)
        return reply

    def test_scalar_properties_keep_case_and_driver_discovery_uses_supplied_jars(self):
        self.assertEqual(self.request(type='open', connection=self.config)['type'], 'ok')

    def test_committed_descriptor_matches_protocol(self):
        description = self.request(type='describe')
        self.assertEqual(description.pop('type'), 'describe')
        description.pop('id')
        actual = {'source/jdbc': {'capabilities': sorted(self.plugin.info['capabilities']), **description}}
        path = ROOT / 'describe.json'
        if os.environ.get('DRE_UPDATE_DESCRIBE'):
            path.write_text(json.dumps(actual, indent=2) + '\n')
        self.assertEqual(actual, json.loads(path.read_text()))

    def test_explicit_driver_class_and_single_jar_path(self):
        self.config.update(driver_class='fixture.FixtureDriver', driver_jars=str(self.jar))
        self.assertEqual(self.request(type='open', connection=self.config)['type'], 'ok')

    def test_numeric_decimal_properties_do_not_pass_through_binary_floating_point(self):
        message = json.dumps({'type': 'open', 'id': 1, 'connection': self.config})
        self.plugin.send_json(message.replace('"12345678901234567890.125"', '12345678901234567890.125'))
        self.assertEqual(self.plugin.receive()['type'], 'ok')

    def test_connection_errors_and_driver_output_never_expose_secrets(self):
        self.config['url'] = 'jdbc:fixture:URL_SENTINEL:failure'
        self.config['properties']['VendorCredential'] = 'PROPERTY_SENTINEL'
        reply = self.request(type='open', connection=self.config)
        self.assertEqual(reply['kind'], 'auth')
        self.assertIn('28000', reply['message'])
        self.assertIn('1017', reply['message'])
        self.assertNotIn('SENTINEL', json.dumps(reply))
        self.assertEqual(self.request(type='describe')['type'], 'describe')

    def test_complex_and_null_properties_name_fields_without_values(self):
        self.config['properties'] = {'nested': {'token': 'SECRET'}, 'list': ['SECRET'], 'null': None}
        reply = self.request(type='validate_connection', connection=self.config)
        self.assertEqual(len(reply['errors']), 3)
        self.assertNotIn('SECRET', json.dumps(reply))
        self.assertIn('properties.nested', reply['errors'][0])
        self.assertEqual(self.request(type='open', connection=self.config)['kind'], 'config')

    def test_owned_fields_are_checked_and_unknown_keys_warn(self):
        self.config.update(url='not JDBC', driver_jars='/does/not/exist.jar', propertiez={})
        reply = self.request(type='validate_connection', connection=self.config)
        self.assertEqual(len(reply['errors']), 2)
        self.assertIn('propertiez', reply['warnings'][0])

    def test_unresolved_environment_fields_are_not_revalidated(self):
        reply = self.request(type='validate_connection', connection={'url': None, 'driver_jars': None},
                             unresolved=['url', 'driver_jars'])
        self.assertEqual(reply['errors'], [])

    def test_restricted_sessions_are_refused(self):
        reply = self.request(type='open', connection=self.config, read_only=True)
        self.assertEqual(reply['kind'], 'refused')

    def test_catalog_returns_accessible_metadata_and_identifier_rules(self):
        self.config['url'] = 'jdbc:fixture:catalog'
        self.assertEqual(self.request(type='open', connection=self.config)['type'], 'ok')
        reply = self.request(type='catalog', tables=['sales schema.order items'],
                             schemas=[], profile='fixture', target='dev')
        self.assertEqual(reply['type'], 'catalog')
        catalog = reply['catalog']
        self.assertEqual(catalog['current_schema'], 'fixture')
        self.assertEqual(catalog['tables'][0]['name'], 'ORDER ITEMS')
        self.assertEqual(len(catalog['tables']), 1)
        self.assertEqual(catalog['tables'][0]['columns'][0]['name'], 'Order ID')
        self.assertEqual(
            catalog['tables'][0]['columns'][0]['type'], 'NUMBER(18,0)')
        self.assertEqual(catalog['tables'][0]['columns'][0]['description'], 'item key')
        self.assertEqual(catalog['dialect']['identifier_case'], 'upper_unless_quoted')
        self.assertTrue(catalog['identifier_metadata']['stores_mixed_case_quoted'])
        self.assertEqual(catalog['warnings'], [])
        self.assertNotIn('jdbc:fixture', json.dumps(reply))

    def test_catalog_permission_denied_has_categorical_diagnostic(self):
        self.config['url'] = 'jdbc:fixture:permission'
        self.assertEqual(self.request(type='open', connection=self.config)['type'], 'ok')
        reply = self.request(type='catalog', schemas=['denied'], tables=[], profile='fixture', target='dev')
        self.assertEqual(reply['kind'], 'query')
        self.assertIn('schema metadata', reply['message'])
        self.assertNotIn('jdbc:fixture', json.dumps(reply))

    def test_catalog_identifier_metadata_does_not_leak_connection(self):
        self.config['url'] = 'jdbc:fixture:catalog'
        self.assertEqual(self.request(type='open', connection=self.config)['type'], 'ok')
        reply = self.request(type='catalog', tables=[], schemas=['sales schema'],
                             profile='fixture', target='dev')
        self.assertEqual(reply['type'], 'catalog')
        self.assertEqual(reply['catalog']['warnings'], [])
        self.assertEqual(
            reply['catalog']['identifier_metadata'],
        {'stores_upper_case': True, 'stores_lower_case': False,
             'stores_mixed_case_quoted': True, 'max_schema_name_length': 30,
             'max_table_name_length': 128, 'max_column_name_length': 30})

    def test_statement_cleanup_failure_has_one_error_reply_not_success_then_error(self):
        self.assertEqual(self.request(type='open', connection=self.config)['type'], 'ok')
        reply = self.request(type='execute', sql='synthetic update')
        self.assertEqual(reply['type'], 'error')
        self.assertEqual(reply['kind'], 'query')
        self.assertIn('SQLSTATE HY000', reply['message'])
        self.assertIn('vendor code 999', reply['message'])
        self.assertEqual(self.request(type='describe')['type'], 'describe')

    def test_eof_ends_process(self):
        self.plugin.process.stdin.close()
        self.assertEqual(self.plugin.process.wait(timeout=10), 0)


if __name__ == '__main__':
    unittest.main()
