"""The command-line contract every product CLI follows (the same file in each Python product).

Output: human-readable text on a terminal, a JSON envelope when piped. ``--json`` forces the
envelope, ``--styled`` forces text, ``--quiet`` prints only ``data``, ``--ids-only`` one ID per
line, ``--count`` the number of results.

Envelope: ``{"ok": true, "data": ..., "summary": "...", "meta": {"warnings": [...]}}``; on failure
``{"ok": false, "error": {"code", "message", "hint"?, "details"?}}`` on stdout (text mode: ``error:``
lines on stderr).

Exit codes: 0 success, 1 other failure, 2 invalid usage or request, 3 not configured or not signed
in, 4 not found, 5 a service or required tool unavailable.
"""
from __future__ import annotations

import argparse
import json
import sys

OK, GENERIC, USAGE, AUTH, NOT_FOUND, UNAVAILABLE = 0, 1, 2, 3, 4, 5

# Error codes by exit status; anything unlisted is GENERIC.
EXIT_FOR_CODE = {
    **dict.fromkeys(('usage', 'invalid_request', 'ambiguous', 'source_required', 'account_required',
                     'interactive_required', 'already_exists', 'stale_source', 'confirmation_required'), USAGE),
    **dict.fromkeys(('not_configured', 'invalid_config', 'auth_error', 'sign_in_required',
                     'incomplete_accounts'), AUTH),
    'not_found': NOT_FOUND,
    **dict.fromkeys(('bad_response', 'timeout', 'provider_error', 'missing_cli', 'accounts_unavailable',
                     'unavailable', 'offline'), UNAVAILABLE),
}

MODES = ('json', 'quiet', 'ids_only', 'count', 'styled')


def add_output_flags(parser: argparse.ArgumentParser) -> None:
    """The output flags, accepted before or after the subcommand."""
    group = parser.add_argument_group('output')
    group.add_argument('--json', action='store_true', default=argparse.SUPPRESS,
                       help='JSON envelope (the default when piped)')
    group.add_argument('--quiet', action='store_true', default=argparse.SUPPRESS, help='only the result data, as JSON')
    group.add_argument('--ids-only', action='store_true', default=argparse.SUPPRESS, help='only IDs, one per line')
    group.add_argument('--count', action='store_true', default=argparse.SUPPRESS, help='only the number of results')
    group.add_argument('--styled', action='store_true', default=argparse.SUPPRESS,
                       help='human-readable text even when piped')


def mode_of(args: argparse.Namespace | None, argv: list[str] | None = None) -> str:
    """The output mode chosen by flags (from parsed args, or raw argv when parsing failed)."""
    chosen = [m for m in MODES if getattr(args, m, False)] if args is not None else \
        [m for m in MODES if '--' + m.replace('_', '-') in (argv or [])]
    if chosen:
        return chosen[0]
    return 'styled' if sys.stdout.isatty() else 'json'


def _ids(data) -> list[str]:
    if isinstance(data, list):
        return [str(item['id']) for item in data if isinstance(item, dict) and 'id' in item]
    if isinstance(data, dict) and 'id' in data:
        return [str(data['id'])]
    return []


def _text(value, indent: int = 0) -> list[str]:
    pad = '  ' * indent
    if isinstance(value, dict):
        lines = []
        for key, item in value.items():
            if isinstance(item, (dict, list)) and item:
                lines.append(f'{pad}{key}:')
                lines.extend(_text(item, indent + 1))
            else:
                lines.append(f'{pad}{key}: {_scalar(item)}')
        return lines
    if isinstance(value, list):
        lines = []
        for item in value:
            if isinstance(item, dict):
                head = item.get('name') or item.get('title') or item.get('id')
                lines.append(f'{pad}- {head}' if head is not None else f'{pad}-')
                rest = {k: v for k, v in item.items() if v not in (None, '', [], {}) and v != head}
                lines.extend(_text(rest, indent + 2))
            else:
                lines.append(f'{pad}- {_scalar(item)}')
        return lines
    return [f'{pad}{_scalar(value)}']


def _scalar(value) -> str:
    if value is None:
        return '-'
    if isinstance(value, bool):
        return 'yes' if value else 'no'
    if isinstance(value, (dict, list)):
        return 'none'
    return str(value)


def summarize(data, noun: str) -> str:
    if isinstance(data, list):
        return f'{len(data)} {noun}'
    return noun


def emit(data, mode: str, summary: str, warnings: list | None = None) -> int:
    warnings = warnings or []
    out = sys.stdout
    if mode == 'json':
        out.write(json.dumps({'ok': True, 'data': data, 'summary': summary, 'meta': {'warnings': warnings}},
                             ensure_ascii=False) + '\n')
    elif mode == 'quiet':
        out.write(json.dumps(data, ensure_ascii=False) + '\n')
    elif mode == 'ids_only':
        out.write(''.join(i + '\n' for i in _ids(data)))
    elif mode == 'count':
        out.write(f'{len(data) if isinstance(data, list) else len(_ids(data))}\n')
    else:
        out.write(summary + '\n')
        body = [] if data in (None, [], {}) else _text(data)
        if body:
            out.write('\n'.join(body) + '\n')
    if mode != 'json':
        for warning in warnings:
            message = warning.get('message', warning) if isinstance(warning, dict) else warning
            account = warning.get('account') if isinstance(warning, dict) else None
            sys.stderr.write(f'warning: {account + ": " if account else ""}{message}\n')
    return OK


def fail(code: str, message: str, mode: str, hint: str | None = None, details=None) -> int:
    """Print an error in the contract's shape and return its exit status."""
    status = EXIT_FOR_CODE.get(code, GENERIC)
    if mode == 'styled':
        sys.stderr.write(f'error: {message}\n' + (f'hint: {hint}\n' if hint else ''))
    else:
        error = {'code': code, 'message': message}
        if hint:
            error['hint'] = hint
        if details is not None:
            error['details'] = details
        sys.stdout.write(json.dumps({'ok': False, 'error': error}, ensure_ascii=False) + '\n')
    return status


class Parser(argparse.ArgumentParser):
    """An ArgumentParser whose usage errors follow the contract (code ``usage``, exit 2)."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        add_output_flags(self)

    def error(self, message):
        argv = sys.argv[1:]
        mode = mode_of(None, argv)
        if mode == 'styled':
            self.print_usage(sys.stderr)
        raise SystemExit(fail('usage', message, mode, hint=f'run `{self.prog} --help`'))
