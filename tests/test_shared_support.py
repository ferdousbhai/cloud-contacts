"""Vendored support checks against disposable checkouts, never real account data."""
import hashlib
import importlib.machinery
import json
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

CHECKER = Path(__file__).resolve().parents[1] / 'bin/check-shared-support'
loader = importlib.machinery.SourceFileLoader('shared_support_checker', str(CHECKER))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)


class SharedSupportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name, members in module.GROUPS.items():
            # Synthetic source bytes keep tests independent of sibling checkouts.
            for member in members:
                target = self.root / member
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(f'fixture source for {name}\n'.encode())
        for repo in module.REPOS:
            target = self.root / repo / 'bin/check-shared-support'
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(CHECKER, target)
            manifest = {name: hashlib.sha256((self.root / members[0]).read_bytes()).hexdigest()
                        for name, members in module.GROUPS.items()
                        if any(member.startswith(repo + '/') for member in members)}
            (self.root / repo / 'shared-support.json').write_text(json.dumps({'product': repo, 'groups': manifest}))
        self.checker = self.root / 'cloud-contacts/bin/check-shared-support'

    def run_check(self, *args):
        return subprocess.run([sys.executable, str(self.checker),
                               '--workspace-root', str(self.root), *args], text=True, capture_output=True)

    def test_full_workspace_matches(self):
        result = self.run_check('--require-all')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('17 files', result.stdout)

    def test_drift_fails_and_record_refuses_to_hide_different_sources(self):
        path = self.root / module.GROUPS['common.py'][1]
        path.write_bytes(path.read_bytes() + b'\n# fixture change\n')
        before = (self.root / 'cloud-contacts/bin/check-shared-support').read_bytes()
        self.assertNotEqual(self.run_check().returncode, 0)
        self.assertNotEqual(self.run_check('--record', 'common.py').returncode, 0)
        self.assertEqual((self.root / 'cloud-contacts/bin/check-shared-support').read_bytes(), before)

    def test_record_updates_scoped_manifests_without_changing_sources(self):
        for member in module.GROUPS['common.py']:
            path = self.root / member
            path.write_bytes(path.read_bytes() + b'\n# agreed fixture change\n')
        before = {member: (self.root / member).read_bytes()
                  for members in module.GROUPS.values() for member in members}
        self.assertEqual(self.run_check('--record', 'common.py').returncode, 0)
        result = self.run_check('--require-all')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(before, {member: (self.root / member).read_bytes() for member in before})

    def test_standalone_clone_checks_digests_and_strict_mode_requires_siblings(self):
        for repo in module.REPOS:
            if repo != 'cloud-contacts':
                shutil.rmtree(self.root / repo)
        self.assertEqual(self.run_check().returncode, 0)
        self.assertNotEqual(self.run_check('--require-all').returncode, 0)
        path = self.root / module.GROUPS['common.py'][0]
        path.write_bytes(path.read_bytes() + b'\n# fixture change\n')
        self.assertIn('Product source digest mismatch', self.run_check().stderr)

    def test_python_group_record_does_not_require_unrelated_rust_repositories(self):
        for repo in module.REPOS:
            if repo not in ('cloud-contacts', 'cloud-tasks'):
                shutil.rmtree(self.root / repo)
        for member in module.GROUPS['common.py']:
            path = self.root / member
            path.write_bytes(path.read_bytes() + b'\n# agreed fixture change\n')
        result = self.run_check('--record', 'common.py')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.run_check().returncode, 0)

    def test_standalone_noncanonical_copy_still_checks_recorded_digest(self):
        for repo in module.REPOS:
            if repo != 'cloud-tasks':
                shutil.rmtree(self.root / repo)
        command = [sys.executable, str(self.root / 'cloud-tasks/bin/check-shared-support'),
                   '--workspace-root', str(self.root)]
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
        path = self.root / module.GROUPS['common.py'][1]
        path.write_bytes(path.read_bytes() + b'\n# local drift\n')
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Product source digest mismatch', result.stderr)

    def test_renamed_standalone_checkout_uses_local_product_manifest(self):
        for repo in module.REPOS:
            if repo != 'cloud-contacts':
                shutil.rmtree(self.root / repo)
        renamed = self.root / 'release-0.2.0'
        (self.root / 'cloud-contacts').rename(renamed)
        self.checker = renamed / 'bin/check-shared-support'
        # Both automatic root selection and an explicit workspace root work.
        self.assertEqual(subprocess.run([sys.executable, str(self.checker)], capture_output=True).returncode, 0)
        self.assertEqual(self.run_check().returncode, 0)
        source = renamed / 'cloud_contacts/common.py'
        source.write_bytes(source.read_bytes() + b'\n# drift\n')
        self.assertIn('Product source digest mismatch', self.run_check().stderr)

    def test_renamed_checkout_checks_present_sibling_and_records_scoped_hashes(self):
        renamed = self.root / 'my-contacts-worktree'
        (self.root / 'cloud-contacts').rename(renamed)
        self.checker = renamed / 'bin/check-shared-support'
        self.assertEqual(self.run_check('--require-all').returncode, 0)
        peer = self.root / 'cloud-tasks/cloud_tasks/common.py'
        peer.write_bytes(peer.read_bytes() + b'\n# agreed change\n')
        self.assertIn('Product source copies differ', self.run_check().stderr)
        (renamed / 'cloud_contacts/common.py').write_bytes(peer.read_bytes())
        result = self.run_check('--record', 'common.py')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.run_check('--require-all').returncode, 0)

    def test_missing_source_and_checker_drift_fail(self):
        path = self.root / module.GROUPS['theme.py'][1]
        path.unlink()
        self.assertIn('Missing shared file', self.run_check().stderr)
        checker = self.root / 'omacloud/bin/check-shared-support'
        checker.write_bytes(checker.read_bytes() + b'\n# fixture checker change\n')
        self.assertIn('Checker sync mismatch', self.run_check('--require-all').stderr)


if __name__ == '__main__':
    unittest.main()
