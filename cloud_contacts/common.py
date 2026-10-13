"""Process and account boundaries shared by the CLI and GTK app."""
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import uuid
from urllib.parse import quote, unquote


class Failure(Exception):
    def __init__(self, code, message, details=None):
        super().__init__(message)
        self.code, self.details = code, details


def read_json(path, default):
    try:
        with Path(path).open() as stream:
            return json.load(stream)
    except FileNotFoundError:
        return default
    except (OSError, ValueError) as error:
        raise Failure('invalid_config', f'Cannot read {path}: {error}') from error


def change_json(path, default, change):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (path.parent / (path.name + '.lock')).open('a') as lock:
        os.chmod(lock.name, 0o600)
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = read_json(path, default)
        result = change(data)
        fd, temporary = tempfile.mkstemp(dir=path.parent, prefix='.' + path.name)
        try:
            with os.fdopen(fd, 'w') as stream:
                json.dump(data, stream, indent=2, ensure_ascii=False)
                stream.write('\n')
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        return result


def array(value, label='provider result'):
    if not isinstance(value, list):
        raise Failure('bad_response', f'{label} is not an array')
    return value


def scoped_id(account, raw):
    return account + ':' + quote(str(raw), safe='')


def split_id(value):
    account, separator, raw = value.partition(':')
    if not separator or not raw:
        raise Failure('invalid_request', 'Use the complete account-prefixed ID returned by list')
    return account, unquote(raw)


class Context:
    def __init__(self, product, config=None):
        self.product = product
        self.env_prefix = product.upper().replace('-', '_')
        base = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home() / '.config')))
        self.path = Path(config or os.environ.get(self.env_prefix + '_CONFIG', base / product / 'config.json')).expanduser().resolve()

    def accounts(self):
        data = read_json(self.path, {'accounts': {}})
        if not isinstance(data, dict) or not isinstance(data.get('accounts'), dict):
            raise Failure('invalid_config', 'Config must contain an accounts object')
        for name, account in data['accounts'].items():
            if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_-]{0,63}', name) or name == 'person' or not isinstance(account, dict) or account.get('provider') not in ('icloud', 'google'):
                raise Failure('invalid_config', 'Invalid account name or provider')
            if account['provider'] == 'google' and not isinstance(account.get('gws_config'), str):
                raise Failure('invalid_config', 'Google accounts need a gws_config directory')
        return data['accounts']

    def account(self, name):
        accounts = self.accounts()
        if name not in accounts:
            raise Failure('not_configured', f'No account named {name}')
        return accounts[name]

    def add_account(self, provider, name=None, gws_config=None):
        if provider not in ('icloud', 'google'):
            raise Failure('invalid_request', 'Provider must be icloud or google')
        name = name or provider
        if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_-]{0,63}', name) or name == 'person':
            raise Failure('invalid_request', 'Use an account name of letters, digits, _ and - (not person)')
        if provider == 'icloud' and gws_config:
            raise Failure('invalid_request', '--gws-config is only for Google')
        def add(data):
            accounts = data.setdefault('accounts', {})
            if name in accounts:
                raise Failure('already_exists', f'Account {name} already exists')
            if provider == 'icloud' and any(a['provider'] == 'icloud' for a in accounts.values()):
                raise Failure('invalid_request', 'icloud-session supports one Apple account; it is already linked')
            account = {'provider': provider, 'identity': str(uuid.uuid4())}
            if provider == 'google':
                account['gws_config'] = str(Path(gws_config).expanduser().resolve() if gws_config else self.path.parent / 'gws' / name)
            accounts[name] = account
            return {'name': name, **account}
        return change_json(self.path, {'accounts': {}}, add)

    def remove_account(self, name):
        def remove(data):
            if name not in data['accounts']:
                raise Failure('not_found', f'No account named {name}')
            if data['accounts'][name]['provider'] == 'icloud':
                data['legacy_migrated'] = True
            del data['accounts'][name]
            return {'removed': name, 'credentials_deleted': False}
        return change_json(self.path, {'accounts': {}}, remove)

    def process(self, command, args, env=None, timeout=90, allow_empty=False):
        try:
            result = subprocess.run([command, *args], capture_output=True, text=True,
                                    env=env, timeout=timeout, check=False)
        except FileNotFoundError as error:
            raise Failure('missing_cli', f'{command} is not installed; see the product README') from error
        except subprocess.TimeoutExpired as error:
            raise Failure('timeout', 'Provider timed out; a write may have succeeded. Inspect before retrying.') from error
        if result.returncode:
            # Prefer structured error text; do not leak provider diagnostics or credentials.
            for line in reversed(result.stderr.splitlines()):
                try:
                    failure = json.loads(line).get('error', {})
                    if isinstance(failure, dict) and failure.get('message'):
                        raise Failure(failure.get('code', 'provider_error'), failure['message'])
                except (ValueError, AttributeError):
                    pass
            raise Failure('provider_error', f'{Path(command).name} exited {result.returncode}; check its sign-in/status in your terminal')
        if allow_empty and not result.stdout.strip():
            return {}
        try:
            return json.loads(result.stdout)
        except ValueError as error:
            raise Failure('bad_response', 'Provider returned invalid JSON') from error

    def icloud(self, args):
        tool = str(Path(__file__).resolve().parent.parent / 'libexec' / (self.product + '-icloud'))
        return self.process(os.environ.get(self.env_prefix + '_ICLOUD_COMMAND', tool), ['--json', *args])

    def google_env(self, name):
        account = self.account(name)
        env = os.environ.copy()
        for key in list(env):
            if key.startswith('GOOGLE_WORKSPACE_CLI_') or key in ('GOOGLE_APPLICATION_CREDENTIALS', 'GOOGLE_WORKSPACE_PROJECT_ID'):
                del env[key]
        env['GOOGLE_WORKSPACE_CLI_CONFIG_DIR'] = account['gws_config']
        env['GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND'] = 'file'
        return env

    def google(self, name, route, params=None, body=None):
        args = list(route)
        if params:
            args += ['--params', json.dumps(params)]
        if body is not None:
            args += ['--json', json.dumps(body)]
        return self.process(os.environ.get(self.env_prefix + '_GWS_COMMAND', 'gws'), args, self.google_env(name), allow_empty=route[-1] in ('delete', 'deleteContact'))

    def pages(self, name, route, params, field):
        rows, seen = [], set()
        params = dict(params)
        for _ in range(1000):
            data = self.google(name, route, params)
            if not isinstance(data, dict):
                raise Failure('bad_response', 'Google page is not an object')
            rows.extend(array(data.get(field, [])))
            token = data.get('nextPageToken')
            if not token:
                return rows
            if token in seen:
                raise Failure('bad_response', 'Google repeated a pagination token')
            seen.add(token)
            params['pageToken'] = token
        raise Failure('page_limit', 'Too many pages; no complete result is available')

    def migrate_legacy(self):
        """Link an existing Apple cache without moving files or contacting Apple."""
        if read_json(self.path, {}).get('legacy_migrated'):
            return {'linked': False, 'reason': 'migration already handled', 'cache_changed': False}
        legacy = 'icloud-contacts' if self.product == 'cloud-contacts' else 'icloud-reminders'
        base = Path(os.environ.get('XDG_DATA_HOME', str(Path.home() / '.local/share')))
        if not base.is_absolute():
            base = Path.home() / '.local/share'
        cache = base / legacy / 'cache.json'
        if not cache.exists():
            return {'linked': False, 'reason': 'no legacy cache', 'cache_changed': False}
        data = read_json(cache, {})
        if not isinstance(data, dict) or not data.get('account'):
            return {'linked': False, 'reason': 'no authenticated legacy cache', 'cache_changed': False}
        if not any(account['provider'] == 'icloud' for account in self.accounts().values()):
            name = 'icloud'
            if name in self.accounts():
                raise Failure('ambiguous', 'Account name icloud is already used by another provider')
            self.add_account('icloud')
        change_json(self.path, {'accounts': {}}, lambda state: state.update(legacy_migrated=True))
        return {'linked': True, 'cache': str(cache), 'cache_changed': False}

    def login(self, name):
        account = self.account(name)
        if account['provider'] == 'icloud':
            command = ['icloud-session', 'sign-in']
            env = None
        else:
            scope = 'contacts' if self.product == 'cloud-contacts' else 'tasks'
            command = [os.environ.get(self.env_prefix + '_GWS_COMMAND', 'gws'), 'auth', 'login',
                       '--scopes', 'https://www.googleapis.com/auth/' + scope]
            env = self.google_env(name)
        # Interactive auth belongs to the user's terminal, never JSON tool capture.
        if not os.isatty(0):
            raise Failure('interactive_required', f'Run {self.product} account login {name} in your terminal')
        returncode = subprocess.call(command, env=env)
        if returncode:
            raise Failure('auth_error', 'Sign-in failed; configure the account OAuth client as described in README')
        return {'account': name, 'signed_in': True}

    def collect(self, operation, account=None):
        accounts = self.accounts()
        if not accounts:
            raise Failure('not_configured', 'Link an account with account add icloud|google')
        selected = {account: self.account(account)} if account else accounts
        rows, warnings = [], []
        for name, cfg in selected.items():
            try:
                rows.extend(operation(name, cfg))
            except Failure as error:
                warnings.append({'account': name, 'code': error.code, 'message': str(error)})
        if len(warnings) == len(selected):
            raise Failure('accounts_unavailable', 'No selected account could be read', warnings)
        return rows, warnings


def resolve(rows, query, text_field):
    exact = [row for row in rows if row['id'] == query]
    if exact:
        return exact[0]
    matches = [row for row in rows if row.get(text_field, '').casefold() == query.casefold()]
    if not matches:
        matches = [row for row in rows if query.casefold() in row.get(text_field, '').casefold()]
    if len(matches) == 1:
        return matches[0]
    if not matches:
        raise Failure('not_found', f'No match for {query}')
    raise Failure('ambiguous', 'Choose an exact ID', [{'id': row['id'], text_field: row[text_field]} for row in matches])
