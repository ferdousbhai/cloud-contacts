# Cloud Contacts

Your iCloud and Google contacts in one address book: a native GTK4/libadwaita
app and a JSON CLI for Ghost or any local agent. Ships its product-owned iCloud backend and Google Workspace `gws`; it owns neither an Apple
session nor a second Google API implementation.

## Install

Python 3.10+ runs the CLI without Python packages. Source installation also needs
Rust/Cargo to build the bundled private iCloud provider. The app additionally needs
PyGObject, GTK4, and libadwaita (Omarchy/Arch packages `python-gobject`, `gtk4`,
`libadwaita`). Install those dependencies with `omarchy pkg add` if missing.
Only `icloud-session` is needed for Apple sign-in; `gws` supplies Google access.
The retired icloud-contacts package is not a dependency.

```sh
git clone https://github.com/ferdousbhai/cloud-contacts.git
cd cloud-contacts
./install.sh
cloud-contacts --app
```

The source installer copies the app and launcher into `~/.local`, including a
native application-menu entry. Make sure `~/.local/bin` is on PATH. An optional
first argument sets another prefix; no sudo is needed for a user-owned prefix.
Re-run the installer after updating the checkout. `packaging/PKGBUILD` builds a
local Arch package; there is no published signed package repository yet.

## Accounts

```sh
cloud-contacts account add icloud
cloud-contacts account add google --name work
cloud-contacts account list
```

iCloud uses the one Apple account already held by `icloud-session`. If needed,
run `icloud-session sign-in` yourself. Install its CLI/apps from
[icloud-for-omarchy](https://github.com/ferdousbhai/icloud-for-omarchy).

Install Google Workspace CLI with `npm install -g @googleworkspace/cli` or its
[release binary](https://github.com/googleworkspace/cli). Each Google account
gets its own directory under `~/.config/cloud-contacts/gws/<name>`. For a new
account, configure an OAuth client there using `gws`'s documented setup; e.g.
in your own terminal (the automated setup needs `gcloud`):

```sh
GOOGLE_WORKSPACE_CLI_CONFIG_DIR="$HOME/.config/cloud-contacts/gws/work" gws auth setup
cloud-contacts account login work
```

Login requests only the Contacts scope. The People API must be enabled for the
OAuth client's Google Cloud project. Manual OAuth setup is supported by `gws`;
never paste passwords, callback URLs, or tokens into an agent conversation.
Alternatively explicitly use an already configured Google account directory:

```sh
cloud-contacts account add google --name personal --gws-config ~/.config/gws
```

Only explicit `--gws-config` opts into sharing an existing credential context.
Ambient access-token/credential-file overrides are removed for provider calls;
Cloudmail's and Cloud Calendar's sign-ins are not read or expanded. `gws` owns
its encrypted credential files and its file-backend encryption key.

The native Accounts window links accounts, opens terminal sign-in, and unlinks
accounts. Removing an account from this app keeps its provider data and
credentials. In the CLI, `account remove NAME` previews and `--yes` applies it.

## Read and identify

```sh
cloud-contacts list
cloud-contacts list 'Alex'
cloud-contacts list --account work
cloud-contacts list --sources
cloud-contacts show 'Alex Smith'
cloud-contacts candidates
```

Every command emits `{ok, data, meta: {warnings}}`; `--json` is accepted anywhere
options are accepted. A provider failure preserves other accounts' results and
appears in warnings. If every selected account fails, the command fails instead
of claiming the address book is empty. There is no automatic background sync.
Reads use the providers' normal refresh behavior, including their local caches.

IDs are opaque, account-prefixed strings; copy them from output. Names used for
`show` must identify exactly one contact. Candidates require matching email or
fully qualified international phone numbers, never a name or phone suffix alone.
A shared email/phone is evidence, not proof; confirm the person before merging.

## Merge confirmed identities

```sh
cloud-contacts merge 'icloud:<id>' 'work:<id>'
cloud-contacts merge 'icloud:<id>' 'work:<id>' --yes
cloud-contacts unmerge 'person:<id>' --yes
```

Replace placeholders with returned IDs. Without `--yes`, merge/unmerge previews
without changing files. Merges create a stable `person:` identity in the local
unified address book. Emails, phones, URLs, and addresses are combined; every
source record is retained with its provenance, including fields this version
doesn't edit. Different names/notes/etc. remain visible as conflicts. The first
selected source supplies the display name; `--primary SOURCE_ID` chooses another.

The source services are not rewritten or deduplicated. Unmerge restores their
separate presentation. An unavailable or deleted source is marked missing,
never silently discarded. Reusing an unlinked account name does not reattach
its old identity bindings to the new account. Identity groups live in `identities.json` beside the
account config, written atomically under a file lock. Back it up to preserve
merges across reinstalls/machines.

## Write to a source

```sh
cloud-contacts add --account work --first Alex --last Smith --email 'alex@example.com'
cloud-contacts edit 'work:<id>' --phone '+14155550100'
cloud-contacts edit 'person:<id>' --source 'work:<id>' --first Alexander
cloud-contacts delete 'work:<id>'
cloud-contacts delete 'work:<id>' --yes
```

Edits target exactly one source, including for merged contacts. Names can be
changed/cleared; emails and phones are added without replacing existing values.
Google edits fetch current contact-source metadata/etags and update only named
fields, preserving unrelated data. Conflicts are returned instead of retried.
iCloud writes use the bundled backend's concurrency/field-preservation rules.
Deletion affects that provider (iCloud contacts have no Recently Deleted).
Unmerge is the way to split the local identity without deleting provider data.

The desktop app supports search, details/provenance, creation, edits, deletion,
matching suggestions, multi-select merge, unmerge, and account management. Native
writes use exactly the CLI's service layer and never automatically follow merge
suggestions. Double-click a row or choose Details to inspect it.

## First-version limits

Linux only; a CLI-first application core plus GTK frontend. No cross-provider
replication, automatic identity merges, provider-side deduplication, Google
organization directory/Other Contacts, or contact photo editing. Google OAuth
client setup is delegated to `gws`, not bundled app credentials. Live-account
behavior still needs an owner-authorized integration pass; automated tests use
fake providers exclusively.

## Agent skill

[skills/cloud-contacts/SKILL.md](skills/cloud-contacts/SKILL.md) teaches AI agents to install,
link, and use cloud-contacts. It ships with the CLI:

```sh
cloud-contacts skill install   # ~/.agents/skills/cloud-contacts, linked into ~/.claude/skills
cloud-contacts skill remove
```

A new release refreshes the installed copy automatically; only a copy cloud-contacts
wrote (marked `.managed-by-cloud-contacts`) is ever changed.

## Develop / remove

```sh
bin/check                              # every check; .githooks/pre-push runs it before a push
git config core.hooksPath .githooks    # once per clone
```

The native test uses a private Broadway display, private D-Bus, temporary HOME,
and fake data, never the owner's desktop or accounts. No runtime dependencies
beyond Python for CLI operation; no separate HTTP service or database.

To remove a source install, remove its launcher at `~/.local/bin/cloud-contacts`,
its menu entry under `~/.local/share/applications/`, and its application directory
`~/.local/share/cloud-contacts`. Account configuration/identity merges under
`~/.config/cloud-contacts` are intentionally retained.

MIT licensed.

## Replacing the legacy app

The legacy standalone app is retired. This product ships the extracted Rust
backend privately under its application directory, with protocol/fixture tests;
there is no subprocess dependency on the legacy CLI or its package. The shared
`icloud-session` service remains responsible for Apple sign-in.

Run `cloud-contacts migrate` to adopt an existing Apple cache without contacting
Apple or copying/moving files. The first native-app launch also adopts it.
Legacy cache paths are intentionally retained so account identity, sync tokens,
and existing records remain valid. Unlinking the account records that migration
is handled, so a later run does not silently relink it.

Arch packaging declares the legacy package replaced/conflicting. A source
installation is not a pacman transaction: after the new provider is built and
installed, remove the legacy package with the package manager. No provider data
or cache directories should be deleted during retirement.
