# cloud-contacts

Your iCloud and Google contacts side by side on Omarchy/Arch: a GTK4/libadwaita window and an
agent-friendly CLI, in Rust. Each contact lives in one account and a change goes to that account
alone; accounts never sync with each other. Sibling of Cloud Calendar and Cloudmail.

This file is for working on the code. For *using* the CLI as an agent, read
`skills/cloud-contacts/SKILL.md` (embedded in the binary: `cloud-contacts skill [install]`).

## Code index

One crate, `crates/cloud-contacts`:

- `src/book.rs` — every linked account's contacts together (`Book`, `Entry`, `Shown`): IDs are
  `<account>:<the account's own id>`, reads fetch the accounts at once, and one that fails is a
  warning (`Warning`) beside the others unless all failed.
- `src/account.rs` — the `Provider` trait each kind of account implements (cache, sync, add,
  update, delete) and what they share.
- `src/icloud/` — iCloud: `api.rs` (icloud.com's contacts service through icloud-session; read
  its header before changing requests) and `mod.rs` (`ICloud`: sync, writes, the 412 retry).
- `src/google.rs` — Google through `gws` (People API), isolated in
  `~/.config/cloud-contacts/gws/<account>`; a person is read into iCloud's card shape with
  Google's record kept under `googlePerson`, and written back on top of it (`card_of`,
  `person_of`), so what this app does not show survives an edit.
- `src/model.rs` (the card and `Change`s), `src/draft.rs` (the edit dialog), `src/vcard.rs`,
  `src/store.rs` (one cache per account under `~/.local/share/cloud-contacts/<account>/`).
- `src/config.rs` — `~/.config/cloud-contacts/config.toml` and the first run (Cloud Contacts
  0.3's `config.json`, or the iCloud account when icloud-session is signed in);
  `src/accounts.rs` — link, sign in again, unlink, status.
- `src/cli.rs` — the command line; `src/main.rs` its GTK-free binary; `src/bin/app/` the window
  (`cloud-contacts-app`, feature `ui`). `src/agent_skill.rs` is a shared copy (SHARED_SUPPORT.md).
- `examples/fake_contacts.rs` — a fake iCloud server (`ICLOUD_SESSION_MOCK=1`); `tests/fake-gws`
  — a fake `gws` (`CLOUD_CONTACTS_GWS_COMMAND`, state in `FAKE_GWS_STATE`). `tests/cli.rs`
  (iCloud), `tests/google.rs` (both accounts), `tests/model.rs` (random edits, `--deep`).

Dependencies on icloud-for-omarchy (icloud-session, icloud-app, icloud-app-ui) are pinned by git
`rev` in the root `Cargo.toml`; bump it deliberately.

## Commands

```sh
bin/check            # shared support, rustfmt, clippy -D warnings, tests, installer tests; .githooks/pre-push runs it
bin/check --deep     # also the random-exploration suites
git config core.hooksPath .githooks   # once per clone (NO_CHECK=1 skips the hook)
cargo build --release                 # target/release/cloud-contacts and cloud-contacts-app
bin/build-package "$(uname -m)"       # full package build as a regular Arch user
sudo bin/arch-run bin/build-package "$(uname -m)" /output  # disposable native Arch chroot
```

Tests never touch a real account, keyring, bus or desktop: they run the built binary with a
temporary HOME and XDG directories (set `XDG_CONFIG_HOME` too: `icloud_app::testing::command`
does not), the fake iCloud server and the fake gws. Never launch the window on the owner's
desktop; use a Broadway display of your own (`gtk4-broadwayd :NN`, an unusual number).

## Conventions

- The CLI follows the shared product contract (`icloud_session::output`): text on a terminal,
  the JSON envelope when piped, `--json/--styled/--quiet/--ids-only/--count`; exit 2 usage,
  3 not signed in / not configured, 4 not found, 5 unavailable. Account failures beside others
  go to `meta.warnings`. Destructive commands take `--yes`. New commands go in the skill too.
- A new kind of account implements `Provider`, prefixes nothing itself (the book does) and is
  added to `book::open_account`, `config::PROVIDERS` and `accounts`.
- Google's OAuth client is not in the source (GitHub push protection refuses it): a release
  build takes it from `CLOUD_CONTACTS_GOOGLE_CLIENT_ID` / `_SECRET` in the build environment
  (`option_env!` in `google.rs`); without them gws signs in with the client it is set up with.
- Release: bump `version` in the root `Cargo.toml` and `pkgver` in the PKGBUILD together, tag
  `vX.Y.Z`, push the tag (CI builds both architectures), draft the GitHub release, then
  `bin/release X.Y.Z` (signs with the maintainer key, never CI). Do not release unless asked.
