---
name: cloud-contacts
description: Search, add, edit, delete, export and merge the owner's iCloud and Google contacts, and keep the two the same when asked, through the cloud-contacts CLI using Bash; install it or link an account when needed. Use for contacts and address-book tasks.
---

# Cloud Contacts

`cloud-contacts` shows the iCloud and Google accounts linked on this computer
side by side. Each contact lives in one account, its ID says which
(`icloud:2B7E…`, `google:people/c123`), and a change goes to that account
alone (unless keep-same is on, below). Use it rather than `gws people`
or icloud.com. [Product docs](https://github.com/ferdousbhai/cloud-contacts).

## Install and accounts

If `command -v cloud-contacts` finds nothing, install it (asks for sudo):

```bash
curl -fsSL https://ferdousbhai.com/cloud-contacts/install.sh | sudo bash
```

```bash
cloud-contacts status                  # linked accounts, signed in or not (no network)
cloud-contacts account add icloud      # iCloud: icloud-session's sign-in (a window a person finishes)
cloud-contacts account add google      # Google: a browser sign-in a person finishes; needs `gws`
cloud-contacts account login NAME      # sign in to an account again
cloud-contacts account remove NAME --yes
```

On a first run with nothing linked, the iCloud account is linked by itself
when icloud-session is signed in (people coming from iCloud for Omarchy's
Contacts app). Signing in is a person's step: say what to run, don't wait on
a browser yourself. Google needs Google's Workspace CLI:
`npm install -g @googleworkspace/cli`.

## Reading

```bash
cloud-contacts list [QUERY] [--account NAME]   # sorted by last name; QUERY: name, company, email, phone digits
cloud-contacts show CONTACT                    # CONTACT: ID, `me`, name, email or phone number
cloud-contacts export [CONTACT...] [--account NAME] [-o FILE]   # vCard 3.0
cloud-contacts refresh                         # fetch every account now
```

Reads fetch every account first; `--cached` reads the last fetch instead.
A name in two accounts is ambiguous (exit 2): the hint lists the IDs; use one.

## Changing

```bash
cloud-contacts add --account NAME --first Ann --last Lee --phone work=+1 555 0100 --email ann@example.com
cloud-contacts edit CONTACT --job-title Manager --nickname "" --remove-phone 555-0199
cloud-contacts delete CONTACT --yes
```

`--account` is needed for `add` when more than one account is linked. An
empty value removes a field. An edit keeps everything it does not name,
including what this tool does not show (photos, Google's groups). iCloud
keeps no Recently Deleted for contacts; Google keeps its Trash 30 days.
Delete only what the person asked to delete.

## Keeping iCloud and Google the same

Only for people with both an iCloud and a Google account linked (otherwise
`keep-same` fails with `needs_both_accounts`, exit 3). Off until turned on.

```bash
cloud-contacts keep-same            # status: on, linked, last_run, problem, waiting
cloud-contacts keep-same on --yes   # only when the person asked to keep them the same
cloud-contacts keep-same run        # make them the same now
cloud-contacts keep-same pair ICLOUD_ID GOOGLE_ID   # a waiting pair is one person
cloud-contacts keep-same apart ICLOUD_ID GOOGLE_ID  # ... or two
cloud-contacts duplicates           # possible duplicates, within and across accounts
cloud-contacts merge CONTACT CONTACT [--keep ACCOUNT] --yes
```

Check `cloud-contacts status` (its `keep_same`) or `keep-same` when asked whether
it is on. Turn it on, call pairs, or merge only when the person asks. A run
stopped at `too_many_deletions` (exit 2) lists what it would delete: show the
person, and run `keep-same run --yes` only if they confirm. Photos, Google's
groups and iCloud-only fields never cross.

## Output and errors

Piped output is a JSON envelope `{ok, data, summary, meta?}`; `--quiet` is
the data alone, `--ids-only` the IDs, `--count` the number. An account that
failed while others answered is in `meta.warnings` (the command still
succeeds). Failures: `{ok:false, error:{code, message, hint?}}`, exit 2
usage/ambiguous, 3 `not_configured`, `sign_in_required` or
`account_unauthorized` (the hint says how to sign in), 4 `not_found`,
5 `account_unavailable`, `network` and the like.
