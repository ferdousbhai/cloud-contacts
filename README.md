# Cloud Contacts

Your iCloud and Google contacts side by side on [Omarchy](https://omarchy.org) (and any Arch
Linux): a GTK4/libadwaita window and a command line for scripts and AI agents. Search, call,
email, add, edit, delete and export as vCard, in either account.

Each contact lives in one account, and a change goes to that account alone: the window shows
both accounts together, labelled, with a filter for one. If you want, Cloud Contacts can also mirror
your contacts between iCloud and Google ([below](#mirroring-icloud-and-google)).

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

## Mirroring iCloud and Google

With an iCloud and a Google account both linked, you can mirror them: a contact
added, changed or deleted in one is added, changed or deleted in the other. It is off until you
turn it on: the window offers it at the top (Turn On… or Not Now) and has a switch, "Mirror
contacts between iCloud and Google", below the contacts, with how it stands under it. From the command
line:

```sh
cloud-contacts mirror          # on or off, how many are linked, the last run, any problem
cloud-contacts mirror on       # asks first; then a timer mirrors them every few minutes
cloud-contacts mirror run      # now
cloud-contacts mirror off      # both accounts keep their contacts as they are
```

- **Turning it on** joins contacts already in both accounts (a shared email or phone and a
  similar name) instead of copying them, and lists the doubtful ones for you to call one person
  or two (`mirror pair` / `mirror apart`, or in the window); neither is copied until you
  decide. Everything else is copied to the other account.
- **What crosses**: names, nickname, company, job title, department, birthday, notes, phones,
  emails, URLs and addresses. **What never crosses**: photos, Google's labels and groups, and the
  fields only iCloud has (social profiles, related names and the like); each account keeps its
  own, and nothing of them is removed.
- **Changed on both sides**: phones, emails, URLs and addresses are joined, without duplicates; a
  single field takes the later change (iCloud's when the times are unknown, with a warning).
- **Guards**: a run that would delete more than 10 contacts, or more than half the linked ones,
  stops and lists them until you confirm with `mirror run --yes`. When an account can't be
  read, a run changes nothing.
- **When**: the timer runs while this computer is on; changes made on your phone cross once a
  computer with Cloud Contacts is awake. Unlinking either account turns it off; both keep their
  contacts. Cloud Contacts 0.4.0 and 0.4.1 called this keep-same; that command still works for
  now, and its links carry over.

`cloud-contacts duplicates` lists contacts that may be one person (the same email, phone or
name), in one account or across both, and `cloud-contacts merge CONTACT CONTACT [--keep
ACCOUNT]` joins two into one and deletes the other (it asks first).

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
