# cloud-contacts

Your iCloud and Google contacts side by side on Omarchy/Arch: a GTK 4 window (omui's look and
widgets, themed by Omarchy) and an agent-friendly CLI, in Rust. Each contact lives in one account and a change goes to that account
alone; accounts are not copied into each other unless the person turns on mirroring. Sibling of Cloud Calendar and Cloudmail.

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
  `person_of`), so what this app does not show survives an edit. One Google sign-in serves
  Cloudmail, Cloud Calendar and Cloud Contacts: they share the OAuth client, the browser sign-in
  asks for all three apps' scopes (`SIGN_IN_SCOPES`, the same list in each app; a scope added to
  one is added to all three), and an account that isn't signed in first copies a working sign-in
  from the other two apps' gws directories (`Google::adopt`, from `accounts::sign_in`). It is a
  copy, never a shared directory. Several Google accounts are accounts of their own names
  (`--name`; the window uses `accounts::next_name`); each keeps its address in
  `gws/<name>/address`, a new one takes over an address not linked here yet, one signed in again
  only its own, and `add` refuses an address another account has.
- `src/model.rs` (the card and `Change`s), `src/draft.rs` (the edit dialog), `src/vcard.rs`,
  `src/store.rs` (one cache per account under `~/.local/share/cloud-contacts/<account>/`).
- `src/config.rs` — `~/.config/cloud-contacts/config.toml` and the first run (Cloud Contacts
  0.3's `config.json`, or the iCloud account when icloud-session is signed in);
  `src/accounts.rs` — link, sign in again, unlink, status.
- `src/mirror.rs` — mirroring iCloud and Google (opt-in): the shared fields
  (`Shared`), the three-way merge, pairing and duplicates, the crash-safe run (`Engine`, read its
  header first), the state in `<data>/mirror.json` (0.4.0–0.4.1's `keep-same.json` is moved on load), and what the window shows (`shown`).
  `packaging/systemd/cloud-contacts-mirror.{service,timer}` run it; `mirror on` enables the
  timer and disables 0.4.0–0.4.1's `cloud-contacts-keep-same.timer` (`CLOUD_CONTACTS_SYSTEMCTL` in tests). `tests/mirror.rs`: two in-memory accounts,
  every merge rule and guard, a run killed after each write, random sequences that must converge.
- `src/dedupe.rs` — duplicates: groups (likely/check) within an account and pairs across, the
  lossless merge (`merge_into`, also mirroring's first join of a pair), which card is kept,
  keep-apart memory (`<data>/apart.json`), and the merge order (`apply`: backup to
  `<data>/merged-backup/<date>.vcf`, then the kept card, then the deletions; read its header).
- `src/cli.rs` — the command line; `src/main.rs` its GTK-free binary, which runs the window for
  `cloud-contacts open` (or no command); `src/bin/app/` the window (`cloud-contacts-app`, feature `ui`,
  installed off `PATH` in `/usr/lib/cloud-contacts/`; `packaging/cloud-contacts-app` is the old command's launcher on `PATH`). `src/agent_skill.rs` is a shared copy (SHARED_SUPPORT.md).
- One name, `cloud-contacts`: the command, the package, `data/cloud-contacts.{desktop,svg}` and the window's
  Wayland app id (Hyprland's class, which `omui::application` gives every window from `NAME` in `src/bin/app/main.rs`). The D-Bus name stays
  `com.ferdousbhai.cloudcontacts` (`APP_ID`): GLib takes no name without a dot. `tests/cli.rs` holds them to it.
- `src/i18n/en.json` — every string the window shows, in Omarchy's language through omui (`tr`, `trn!`,
  `i18n::bind_*`; `Window::retranslate` writes again what the window wrote from its state); the app binary's
  `the_catalogs_match_the_sources` test keeps it in step. The library and the CLI stay English: what the window
  shows of the library's is said by the window (`draft::Problem`, `mirror::Line`), not as the library's English.
- `examples/fake_contacts.rs` — a fake iCloud server (`ICLOUD_SESSION_MOCK=1`); `tests/fake-gws`
  — a fake `gws` (`CLOUD_CONTACTS_GWS_COMMAND`, state in `FAKE_GWS_STATE`). `tests/cli.rs`
  (iCloud), `tests/google.rs` (both accounts), `tests/model.rs` (random edits, `--deep`).

Dependencies on icloud-for-omarchy (icloud-session, icloud-app, icloud-app-ui) and on
[omui](https://github.com/ferdousbhai/omui) (the window's look, theme, icons and widgets; no
libadwaita, which `tests/cli.rs` checks) are pinned by git `rev` in the root `Cargo.toml`; bump
them deliberately. omui's Strata and Lucide notices ship in `THIRD_PARTY_NOTICES.md`.

## Commands

```sh
bin/check            # shared support, rustfmt, clippy -D warnings, tests, app QA, installer tests; .githooks/pre-push runs it
bin/check --deep     # also the random-exploration suites
NO_APP=1 bin/check   # on a machine without GTK 4: the command line and the library alone (without it, no GTK 4 fails)
git config core.hooksPath .githooks   # once per clone (NO_CHECK=1 skips the hook)
cargo build --release                 # target/release/cloud-contacts and cloud-contacts-app
bin/build-package "$(uname -m)"       # full package build as a regular Arch user
sudo bin/arch-run bin/build-package "$(uname -m)" /output  # disposable native Arch chroot
```

Tests never touch a real account, keyring, bus or desktop: they run the built binary with a
temporary HOME and XDG directories (set `XDG_CONFIG_HOME` too: `icloud_app::testing::command`
does not), the fake iCloud server and the fake gws. Never launch the window on the owner's
desktop: `dev/preview.sh <out-dir> [theme-dir ...]` takes pictures of it on those fakes in a nested
Hyprland with a private HOME, XDG directories and D-Bus (its header lists the scenes).
`dev/qa.sh <cloud-contacts-app> dev/qa/*.jsonl` drives the window, built with feature `qa`, through
the scripts' flows on the same fakes with omui's scripted QA (its protocol is in omui's README), on a
private broadway display with no session bus; bin/check runs it. `.github/workflows/check.yml` runs bin/check on
every push, in an Arch container as a regular user. README.md's picture is `dev/qa/screenshot.jsonl`'s shot (its
header has the command), and its Keyboard table lists every shortcut, as the window's overlay (F1, Ctrl+?) does,
which `tests/cli.rs` holds both to. `dev/qa/language.jsonl` switches the language live with a stand-in Danish
(`dev/qa/da.json`, which omui's QA `language` command loads).

## Conventions

- The CLI follows the shared product contract (`icloud_session::output`): text on a terminal,
  the JSON envelope when piped, `--json/--styled/--quiet/--ids-only/--count`. Exit codes are
  Omarchy's, 0 or 1: a failure exits 1 with stdout empty and the error on stderr (`cli::report`;
  the JSON error object in machine modes, its stable `code` the kind). Account failures beside others
  go to `meta.warnings`. Destructive commands take `--yes`. New commands go in the skill too.
- A new kind of account implements `Provider`, prefixes nothing itself (the book does) and is
  added to `book::open_account`, `config::PROVIDERS` and `accounts`.
- Google's OAuth client is Cloud Calendar's Desktop-app client, in `google.rs` (its secret is
  public by Google's rules for installed apps; the People API is on in its project).
  `CLOUD_CONTACTS_GOOGLE_CLIENT_ID` / `_SECRET` at run time, both set, use another.
- Release: bump `version` in the root `Cargo.toml` and `pkgver` in the PKGBUILD together, tag
  `vX.Y.Z`, push the tag (CI builds both architectures), draft the GitHub release, then
  `bin/release X.Y.Z` (signs with the maintainer key, never CI). Do not release unless asked.
