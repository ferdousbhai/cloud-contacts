"""Native GTK4/libadwaita UI. All operations go through the CLI's service layer."""
from concurrent.futures import ThreadPoolExecutor
import shutil
import subprocess
from pathlib import Path
import gi
gi.require_version('Gtk', '4.0')
gi.require_version('Adw', '1')
from gi.repository import Adw, GLib, Gtk
from .common import Failure


class Window(Adw.ApplicationWindow):
    def __init__(self, app, service):
        super().__init__(application=app, title='Cloud Contacts' if service.product == 'cloud-contacts' else 'Cloud Tasks')
        self.service = service
        self.contacts = service.product == 'cloud-contacts'
        self.worker = ThreadPoolExecutor(max_workers=1)
        self.rows = []
        self.alive = True
        self.busy = False
        self.notice = ''
        self.set_default_size(880, 680)
        self.connect('close-request', self.closed)
        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.set_content(outer)
        header = Adw.HeaderBar()
        outer.append(header)
        for text, action in [('Refresh', self.refresh), ('New', self.new), ('Edit', self.edit),
                             ('Delete', self.delete), ('Details', lambda: self.details(self.selected()) if self.selected() else None), ('Accounts', self.accounts)]:
            button = Gtk.Button(label=text)
            button.connect('clicked', lambda _button, fn=action: fn())
            header.pack_start(button) if text in ['Refresh', 'New'] else header.pack_end(button)
        if self.contacts:
            toolbar = Gtk.Box(spacing=8, margin_start=12, margin_end=12, margin_top=8)
            for text, action in [('Merge selected identities', self.merge), ('Unmerge selected', self.unmerge),
                                 ('Find matching contacts', self.candidates)]:
                button = Gtk.Button(label=text)
                button.connect('clicked', lambda _button, fn=action: fn())
                toolbar.append(button)
            outer.append(toolbar)
        else:
            self.completed = Gtk.CheckButton(label='Include completed tasks', margin_start=12)
            self.completed.connect('toggled', lambda _: self.refresh())
            outer.append(self.completed)
        self.search = Gtk.SearchEntry(placeholder_text='Search contacts' if self.contacts else 'Search tasks',
                                     margin_start=12, margin_end=12, margin_top=8, margin_bottom=8)
        self.search.connect('search-changed', lambda _: self.render())
        outer.append(self.search)
        self.message = Gtk.Label(wrap=True, xalign=0, margin_start=12, margin_end=12, margin_bottom=8)
        outer.append(self.message)
        scroll = Gtk.ScrolledWindow(vexpand=True)
        self.listbox = Gtk.ListBox(selection_mode=Gtk.SelectionMode.MULTIPLE if self.contacts else Gtk.SelectionMode.SINGLE)
        self.listbox.set_activate_on_single_click(False)
        self.listbox.add_css_class('boxed-list')
        self.listbox.connect('row-activated', lambda _, row: self.details(row.data))
        scroll.set_child(self.listbox)
        outer.append(scroll)
        self.refresh()

    def closed(self, *_):
        self.alive = False
        self.worker.shutdown(wait=False, cancel_futures=True)
        return False

    def run(self, operation, done=None):
        if self.busy:
            return
        self.busy = True
        self.listbox.set_sensitive(False)
        self.message.set_text('Working…')
        future = self.worker.submit(operation)
        def finish():
            if not self.alive:
                return False
            self.busy = False
            self.listbox.set_sensitive(True)
            try:
                result = future.result()
                self.message.set_text('')
                if done:
                    done(result)
                else:
                    if isinstance(result, dict) and result.get('warning'):
                        self.notice = result['warning']
                    self.refresh()
            except Exception as error:
                if done is None:
                    self.render()
                self.message.set_text(str(error))
            return False
        future.add_done_callback(lambda _: GLib.idle_add(finish))

    def refresh(self):
        include_completed = False if self.contacts else self.completed.get_active()
        self.run(lambda: self.service.list() if self.contacts else self.service.list(include_completed=include_completed), self.loaded)

    def loaded(self, result):
        self.rows, warnings = result
        self.message.set_text(self.notice + '\n' + '\n'.join('Partial results: ' + warning.get('account', '') + ': ' + warning['message'] for warning in warnings))
        if not self.rows and not warnings and not self.notice:
            self.message.set_text('No contacts found.' if self.contacts else 'No tasks found.')
        self.render()

    def render(self):
        while (child := self.listbox.get_first_child()) is not None:
            self.listbox.remove(child)
        needle = self.search.get_text().casefold()
        for data in self.rows:
            name = data.get('name' if self.contacts else 'title', '')
            secondary = ', '.join(item['value'] for item in data.get('emails', [])) if self.contacts else data.get('list', '')
            if needle not in (name + ' ' + secondary).casefold():
                continue
            row = Gtk.ListBoxRow()
            row.data = data
            box = Gtk.Box(spacing=10, margin_start=12, margin_end=12, margin_top=12, margin_bottom=12)
            row.set_child(box)
            if not self.contacts:
                check = Gtk.CheckButton(active=data.get('completed', False), valign=Gtk.Align.CENTER)
                def toggle(widget, identity=data['id']):
                    completed = widget.get_active()
                    self.run(lambda: self.service.edit(identity, completion=completed))
                check.connect('toggled', toggle)
                box.append(check)
            labelbox = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, hexpand=True, spacing=4)
            labelbox.append(Gtk.Label(label=name, xalign=0, wrap=True))
            due = (data.get('due') or {}).get('date', '')
            secondary += (' · ' + due) if due else ''
            detail = Gtk.Label(label=secondary + '\n' + data['id'], xalign=0, wrap=True, selectable=True)
            detail.add_css_class('dim-label')
            labelbox.append(detail)
            box.append(labelbox)
            if data.get('conflicts') or data.get('missing_sources'):
                box.append(Gtk.Label(label='Source differences'))
            self.listbox.append(row)

    def selected(self, single=True):
        rows = self.listbox.get_selected_rows()
        if not rows or (single and len(rows) != 1):
            self.message.set_text('Select one record.' if single else 'Select contacts to merge.')
            return None
        return rows[0].data if single else [row.data for row in rows]

    def form(self, title, fields, action, help_text=''):
        dialog = Gtk.Window(title=title, transient_for=self, modal=True, default_width=520)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_start=18, margin_end=18, margin_top=18, margin_bottom=18)
        dialog.set_child(box)
        if help_text:
            box.append(Gtk.Label(label=help_text, wrap=True, xalign=0))
        entries = {}
        for key, label, default in fields:
            box.append(Gtk.Label(label=label, xalign=0))
            if isinstance(default, list):
                entry = Gtk.DropDown.new_from_strings([label for _, label in default])
                entry.values = [value for value, _ in default]
            else:
                entry = Gtk.Entry(text=default or '', hexpand=True)
            entries[key] = entry
            box.append(entry)
        feedback = Gtk.Label(wrap=True, xalign=0)
        box.append(feedback)
        save = Gtk.Button(label='Apply')
        save.add_css_class('suggested-action')
        def submit(_):
            values = {key: entry.values[entry.get_selected()] if isinstance(entry, Gtk.DropDown) else entry.get_text() for key, entry in entries.items()}
            try:
                operation = action(values)
                dialog.close()
                self.run(operation)
            except Exception as error:
                feedback.set_text(str(error))
        save.connect('clicked', submit)
        box.append(save)
        dialog.present()

    def new(self):
        if self.contacts:
            fields = [('account', 'Target account', [('', 'Choose an account')] + [(name, name + ' — ' + cfg['provider']) for name, cfg in self.service.accounts().items()]), ('first', 'First name', ''), ('last', 'Last name', ''),
                      ('email', 'Email', ''), ('phone', 'Phone', '')]
            self.form('New contact', fields, lambda v: lambda: self.service.write(account=v['account'], first=v['first'], last=v['last'],
                      emails=[v['email']] if v['email'] else [], phones=[v['phone']] if v['phone'] else []))
        else:
            self.run(lambda: self.service.lists(), self.new_task)

    def new_task(self, result):
        lists, warnings = result
        self.form('New task', [('list', 'Target list', [('', 'Choose a list')] + [(row['id'], row['name'] + ' — ' + row['account']) for row in lists]), ('title', 'Title', ''), ('notes', 'Notes', ''), ('due', 'Due date/time (Google: date only)', '')],
                  lambda v: lambda: self.service.add(v['title'], v['list'], v['notes'], v['due'] or None),
                  'Some accounts failed; check status.' if warnings else 'Choose the source list for this task.')

    def edit(self):
        data = self.selected()
        if not data:
            return
        if self.contacts:
            fields = [('source', 'Source account record', [('', 'Choose a source')] + [(row['id'], row.get('name', '') + ' — ' + row['account']) for row in data.get('sources', [data])]), ('first', 'First name', data.get('first', '')),
                      ('last', 'Last name', data.get('last', '')), ('email', 'Add email', ''), ('phone', 'Add phone', '')]
            def action(v):
                kwargs = {'source': v['source'] or None, 'emails': [v['email']] if v['email'] else [], 'phones': [v['phone']] if v['phone'] else []}
                for field in ['first', 'last']:
                    if v[field] != (data.get(field) or ''):
                        kwargs[field] = v[field]
                return lambda: self.service.write(data['id'], **kwargs)
            self.form('Edit contact', fields, action, 'Edits affect exactly one provider record. Open the contact to see source IDs.')
        else:
            due = data.get('due') or {}
            due_text = due.get('date', '') + ((' ' + due['time']) if due.get('time') else '')
            def action(v):
                kwargs = {}
                for field in ['title', 'notes']:
                    if v[field] != (data.get(field) or ''):
                        kwargs[field] = v[field]
                if v['due'] != due_text:
                    kwargs.update({'due': v['due']} if v['due'] else {'no_due': True})
                return lambda: self.service.edit(data['id'], **kwargs)
            self.form('Edit task', [('title', 'Title', data['title']), ('notes', 'Notes', data.get('notes', '')),
                      ('due', 'Due (empty clears; Google supports date only)', due_text)], action,
                      'Existing Apple alarms retain their own times when you change a due date.')

    def confirm(self, heading, body, action):
        dialog = Adw.AlertDialog(heading=heading, body=body)
        dialog.add_response('cancel', 'Cancel')
        dialog.add_response('apply', 'Apply')
        dialog.set_response_appearance('apply', Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_default_response('cancel')
        dialog.set_close_response('cancel')
        dialog.connect('response', lambda _, response: self.run(action) if response == 'apply' else None)
        dialog.present(self)

    def delete(self):
        data = self.selected()
        if not data:
            return
        if self.contacts and data['id'].startswith('person:'):
            self.message.set_text('Select a source record ID using the CLI to delete it; use Unmerge to split this identity.')
            return
        self.confirm('Delete from source account?', 'This affects the provider, not just this view.\n' + data['id'] +
                     ('\nAssigned Google tasks can also be removed from Docs/Chat.' if not self.contacts else ''),
                     lambda: self.service.delete(data['id'], yes=True))

    def merge(self):
        rows = self.selected(single=False)
        if not rows or len(rows) < 2:
            self.message.set_text('Select at least two contacts representing the same person.')
            return
        ids = [row['id'] for row in rows]
        self.confirm('These records identify the same person?', '\n'.join(row['name'] + ' — ' + row['id'] for row in rows) +
                     '\nCombines the local view; source records stay intact. The first record supplies the display name.',
                     lambda: self.service.merge(ids, yes=True))

    def unmerge(self):
        data = self.selected()
        if data:
            self.confirm('Split this merged identity?', 'Source records remain unchanged.', lambda: self.service.unmerge(data['id'], yes=True))

    def candidates(self):
        self.run(self.service.candidates, lambda result: self.details({'matching_contacts': result[0], 'warnings': result[1]}))

    def details(self, data):
        dialog = Gtk.Window(title='Details', transient_for=self, default_width=640, default_height=520)
        scroll = Gtk.ScrolledWindow()
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_start=18, margin_end=18, margin_top=18, margin_bottom=18)
        scroll.set_child(box)
        dialog.set_child(scroll)
        if 'matching_contacts' in data:
            lines = []
            for pair in data['matching_contacts']:
                lines.append(' / '.join(pair['names']))
                lines.extend(pair['sources'])
                lines.extend(item['kind'] + ': ' + item['value'] for item in pair['evidence'])
                lines.append('Confirm these identify the same person before merging.\n')
            body = '\n'.join(lines) or 'No exact email/international-phone matches found.'
        else:
            lines = [data.get('name', data.get('title', '')), data['id']]
            if self.contacts:
                lines.extend('Email: ' + item['value'] for item in data.get('emails', []))
                lines.extend('Phone: ' + item['value'] for item in data.get('phones', []))
                lines.extend('Source: ' + item['account'] + ' — ' + item['id'] for item in data.get('sources', [data]))
                for field, values in data.get('conflicts', {}).items():
                    lines.append('Different ' + field + ':')
                    lines.extend('  ' + value['source'] + ': ' + str(value['value']) for value in values)
                lines.extend('Unavailable source: ' + item for item in data.get('missing_sources', []))
            else:
                lines += ['List: ' + data.get('list', ''), data.get('notes', ''),
                          'Completed' if data.get('completed') else 'Open']
                if data.get('due'):
                    lines.append('Due: ' + str(data['due'].get('date', '')) + ' ' + str(data['due'].get('time') or ''))
            body = '\n'.join(lines)
        box.append(Gtk.Label(label=body, wrap=True, xalign=0, selectable=True))
        dialog.present()

    def login_account(self, name):
        terminal = shutil.which('xdg-terminal-exec')
        if not terminal:
            self.message.set_text('Run ' + self.service.product + ' account login ' + name + ' in your terminal.')
            return
        executable = Path(__file__).resolve().parent.parent / 'bin' / self.service.product
        subprocess.Popen([terminal, str(executable), '--config', str(self.service.path), 'account', 'login', name])
        self.message.set_text('Complete sign-in in the terminal/browser, then click Refresh.')

    def add_account(self):
        fields = [('provider', 'Provider', [('icloud', 'iCloud'), ('google', 'Google')]), ('name', 'Account name (optional)', ''),
                  ('gws', 'Google: existing gws config directory (optional)', '')]
        self.form('Link an account', fields, lambda v: lambda: self.service.add_account(v['provider'], v['name'] or None, v['gws'] or None),
                  'iCloud uses the shared Apple sign-in. Google needs a configured gws OAuth client; see README. Linking does not copy credentials.')

    def accounts(self):
        dialog = Gtk.Window(title='Accounts', transient_for=self, default_width=560, default_height=360)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_start=18, margin_end=18, margin_top=18, margin_bottom=18)
        dialog.set_child(box)
        for name, cfg in self.service.accounts().items():
            row = Gtk.Box(spacing=8)
            row.append(Gtk.Label(label=name + ' — ' + cfg['provider'], hexpand=True, xalign=0))
            login = Gtk.Button(label='Sign in')
            login.connect('clicked', lambda _, account=name: self.login_account(account))
            row.append(login)
            remove = Gtk.Button(label='Remove')
            def unlink(_button, account=name):
                dialog.close()
                self.confirm('Unlink this account?', 'Removes it from this app; provider records and credentials remain intact.',
                             lambda: self.service.remove_account(account))
            remove.connect('clicked', unlink)
            row.append(remove)
            box.append(row)
        button = Gtk.Button(label='Link account')
        button.connect('clicked', lambda _: (dialog.close(), self.add_account()))
        box.append(button)
        dialog.present()


def launch(service):
    app = Adw.Application(application_id='com.ferdousbhai.' + service.product.replace('-', ''))
    app.connect('activate', lambda instance: Window(instance, service).present())
    return app.run([])
