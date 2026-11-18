# Cloud Contacts

Your iCloud and Google contacts side by side on [Omarchy](https://omarchy.org) (and any Arch
Linux): a GTK4/libadwaita window and a command line for scripts and AI agents. Search, call,
email, add, edit, delete and export as vCard, in either account.

Each contact lives in one account, and a change goes to that account alone. Your iCloud and
Google contacts are never copied into each other: the window shows them together, labelled, with
a filter for one account.

## Install

On Omarchy or Arch Linux (x86_64 or aarch64):

```sh
curl -fsSL https://ferdousbhai.com/cloud-contacts/install.sh | sudo bash
```

It trusts the package-signing key, adds the `[cloud-contacts]` pacman repository and installs the
package; updates then arrive with `omarchy update`. The package replaces `icloud-contacts`, iCloud
for Omarchy's Contacts app: your iCloud contacts appear here on the first run, through the same
iCloud sign-in.

## Accounts

```sh
cloud-contacts account add icloud    # through icloud-session, the iCloud sign-in your iCloud apps share
cloud-contacts account add google    # in your browser, through Google's Workspace CLI (gws)
cloud-contacts status                # what is linked, and signed in
```

The window's accounts menu does the same. iCloud needs `icloud-session` (from
[iCloud for Omarchy](https://github.com/ferdousbhai/icloud-for-omarchy)); Google needs `gws`:
`npm install -g @googleworkspace/cli`. Cloud Contacts keeps Google's sign-in in its own
directory, `~/.config/cloud-contacts/gws/<account>`, apart from any gws setup of yours. Unlinking
an account forgets it on this computer; its contacts stay in the account.

## Everyday use

- **Browse and search**: every contact, sorted by last name, each labelled with its account. The
  filter above the list shows one account.
- **Copy, call, email**: a click copies a phone number or address; emails and links open.
- **Add**: the new contact goes to the account you pick (the one the filter shows, to start).
- **Edit**: names, company, phones, emails, URLs, birthday and notes. What the window does not
  show (photos, social profiles, Google's labels and groups) stays as the account has it.
- **Delete**: from the account it is in, on every device. iCloud keeps no Recently Deleted for
  contacts; Google keeps them in its Trash for 30 days.

## Command line

```sh
cloud-contacts list [QUERY] [--account NAME]
cloud-contacts show CONTACT            # an ID (icloud:..., google:people/...), `me`, a name, an email or a phone
cloud-contacts add --account google --first Ann --last Lee --phone work=+1 555 0100
cloud-contacts edit CONTACT --job-title Manager --remove-email old@example.com
cloud-contacts delete CONTACT --yes
cloud-contacts export [CONTACT...] -o contacts.vcf
cloud-contacts refresh                 # fetch every account now
```

Text on a terminal, a JSON envelope when piped (`--json`, `--quiet`, `--ids-only`, `--count`).
An account that fails while the others answer is a warning (`meta.warnings`), not an error.
Exit codes: 2 usage or ambiguous, 3 not signed in or nothing linked, 4 not found, 5 an account
out of reach. `cloud-contacts --help` has the rest. For coding agents, `cloud-contacts skill
install` installs the agent skill.

## From source

```sh
cargo build --release        # needs gtk4 and libadwaita for the window
target/release/cloud-contacts --help
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
