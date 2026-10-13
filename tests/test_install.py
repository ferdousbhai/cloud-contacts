import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class InstallerTests(unittest.TestCase):
    def test_source_install_is_repeatable_and_works_with_spaces(self):
        root = Path(__file__).resolve().parent.parent
        with tempfile.TemporaryDirectory() as tmp:
            prefix = Path(tmp) / 'prefix with spaces'
            env = os.environ.copy()
            env.update({'HOME': tmp, 'XDG_CONFIG_HOME': tmp})
            for _ in range(2):
                result = subprocess.run([str(root / 'install.sh'), str(prefix)], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
            binary = prefix / 'bin/cloud-contacts'
            result = subprocess.run([str(binary), 'account', 'list', '--json'], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['data'], {})
            desktop = prefix / 'share/applications/com.ferdousbhai.cloudcontacts.desktop'
            self.assertIn('Exec="' + str(binary) + '" --app', desktop.read_text())
