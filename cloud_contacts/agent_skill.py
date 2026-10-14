"""Install this CLI's agent skill for coding agents (hey-cli convention).

The skill ships beside the package (``skills/<name>/SKILL.md``) and is copied to
``~/.agents/skills/<name>/`` (read by Codex, Pi and others); Claude Code gets a
``~/.claude/skills/<name>`` link to it. Only directories this CLI created
(marked by ``.managed-by-<cli>``) are ever written, refreshed or removed; a
symlink or a hand-authored skill at that path is left alone. The Rust
products carry the same logic in ``agent_skill.rs``; keep them in sync.
"""
from __future__ import annotations

import os
from pathlib import Path

SKILL_FILE = 'SKILL.md'
VERSION_FILE = '.installed-version'


class Unmanaged(Exception):
    """The skill path exists but this CLI did not create it."""


class Skill:
    def __init__(self, name: str, cli: str, version: str, source: Path, adopts_legacy: bool = False):
        self.name, self.cli, self.version, self.source = name, cli, version, source
        # Adopt an unmarked directory holding only SKILL.md (a pre-marker release's copy).
        self.adopts_legacy = adopts_legacy

    @property
    def body(self) -> str:
        return self.source.read_text()

    @property
    def marker(self) -> str:
        return f'.managed-by-{self.cli}'

    def dir(self, home: Path) -> Path:
        return home / '.agents/skills' / self.name

    def claude_link(self, home: Path) -> Path:
        return home / '.claude/skills' / self.name

    @property
    def claude_target(self) -> str:
        return f'../../.agents/skills/{self.name}'

    def owns(self, path: Path) -> bool:
        marker = path / self.marker
        return path.is_dir() and not path.is_symlink() and marker.is_file() and not marker.is_symlink()

    def legacy(self, path: Path) -> bool:
        if not self.adopts_legacy or path.is_symlink() or not path.is_dir():
            return False
        names = [entry.name for entry in path.iterdir()]
        return names == [SKILL_FILE] and (path / SKILL_FILE).is_file() and not (path / SKILL_FILE).is_symlink()

    def unmanaged(self, path: Path) -> Unmanaged:
        return Unmanaged(f'{path} exists but was not written by {self.cli}; move it aside to let {self.cli} install its skill there')


def home() -> Path | None:
    value = os.environ.get('HOME')
    return Path(value) if value else None


def _write(path: Path, data: str, skill: Skill) -> None:
    """Write a regular file, refusing to follow a symlink or replace anything else."""
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise skill.unmanaged(path)
    path.write_text(data)


def install(home: Path, skill: Skill) -> dict:
    target = skill.dir(home)
    if target.is_symlink() or (target.exists() and not target.is_dir()):
        raise skill.unmanaged(target)
    if target.is_dir() and not skill.owns(target) and not skill.legacy(target) and any(target.iterdir()):
        raise skill.unmanaged(target)
    target.mkdir(parents=True, exist_ok=True)
    _write(target / skill.marker, f'This skill is managed by {skill.cli}. Manual edits will be overwritten on upgrade.\n', skill)
    _write(target / SKILL_FILE, skill.body, skill)
    _write(target / VERSION_FILE, skill.version, skill)
    out = {'paths': [str(target / SKILL_FILE)], 'notices': []}

    claude = home / '.claude'
    if claude.is_dir() and not claude.is_symlink():
        link = skill.claude_link(home)
        if link.is_symlink() and os.readlink(link) == skill.claude_target:
            pass
        elif link.is_symlink() and not link.exists():  # dangling: replace
            link.unlink()
            link.symlink_to(skill.claude_target)
            out['paths'].append(str(link))
        elif link.is_symlink() or link.exists():
            out['notices'].append(f'Left {link} alone: it is not a link {skill.cli} made.')
        else:
            link.parent.mkdir(parents=True, exist_ok=True)
            link.symlink_to(skill.claude_target)
            out['paths'].append(str(link))
    return out


def remove(home: Path, skill: Skill) -> dict:
    out = {'paths': [], 'notices': []}
    link = skill.claude_link(home)
    if link.is_symlink() and os.readlink(link) == skill.claude_target:
        link.unlink()
        out['paths'].append(str(link))
    target = skill.dir(home)
    if skill.owns(target):
        for name in (SKILL_FILE, VERSION_FILE, skill.marker):
            (target / name).unlink(missing_ok=True)
        try:
            target.rmdir()
        except OSError:
            out['notices'].append(f'Left {target}: it holds files {skill.cli} did not write.')
        out['paths'].append(str(target))
    elif target.is_symlink() or target.exists():
        out['notices'].append(f'Left {target} alone: it is not a skill {skill.cli} installed.')
    return out


def refresh(home: Path, skill: Skill) -> bool:
    """Bring an owned copy up to date after an upgrade; silent and cheap."""
    target = skill.dir(home)
    if skill.legacy(target):
        try:
            install(home, skill)
            return True
        except (OSError, Unmanaged):
            return False
    try:
        if not skill.owns(target) or (target / VERSION_FILE).read_text() == skill.version:
            return False
    except FileNotFoundError:
        pass
    try:
        _write(target / SKILL_FILE, skill.body, skill)
        _write(target / VERSION_FILE, skill.version, skill)
        return True
    except (OSError, Unmanaged):
        return False
