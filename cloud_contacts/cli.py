import argparse
import json
import sys
from pathlib import Path
from . import __version__, agent_skill, cli_output
from .common import Failure
from . import recall_client
from .service import Contacts


Parser = cli_output.Parser


def parser():
    p = Parser(prog='cloud-contacts', description='Unified iCloud and Google contacts with reversible identity merges')
    p.add_argument('--config', help='account config JSON (default XDG config directory)')
    p.add_argument('--app', action='store_true', help='open the native GTK app')
    p.add_argument('--version', action='version', version='cloud-contacts 0.2.0')
    sub = p.add_subparsers(dest='command')
    skill = sub.add_parser('skill', help='print the agent skill; install/remove it for coding agents (~/.agents/skills, linked for Claude Code)')
    skill.add_argument('action', nargs='?', choices=['install', 'remove'])
    sub.add_parser('migrate', help='adopt an existing legacy Apple cache without moving data')
    sub.add_parser('status', help='check configured accounts with provider reads')
    account = sub.add_parser('account').add_subparsers(dest='action', required=True)
    account.add_parser('list', help='list configuration; no credential access')
    add = account.add_parser('add')
    add.add_argument('provider', choices=['icloud', 'google'])
    add.add_argument('--name')
    add.add_argument('--gws-config', help='explicit existing gws account directory, or omit for a product-owned directory')
    login = account.add_parser('login')
    login.add_argument('name')
    remove = account.add_parser('remove')
    remove.add_argument('name')
    remove.add_argument('--yes', action='store_true')
    listing = sub.add_parser('list', help='list/search the unified address book')
    listing.add_argument('--semantic', metavar='QUERY', help='rank current account/list records with local Recall; text fallback when unavailable')
    listing.add_argument('query', nargs='?', default='')
    listing.add_argument('--account')
    listing.add_argument('--sources', action='store_true')
    show = sub.add_parser('show')
    show.add_argument('id')
    sub.add_parser('candidates', help='identity suggestions based on exact email or international phone; never auto-merge')
    merge = sub.add_parser('merge', help='preview a confirmed identity merge; --yes applies it locally')
    merge.add_argument('ids', nargs='+')
    merge.add_argument('--primary')
    merge.add_argument('--yes', action='store_true')
    unmerge = sub.add_parser('unmerge')
    unmerge.add_argument('id')
    unmerge.add_argument('--yes', action='store_true')
    for verb in ['add', 'edit']:
        change = sub.add_parser(verb)
        if verb == 'edit':
            change.add_argument('id')
            change.add_argument('--source')
        else:
            change.add_argument('--account', required=True)
        change.add_argument('--first')
        change.add_argument('--last')
        change.add_argument('--email', action='append')
        change.add_argument('--phone', action='append')
    delete = sub.add_parser('delete')
    delete.add_argument('id')
    delete.add_argument('--source')
    delete.add_argument('--yes', action='store_true')
    return p


def dispatch(service, args):
    if args.command == 'list':
        if args.semantic:
            rows, warnings = service.list('', args.account, args.sources)
            ids = recall_client.rank('cloud-contacts', args.semantic, recall_client.records(rows))
            if ids is None:
                rows, warnings = service.list(args.semantic, args.account, args.sources)
                return rows, [*warnings, {'account': '', 'message': 'Recall unavailable or warming up; showing text matches.'}]
            by_id = {row['id']: row for row in rows}
            return [by_id[identity] for identity in ids], warnings
        return service.list(args.query, args.account, args.sources)
    if args.command == 'show':
        return service.show(args.id)
    if args.command == 'candidates':
        return service.candidates()
    if args.command == 'merge':
        return service.merge(args.ids, args.primary, args.yes)
    if args.command == 'unmerge':
        return service.unmerge(args.id, args.yes)
    if args.command == 'add':
        return service.write(account=args.account, first=args.first, last=args.last, emails=args.email, phones=args.phone)
    if args.command == 'edit':
        return service.write(args.id, first=args.first, last=args.last, emails=args.email, phones=args.phone, source=args.source)
    if args.command == 'delete':
        return service.delete(args.id, args.yes, args.source)
    if args.command == 'migrate':
        return service.migrate_legacy()
    if args.command == 'status':
        rows, warnings = service.list()
        return {'accounts': service.accounts(), 'visible_records': len(rows), 'healthy': not warnings}, warnings
    if args.command == 'account':
        if args.action == 'list':
            return service.accounts()
        if args.action == 'add':
            return service.add_account(args.provider, args.name, args.gws_config)
        if args.action == 'login':
            return service.login(args.name)
        if args.action == 'remove':
            if not args.yes:
                service.account(args.name)
                return {'remove': args.name, 'applied': False, 'credentials_deleted': False}
            return service.remove_account(args.name)
    raise Failure('invalid_request', 'Choose a command; see --help')


SKILL = agent_skill.Skill('cloud-contacts', 'cloud-contacts', __version__, Path(__file__).resolve().parent.parent / 'skills' / 'cloud-contacts' / 'SKILL.md')


def skill(action):
    if action is None:
        sys.stdout.write(SKILL.body)
        return None
    home = agent_skill.home()
    if home is None:
        raise Failure('local_error', 'no home directory to install the skill into')
    try:
        return agent_skill.install(home, SKILL) if action == 'install' else agent_skill.remove(home, SKILL)
    except agent_skill.Unmanaged as error:
        raise Failure('skill_unmanaged', str(error)) from None


def summary(args, data):
    """A one-line summary of a command's result, for the envelope and text output."""
    words = [w for w in (args.command, getattr(args, 'action', None)) if w]
    if isinstance(data, list):
        noun = 'lists' if args.command == 'lists' else 'contacts'
        return cli_output.summarize(data, 'candidates' if args.command == 'candidates' else noun)
    return ' '.join(words) if words else 'cloud-contacts'


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    args = parser().parse_args(argv)
    mode = cli_output.mode_of(args)
    home = agent_skill.home()
    if home is not None:
        agent_skill.refresh(home, SKILL)
    try:
        if args.command == 'skill':
            result = skill(args.action)
            if result is not None:
                cli_output.emit(result, mode, f'skill {args.action}')
            return 0
        service = Contacts(args.config)
        if args.app or args.command is None:
            from .app import launch
            return launch(service)
        result = dispatch(service, args)
        warnings = []
        if isinstance(result, tuple):
            result, warnings = result
        return cli_output.emit(result, mode, summary(args, result), warnings)
    except Failure as error:
        raise SystemExit(cli_output.fail(error.code, str(error), mode, details=error.details)) from None
    except (OSError, ValueError) as error:
        raise SystemExit(cli_output.fail('local_error', str(error), mode)) from None
