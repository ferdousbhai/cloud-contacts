import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

from cloud_contacts import cli_output

ROOT = Path(__file__).resolve().parent.parent


class ContractTests(unittest.TestCase):
    def run_cli(self, *args):
        with tempfile.TemporaryDirectory() as tmp:
            env = dict(os.environ, HOME=tmp, XDG_CONFIG_HOME=tmp)
            return subprocess.run([sys.executable, str(ROOT / 'bin/cloud-contacts'), *args], env=env, capture_output=True, text=True)

    def test_piped_output_is_the_envelope(self):
        result = self.run_cli('account', 'list')
        self.assertEqual(result.returncode, 0, result.stderr)
        body = json.loads(result.stdout)
        self.assertEqual(set(body), {'ok', 'data', 'summary', 'meta'})
        self.assertTrue(body['ok'])

    def test_output_modes(self):
        self.assertEqual(self.run_cli('account', 'list', '--quiet').stdout.strip(), '{}')
        self.assertEqual(self.run_cli('--count', 'account', 'list').stdout.strip(), '0')
        styled = self.run_cli('account', 'list', '--styled')
        self.assertEqual(styled.returncode, 0)
        self.assertFalse(styled.stdout.lstrip().startswith('{'))

    def test_usage_errors_are_json_with_exit_2(self):
        result = self.run_cli('no-such-command')
        self.assertEqual(result.returncode, 2)
        error = json.loads(result.stdout)['error']
        self.assertEqual(error['code'], 'usage')
        self.assertIn('--help', error['hint'])

    def test_exit_codes_follow_the_contract(self):
        for code, status in (('invalid_request', 2), ('not_configured', 3), ('auth_error', 3),
                             ('not_found', 4), ('provider_error', 5), ('local_error', 1)):
            with redirect_stdout(io.StringIO()) as out:
                self.assertEqual(cli_output.fail(code, 'x', 'json'), status, code)
            self.assertEqual(json.loads(out.getvalue())['error']['code'], code)
        with redirect_stderr(io.StringIO()) as err:
            cli_output.fail('not_found', 'gone', 'styled', hint='look elsewhere')
        self.assertEqual(err.getvalue(), 'error: gone\nhint: look elsewhere\n')
