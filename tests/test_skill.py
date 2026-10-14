import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from cloud_contacts import agent_skill
from cloud_contacts.cli import SKILL

ROOT = Path(__file__).resolve().parent.parent


class SkillTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        (self.home / '.claude').mkdir()

    def tearDown(self):
        self.tmp.cleanup()

    def test_ships_the_repository_skill(self):
        self.assertTrue(SKILL.body.startswith('---\nname: cloud-contacts\n'))

    def test_installs_links_refreshes_and_removes(self):
        out = agent_skill.install(self.home, SKILL)
        installed = self.home / '.agents/skills/cloud-contacts/SKILL.md'
        self.assertEqual(installed.read_text(), SKILL.body)
        link = self.home / '.claude/skills/cloud-contacts'
        self.assertEqual(os.readlink(link), '../../.agents/skills/cloud-contacts')
        self.assertEqual(len(out['paths']), 2)
        agent_skill.install(self.home, SKILL)  # idempotent
        (installed.parent / '.installed-version').write_text('0.0.0')
        installed.write_text('old')
        self.assertTrue(agent_skill.refresh(self.home, SKILL))
        self.assertFalse(agent_skill.refresh(self.home, SKILL))
        self.assertEqual(installed.read_text(), SKILL.body)
        agent_skill.remove(self.home, SKILL)
        self.assertFalse(installed.parent.exists())
        self.assertFalse(link.is_symlink())

    def test_never_touches_what_it_did_not_create(self):
        checkout = self.home / 'checkout'
        checkout.mkdir()
        (checkout / 'SKILL.md').write_text('mine')
        (self.home / '.agents/skills').mkdir(parents=True)
        (self.home / '.agents/skills/cloud-contacts').symlink_to(checkout)
        with self.assertRaises(agent_skill.Unmanaged):
            agent_skill.install(self.home, SKILL)
        self.assertFalse(agent_skill.refresh(self.home, SKILL))
        agent_skill.remove(self.home, SKILL)
        self.assertEqual((checkout / 'SKILL.md').read_text(), 'mine')
        (self.home / '.agents/skills/cloud-contacts').unlink()
        (self.home / '.claude/skills/cloud-contacts').mkdir(parents=True)
        out = agent_skill.install(self.home, SKILL)
        self.assertEqual(len(out['notices']), 1)
        agent_skill.remove(self.home, SKILL)
        self.assertTrue((self.home / '.claude/skills/cloud-contacts').is_dir())

    def test_cli_reports_an_unmanaged_skill_as_json(self):
        env = dict(os.environ, HOME=str(self.home), XDG_CONFIG_HOME=str(self.home))
        (self.home / '.agents/skills/cloud-contacts').mkdir(parents=True)
        (self.home / '.agents/skills/cloud-contacts/SKILL.md').write_text('mine')
        result = subprocess.run([sys.executable, str(ROOT / 'bin/cloud-contacts'), 'skill', 'install'], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout)['error']['code'], 'skill_unmanaged')


class LegacyTests(unittest.TestCase):
    def test_adopts_only_a_pre_marker_copy_when_enabled(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            target = home / '.agents/skills/cloud-contacts'
            target.mkdir(parents=True)
            (target / 'SKILL.md').write_text('old')
            with self.assertRaises(agent_skill.Unmanaged):
                agent_skill.install(home, SKILL)  # this CLI never wrote unmarked copies
            legacy = agent_skill.Skill('cloud-contacts', 'cloud-contacts', '9', SKILL.source, adopts_legacy=True)
            self.assertTrue(agent_skill.refresh(home, legacy))
            self.assertEqual((target / 'SKILL.md').read_text(), SKILL.body)
            (target / 'notes.md').write_text('mine')
            for name in ('.managed-by-cloud-contacts', '.installed-version'):
                (target / name).unlink()
            with self.assertRaises(agent_skill.Unmanaged):
                agent_skill.install(home, legacy)
