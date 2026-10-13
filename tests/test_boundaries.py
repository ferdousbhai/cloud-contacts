import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from cloud_contacts.common import Context, Failure, scoped_id, split_id


class BoundaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = patch.dict(os.environ, {'HOME': str(self.root), 'XDG_CONFIG_HOME': str(self.root / 'config')})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.context = Context('cloud-contacts')

    def test_account_config_isolated_and_removal_preserves_credentials(self):
        account = self.context.add_account('google', 'work')
        self.assertEqual(account['gws_config'], str(self.context.path.parent / 'gws/work'))
        credentials = Path(account['gws_config']) / 'credentials.enc'
        credentials.parent.mkdir(parents=True)
        credentials.write_text('test-only')
        self.context.remove_account('work')
        self.assertTrue(credentials.exists())
        self.assertEqual(self.context.accounts(), {})
        self.assertEqual(self.context.path.stat().st_mode & 0o777, 0o600)

    def test_reject_duplicate_apple_session_and_unsafe_account_names(self):
        self.context.add_account('icloud')
        with self.assertRaises(Failure):
            self.context.add_account('icloud', 'second')
        for name in ['../escape', 'a:b', 'person']:
            with self.assertRaises(Failure):
                self.context.add_account('google', name)

    def test_google_credentials_cannot_leak_from_parent_environment(self):
        self.context.add_account('google')
        with patch.dict(os.environ, {'GOOGLE_WORKSPACE_CLI_TOKEN': 'test-token',
                                    'GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE': '/not-real',
                                    'GOOGLE_APPLICATION_CREDENTIALS': '/not-real'}):
            env = self.context.google_env('google')
        self.assertNotIn('GOOGLE_WORKSPACE_CLI_TOKEN', env)
        self.assertNotIn('GOOGLE_APPLICATION_CREDENTIALS', env)
        self.assertNotIn('GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE', env)
        self.assertTrue(env['GOOGLE_WORKSPACE_CLI_CONFIG_DIR'].endswith('/gws/google'))

    def test_provider_is_argv_not_shell_and_config_is_forwarded(self):
        tool = self.root / 'fake-gws'
        tool.write_text('#!/usr/bin/env python3\nimport json,os,sys\nprint(json.dumps({"argv":sys.argv[1:],"config":os.environ.get("GOOGLE_WORKSPACE_CLI_CONFIG_DIR")}))\n')
        tool.chmod(0o755)
        self.context.add_account('google')
        payload = '$(touch SHOULD_NOT_EXIST); `false`'
        with patch.dict(os.environ, {self.context.env_prefix + '_GWS_COMMAND': str(tool)}):
            result = self.context.google('google', ['tasks', 'tasks', 'insert'], {'tasklist': 'id'}, {'title': payload})
        self.assertEqual(json.loads(result['argv'][-1]), {'title': payload})
        self.assertEqual(result['config'], self.context.account('google')['gws_config'])
        self.assertFalse((self.root / 'SHOULD_NOT_EXIST').exists())

    def test_pagination_is_complete_and_repeated_token_fails(self):
        self.context.add_account('google')
        with patch.object(self.context, 'google', side_effect=[{'items': [1], 'nextPageToken': 'next'}, {'items': [2]}]) as call:
            self.assertEqual(self.context.pages('google', ['tasks', 'tasklists', 'list'], {}, 'items'), [1, 2])
            self.assertEqual(call.call_args.args[2]['pageToken'], 'next')
        with patch.object(self.context, 'google', return_value={'items': [], 'nextPageToken': 'same'}):
            with self.assertRaises(Failure):
                self.context.pages('google', [], {}, 'items')

    def test_all_accounts_failing_is_not_an_empty_success(self):
        self.context.add_account('icloud')
        def fail(*_):
            raise Failure('offline', 'fixture offline')
        with self.assertRaises(Failure) as caught:
            self.context.collect(fail)
        self.assertEqual(caught.exception.code, 'accounts_unavailable')

    def test_opaque_ids_round_trip_reserved_characters(self):
        raw = 'people/a:b /?+%'
        self.assertEqual(split_id(scoped_id('work', raw)), ('work', raw))

    def test_cli_help_does_not_access_accounts(self):
        root = Path(__file__).resolve().parent.parent
        result = subprocess.run([str(root / 'bin/cloud-contacts'), '--help'], text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run([str(root / 'bin/cloud-contacts'), '--json', 'account', 'list'], text=True, capture_output=True)
        self.assertEqual(json.loads(result.stdout)['data'], {})
