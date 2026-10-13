import argparse
import json
import sys
from .common import Failure
from .service import Contacts


class Parser(argparse.ArgumentParser):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.add_argument('--json', action='store_true', default=argparse.SUPPRESS, help='JSON envelope (also the default)')


def parser():
    p = Parser(prog='cloud-contacts', description='Unified iCloud and Google contacts with reversible identity merges')
    p.add_argument('--config', help='account config JSON (default XDG config directory)')
    p.add_argument('--app', action='store_true', help='open the native GTK app')
    p.add_argument('--version', action='version', version='cloud-contacts 0.1.0')
    sub = p.add_subparsers(dest='command')
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


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    args = parser().parse_args(argv)
    try:
        service = Contacts(args.config)
        if args.app or args.command is None:
            from .app import launch
            return launch(service)
        result = dispatch(service, args)
        warnings = []
        if isinstance(result, tuple):
            result, warnings = result
        print(json.dumps({'ok': True, 'data': result, 'meta': {'warnings': warnings}}, ensure_ascii=False))
        return 0
    except Failure as error:
        print(json.dumps({'ok': False, 'error': {'code': error.code, 'message': str(error), 'details': error.details}}, ensure_ascii=False))
        raise SystemExit(1)
    except (OSError, ValueError) as error:
        print(json.dumps({'ok': False, 'error': {'code': 'local_error', 'message': str(error)}}))
        raise SystemExit(1)
