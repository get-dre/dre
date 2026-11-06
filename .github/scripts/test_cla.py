"""The CLA bot never evaluates pull-request code or accepts another user's signature."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('cla', Path(__file__).with_name('cla.py'))
cla = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cla)


class Cla(unittest.TestCase):
    def test_every_commit_author_must_sign_by_id(self):
        authors = [{'author': {'id': 1, 'login': 'alice'}}, {'author': {'id': 2, 'login': 'bob'}}]
        self.assertEqual(cla.unsigned(authors, [{'id': 1, 'name': 'old-login'}]), ['bob'])
        self.assertEqual(cla.unsigned([{'author': None}], []), ['unlinked commit author'])
        self.assertEqual(cla.unsigned([{'author': {'id': 3, 'login': 'dependabot[bot]'}}], []), [])

    def test_only_exact_signature_comment_records_the_comment_author(self):
        event = {'comment': {'body': cla.SIGN, 'user': {'id': 2, 'login': 'bob'},
                             'id': 42, 'created_at': 'today'}}
        with patch.object(cla, 'save_signature') as save:
            cla.sign_comment('get-dre/dre', 7, event)
            self.assertEqual(save.call_args.args[1]['id'], 2)
            self.assertEqual(save.call_args.args[1]['comment_id'], 42)
            save.reset_mock()
            event['comment']['body'] = 'recheck'
            cla.sign_comment('get-dre/dre', 7, event)
            save.assert_not_called()


if __name__ == '__main__':
    unittest.main()
