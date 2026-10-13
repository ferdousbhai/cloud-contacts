"""Unified contacts with reversible, local identity merges and explicit provider writes."""
import re
import json
import uuid
from .common import Context, Failure, array, change_json, read_json, resolve, scoped_id, split_id

FIELDS = 'names,nicknames,emailAddresses,phoneNumbers,organizations,biographies,urls,addresses,birthdays,metadata'


def google_contact(name, row):
    raw_id = row.get('resourceName')
    if not raw_id:
        raise Failure('bad_response', 'Google contact has no resourceName')
    names = row.get('names') or [{}]
    return {'id': scoped_id(name, raw_id), 'account': name, 'provider': 'google',
            'name': names[0].get('displayName') or ' '.join(filter(None, [names[0].get('givenName'), names[0].get('familyName')])) or (row.get('emailAddresses') or [{}])[0].get('value') or raw_id,
            'first': names[0].get('givenName'), 'middle': names[0].get('middleName'), 'last': names[0].get('familyName'),
            'nickname': (row.get('nicknames') or [{}])[0].get('value'),
            'company': (row.get('organizations') or [{}])[0].get('name'),
            'job_title': (row.get('organizations') or [{}])[0].get('title'),
            'notes': '\n'.join(v.get('value', '') for v in row.get('biographies', [])),
            'birthday': (row.get('birthdays') or [{}])[0].get('date'),
            'urls': [{'label': v.get('type', 'other'), 'value': v['value']} for v in row.get('urls', []) if v.get('value')],
            'addresses': row.get('addresses', []),
            'emails': [{'label': v.get('type', 'other'), 'value': v['value']} for v in row.get('emailAddresses', []) if v.get('value')],
            'phones': [{'label': v.get('type', 'other'), 'value': v['value']} for v in row.get('phoneNumbers', []) if v.get('value')],
            'source': row}


def cloud_contact(name, row):
    if not row.get('id'):
        raise Failure('bad_response', 'iCloud contact has no ID')
    return {**row, 'id': scoped_id(name, row['id']), 'account': name, 'provider': 'icloud', 'source': row}


def phone_key(value):
    value = value.strip()
    if not value.startswith('+') or re.search(r'[A-Za-z]', value):
        return None
    digits = ''.join(c for c in value if c.isascii() and c.isdigit())
    return '+' + digits if 8 <= len(digits) <= 15 else None


def match_keys(contact):
    keys = {('email', row['value'].strip().casefold()) for row in contact.get('emails', []) if '@' in row['value']}
    keys.update(('phone', key) for row in contact.get('phones', []) if (key := phone_key(row['value'])))
    return keys


class Contacts(Context):
    def __init__(self, config=None):
        super().__init__('cloud-contacts', config)
        self.identities = self.path.parent / 'identities.json'

    def source_rows(self, account=None):
        def load(name, cfg):
            if cfg['provider'] == 'icloud':
                return [cloud_contact(name, row) for row in array(self.icloud(['list']))]
            return [google_contact(name, row) for row in self.pages(name, ['people', 'people', 'connections', 'list'],
                     {'resourceName': 'people/me', 'personFields': FIELDS, 'pageSize': 1000}, 'connections')]
        return self.collect(load, account)

    def list(self, query='', account=None, sources=False):
        rows, warnings = self.source_rows(account)
        if not sources:
            groups = read_json(self.identities, {'groups': {}})['groups']
            by_id = {row['id']: row for row in rows}
            accounts = self.accounts()
            grouped = set()
            merged = []
            for identity, group in groups.items():
                present = [by_id[item] for item in group['sources'] if item in by_id and
                           group.get('bindings', {}).get(item) == accounts.get(split_id(item)[0], {}).get('identity')]
                if not present:
                    continue
                grouped.update(item['id'] for item in present)
                primary = next((item for item in present if item['id'] == group['primary']), present[0])
                contact = {'id': identity, 'name': primary['name'], 'primary': group['primary'],
                           'sources': present, 'missing_sources': [item for item in group['sources'] if item not in {source['id'] for source in present}],
                           'emails': [], 'phones': [], 'urls': [], 'addresses': [], 'conflicts': {},
                           **{field: primary.get(field) for field in ['first', 'middle', 'last', 'nickname', 'company', 'notes', 'birthday']}}
                for field in ['emails', 'phones', 'urls', 'addresses']:
                    seen = set()
                    for item in present:
                        for value in item.get(field, []):
                            key = value['value'].strip().casefold() if field == 'emails' else (phone_key(value['value']) or value['value']) if field == 'phones' else json.dumps(value, sort_keys=True)
                            if key not in seen:
                                contact[field].append(value)
                                seen.add(key)
                for field in ['name', 'first', 'middle', 'last', 'nickname', 'company', 'notes', 'birthday']:
                    values = [{'source': item['id'], 'value': item.get(field)} for item in present if item.get(field)]
                    if len({json.dumps(item['value'], sort_keys=True) for item in values}) > 1:
                        contact['conflicts'][field] = values
                merged.append(contact)
            rows = [row for row in rows if row['id'] not in grouped] + merged
        if query:
            needle = query.casefold()
            rows = [row for row in rows if needle in row.get('name', '').casefold() or
                    any(needle in item['value'].casefold() for field in ['emails', 'phones'] for item in row.get(field, []))]
        return sorted(rows, key=lambda row: (row['name'].casefold(), row['id'])), warnings

    def show(self, query):
        rows, warnings = self.list()
        return resolve(rows, query, 'name'), warnings

    def candidates(self):
        rows, warnings = self.source_rows()
        indexed, evidence = {}, {}
        by_id = {row['id']: row for row in rows}
        for row in rows:
            for key in match_keys(row):
                for other in indexed.get(key, []):
                    pair = tuple(sorted([other, row['id']]))
                    evidence.setdefault(pair, set()).add(key)
                indexed.setdefault(key, []).append(row['id'])
        pairs = [{'sources': list(pair), 'names': [by_id[identity]['name'] for identity in pair],
                  'evidence': [{'kind': key, 'value': value} for key, value in sorted(shared)],
                  'requires_identity_confirmation': True} for pair, shared in sorted(evidence.items())]
        return pairs, warnings

    def merge(self, ids, primary=None, yes=False):
        rows, warnings = self.list()
        rows = rows + [source for row in rows for source in row.get('sources', [])]
        if warnings:
            raise Failure('incomplete_accounts', 'Resolve account failures before merging identities', warnings)
        by_id = {row['id']: row for row in rows}
        selected = []
        for identity in ids:
            if identity not in by_id:
                raise Failure('not_found', f'Unknown contact ID {identity}; use list or list --sources')
            row = by_id[identity]
            selected.extend(item['id'] for item in row.get('sources', [row]))
        selected = list(dict.fromkeys(selected))
        if len(selected) < 2:
            raise Failure('invalid_request', 'Choose at least two distinct source contacts')
        if primary and primary not in selected:
            raise Failure('invalid_request', '--primary must be a selected source contact ID')
        preferred = primary or selected[0]
        plan = {'sources': selected, 'primary': preferred, 'provider_records_changed': False, 'applied': yes}
        if not yes:
            return plan
        accounts = self.accounts()
        def apply(data):
            groups = data['groups']
            overlapping = [key for key, group in groups.items() if any(item in selected and
                           group.get('bindings', {}).get(item) == accounts[split_id(item)[0]].get('identity')
                           for item in group['sources'])]
            identity = overlapping[0] if overlapping else 'person:' + str(uuid.uuid4())
            all_sources = list(selected)
            bindings = {}
            for key in overlapping:
                previous = groups.pop(key)
                all_sources.extend(previous['sources'])
                bindings.update(previous.get('bindings', {}))
            bindings.update({item: accounts[split_id(item)[0]]['identity'] for item in selected})
            groups[identity] = {'sources': list(dict.fromkeys(all_sources)), 'primary': preferred, 'bindings': bindings}
            return {**plan, 'id': identity, 'sources': groups[identity]['sources']}
        return change_json(self.identities, {'groups': {}}, apply)

    def unmerge(self, identity, yes=False):
        def apply(data):
            if identity not in data['groups']:
                raise Failure('not_found', 'No merged contact with that ID')
            group = data['groups'][identity]
            if yes:
                del data['groups'][identity]
            return {'id': identity, 'sources': group['sources'], 'applied': yes, 'provider_records_changed': False}
        if not yes:
            return apply(read_json(self.identities, {'groups': {}}))
        return change_json(self.identities, {'groups': {}}, apply)

    def target(self, identity, source=None):
        if identity.startswith('person:'):
            group = read_json(self.identities, {'groups': {}})['groups'].get(identity)
            if not group:
                raise Failure('not_found', 'Unknown merged contact')
            if not source or source not in group['sources']:
                raise Failure('source_required', 'Editing a merged contact requires --source with one of its source IDs', group['sources'])
            account_name, _ = split_id(source)
            if group.get('bindings', {}).get(source) != self.account(account_name).get('identity'):
                raise Failure('stale_source', 'This source belonged to a previously linked account; select a current source record explicitly')
            identity = source
        elif source and source != identity:
            raise Failure('invalid_request', '--source differs from the selected record')
        name, raw = split_id(identity)
        return name, raw, self.account(name)

    def write(self, identity=None, account=None, first=None, last=None, emails=None, phones=None, source=None):
        fields = {'first': first, 'last': last, 'emails': emails or [], 'phones': phones or []}
        if all(value is None or value == [] for value in fields.values()):
            raise Failure('invalid_request', 'Provide a name, email, or phone change')
        if identity:
            name, raw, cfg = self.target(identity, source)
        else:
            if not account:
                raise Failure('account_required', 'Creating a contact requires --account')
            name, raw, cfg = account, None, self.account(account)
        if cfg['provider'] == 'icloud':
            args = ['edit', raw] if raw else ['add']
            for key in ['first', 'last']:
                if fields[key] is not None:
                    args += ['--' + key, fields[key]]
            for field, flag in [('emails', '--email'), ('phones', '--phone')]:
                for value in fields[field]:
                    args += [flag, value]
            result = self.icloud(args)
            if not isinstance(result.get('contact'), dict):
                raise Failure('bad_response', 'Write may have succeeded; inspect the contact before retrying')
            return {**result, 'contact': cloud_contact(name, result['contact'])}
        params = {'personFields': FIELDS}
        current = self.google(name, ['people', 'people', 'get'], {'resourceName': raw, 'personFields': FIELDS}) if raw else {}
        body, masks = {}, []
        if raw:
            body['metadata'] = current.get('metadata', {})
            if current.get('etag'):
                body['etag'] = current['etag']
            params['resourceName'] = raw
        if first is not None or last is not None:
            old = (current.get('names') or [{}])[0]
            body['names'] = [{'givenName': first if first is not None else old.get('givenName', ''),
                              'familyName': last if last is not None else old.get('familyName', '')}]
            masks.append('names')
        for values, field in [(emails, 'emailAddresses'), (phones, 'phoneNumbers')]:
            if values:
                body[field] = list(current.get(field, []))
                existing = {item.get('value') for item in body[field]}
                body[field].extend({'value': value} for value in values if value not in existing)
                masks.append(field)
        if raw:
            params['updatePersonFields'] = ','.join(masks)
        result = self.google(name, ['people', 'people', 'updateContact' if raw else 'createContact'], params, body)
        return {'action': 'edit' if raw else 'add', 'contact': google_contact(name, result)}

    def delete(self, identity, yes=False, source=None):
        name, raw, cfg = self.target(identity, source)
        if not yes:
            return {'source': scoped_id(name, raw), 'applied': False, 'warning': 'Deletion affects the source account; iCloud contacts have no Recently Deleted'}
        if cfg['provider'] == 'icloud':
            self.icloud(['delete', raw, '--yes'])
        else:
            self.google(name, ['people', 'people', 'deleteContact'], {'resourceName': raw})
        return {'source': scoped_id(name, raw), 'applied': True}
