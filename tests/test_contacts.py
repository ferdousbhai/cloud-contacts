import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from cloud_contacts.common import Failure, scoped_id
from cloud_contacts.service import Contacts, cloud_contact, google_contact, phone_key


class ContactTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.env = patch.dict(os.environ, {'HOME': self.tmp.name, 'XDG_CONFIG_HOME': self.tmp.name})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.service = Contacts()
        self.service.add_account('icloud')
        self.service.add_account('google')
        self.apple = cloud_contact('icloud', {'id': 'apple/id', 'name': 'Alex Smith', 'first': 'Alex',
                    'emails': [{'label': 'home', 'value': 'alex@example.com'}],
                    'phones': [{'label': 'mobile', 'value': '+1 415 555 0100'}], 'notes': 'Apple-only', 'has_photo': True})
        self.google = google_contact('google', {'resourceName': 'people/g1', 'names': [{'displayName': 'Alexander Smith', 'givenName': 'Alexander'}],
                     'emailAddresses': [{'value': 'ALEX@example.com'}], 'biographies': [{'value': 'Google-only'}],
                     'metadata': {'sources': [{'type': 'CONTACT', 'id': 'g1', 'etag': 'fresh'}]}})

    def rows(self):
        return patch.object(self.service, 'source_rows', return_value=([self.apple, self.google], []))

    def test_candidates_are_evidence_not_automatic_merges(self):
        with self.rows():
            candidates, _ = self.service.candidates()
            self.assertEqual(len(candidates), 1)
            self.assertTrue(candidates[0]['requires_identity_confirmation'])
        self.assertFalse(self.service.identities.exists())
        self.assertIsNone(phone_key('5550100'))
        self.assertIsNone(phone_key('+1 415 555 0100 ext 4'))

    def test_merge_preserves_every_source_and_conflicts_then_unmerges(self):
        with self.rows():
            preview = self.service.merge([self.apple['id'], self.google['id']])
            self.assertFalse(preview['applied'])
            self.assertFalse(self.service.identities.exists())
            merged = self.service.merge([self.apple['id'], self.google['id']], yes=True)
            records, _ = self.service.list()
            self.assertEqual(len(records), 1)
            record = records[0]
            self.assertEqual(record['id'], merged['id'])
            self.assertEqual(len(record['sources']), 2)
            self.assertEqual(record['sources'][0]['source']['notes'], 'Apple-only')
            self.assertEqual(record['sources'][1]['source']['biographies'][0]['value'], 'Google-only')
            self.assertIn('name', record['conflicts'])
            self.assertEqual(len(record['emails']), 1)
            self.service.unmerge(merged['id'], yes=True)
            self.assertEqual(len(self.service.list()[0]), 2)

    def test_merged_edit_must_target_one_of_its_source_records(self):
        with self.rows():
            identity = self.service.merge([self.apple['id'], self.google['id']], yes=True)['id']
        with self.assertRaises(Failure):
            self.service.write(identity, first='New')
        with self.assertRaises(Failure):
            self.service.write(identity, first='New', source='google:other')
        with patch.object(self.service, 'icloud', return_value={'contact': self.apple['source']}) as call:
            self.service.write(identity, source=self.apple['id'], first='New')
            self.assertEqual(call.call_args.args[0], ['edit', 'apple/id', '--first', 'New'])

    def test_google_edit_reads_fresh_etag_and_changes_only_named_fields(self):
        current = self.google['source']
        with patch.object(self.service, 'google', side_effect=[current, current]) as call:
            self.service.write(self.google['id'], emails=['new@example.com'])
        params, body = call.call_args.args[2:]
        self.assertEqual(params['updatePersonFields'], 'emailAddresses')
        self.assertEqual(body['metadata']['sources'][0]['etag'], 'fresh')
        self.assertEqual([value['value'] for value in body['emailAddresses']], ['ALEX@example.com', 'new@example.com'])
        self.assertNotIn('biographies', body)
        self.assertNotIn('names', body)

    def test_partial_account_retains_other_contacts_and_marks_merge_unavailable(self):
        with patch.object(self.service, 'icloud', return_value=[self.apple['source']]), \
             patch.object(self.service, 'google', side_effect=Failure('offline', 'fixture offline')):
            rows, warnings = self.service.list()
            self.assertEqual(len(rows), 1)
            self.assertEqual(warnings[0]['account'], 'google')
            with self.assertRaises(Failure):
                self.service.merge([self.apple['id'], self.google['id']], yes=True)

    def test_missing_merged_source_is_visible_and_does_not_unmerge(self):
        with self.rows():
            identity = self.service.merge([self.apple['id'], self.google['id']], yes=True)['id']
        with patch.object(self.service, 'source_rows', return_value=([self.apple], [{'account': 'google', 'code': 'offline', 'message': 'offline'}])):
            rows, _ = self.service.list()
            self.assertEqual(rows[0]['id'], identity)
            self.assertEqual(rows[0]['missing_sources'], [self.google['id']])

    def test_reusing_account_name_does_not_attach_old_merged_identity(self):
        with self.rows():
            identity = self.service.merge([self.apple['id'], self.google['id']], yes=True)['id']
        self.service.remove_account('google')
        self.service.add_account('google')
        with self.rows():
            records, _ = self.service.list()
            merged = next(row for row in records if row['id'] == identity)
            self.assertEqual(merged['missing_sources'], [self.google['id']])
            self.assertEqual(len(merged['sources']), 1)
            self.assertTrue(any(row['id'] == self.google['id'] for row in records))

    def test_stale_merged_source_cannot_write_to_a_relinked_account(self):
        with self.rows():
            identity = self.service.merge([self.apple['id'], self.google['id']], yes=True)['id']
        self.service.remove_account('google')
        self.service.add_account('google')
        with patch.object(self.service, 'google') as call:
            with self.assertRaises(Failure) as caught:
                self.service.write(identity, source=self.google['id'], first='Changed')
            self.assertEqual(caught.exception.code, 'stale_source')
            call.assert_not_called()

    def test_google_edit_deduplicates_requested_values_without_changing_existing_fields(self):
        current = self.google['source']
        with patch.object(self.service, 'google', side_effect=[current, current]) as call:
            self.service.write(self.google['id'], emails=['new@example.com', 'new@example.com', 'ALEX@example.com'])
        body = call.call_args.args[3]
        self.assertEqual(body['emailAddresses'], [*current['emailAddresses'], {'value': 'new@example.com'}])
        self.assertEqual(body['metadata'], current['metadata'])
