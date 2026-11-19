//! The command line: `cloud-contacts <command>` reads and changes every
//! linked account's contacts without GTK, through the same [`Book`] the
//! window uses. Output follows the shared product contract
//! ([`icloud_session::output`]): text on a terminal, the JSON envelope when
//! piped, `--json`/`--styled`/`--quiet`/`--ids-only`/`--count` to choose.
//!
//! Exit codes: 0 ok; 1 error; 2 usage, an ambiguous name, or `delete`
//! without `--yes` when nobody can be asked; 3 not signed in (iCloud's
//! `sign_in_required`/`session_rejected`, Google's `account_unauthorized`),
//! or no account linked (`not_configured`); 4 not found; 5 an account out of
//! reach (offline, `icloud-sessiond` or `gws` missing or failing, the
//! keyring locked, a server error). The JSON `code` tells them apart. An
//! account that fails while others answer is a warning (`meta.warnings`, and
//! `warning:` lines on stderr in text mode), not an error.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use icloud_app::cli::{confirm, no_data_dir, other, session_failure, usage, warn, warning};
use icloud_session::cli::{EXIT_ERROR, EXIT_NOT_FOUND, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{self, CliError, Mode, OutputFlags, Response};
use serde_json::{Value, json};

use crate::accounts;
use crate::agent_skill;
use crate::book::{Book, Entry, Shown};
use crate::config;
use crate::dedupe;
use crate::error::{Error, Kind};
use crate::icloud::api::SessionTransport;
use crate::mirror;
use crate::model::{self, Change, Field, Multi};
use crate::store::Store;
use crate::vcard;

const AFTER_HELP: &str = "\
CONTACT is a contact's ID (icloud:..., google:people/...), `me` (your own
iCloud card), its exact name (ignoring case), a unique part of it, an email
address, or a phone number (matched by its digits). An ambiguous CONTACT
lists the matches; use the ID then.

PHONE, EMAIL and URL take an optional label first: work=+1 555 0100,
home=ann@example.com, WhatsApp=+62 812. Without one, a phone is mobile, an
email home and a URL homepage.

Each account keeps its own contacts: a change goes to the account the
contact is in, and nothing is copied between accounts. Reading commands
fetch every account first; --cached reads what the last fetch left instead.
An account that fails is a warning beside the others' contacts.

Output: human-readable text on a terminal, a JSON envelope
{\"ok\":true,\"data\",\"summary\",\"meta\"?} when piped (meta.warnings: the
accounts that failed). --json forces the envelope, --styled text, --quiet
prints only the data, --ids-only the contact ids, --count the number of
results. Flags combine: ids-only > count > quiet > json > styled. A failure
is {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (cancelled, error, bad_request, http 4xx)
  2 usage, ambiguous, or delete without --yes when nobody can be asked
  3 not_configured (no account linked), sign_in_required or
    session_rejected (iCloud: icloud-session sign-in), account_unauthorized
    (Google: cloud-contacts account login NAME)
  4 not_found
  5 offline, network, session_service (icloud-sessiond unreachable),
    keyring_unavailable, bad_response, account_unavailable (gws missing or
    Google unreachable), http 5xx";

#[derive(Parser)]
#[command(
    name = "cloud-contacts",
    version,
    about = "Your iCloud and Google contacts from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    #[command(flatten)]
    out: OutputFlags,
    /// Keep the accounts' caches in DIR (default ~/.local/share/cloud-contacts).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Read the caches without fetching first.
    #[arg(long, global = true)]
    cached: bool,
    #[command(subcommand)]
    command: Command,
}

/// The fields `add` and `edit` set. An empty value removes the field.
#[derive(clap::Args)]
struct Fields {
    #[arg(long, value_name = "NAME")]
    first: Option<String>,
    #[arg(long, value_name = "NAME")]
    middle: Option<String>,
    #[arg(long, value_name = "NAME")]
    last: Option<String>,
    #[arg(long, value_name = "NAME")]
    nickname: Option<String>,
    #[arg(long, value_name = "NAME")]
    company: Option<String>,
    #[arg(long, value_name = "TEXT")]
    job_title: Option<String>,
    #[arg(long, value_name = "TEXT")]
    department: Option<String>,
    /// YYYY-MM-DD, or MM-DD without a year.
    #[arg(long, value_name = "DATE")]
    birthday: Option<String>,
    #[arg(long, value_name = "TEXT")]
    notes: Option<String>,
    /// Add a phone number (repeatable).
    #[arg(long = "phone", value_name = "PHONE")]
    phones: Vec<String>,
    /// Add an email address (repeatable).
    #[arg(long = "email", value_name = "EMAIL")]
    emails: Vec<String>,
    /// Add a URL (repeatable).
    #[arg(long = "url", value_name = "URL")]
    urls: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// The linked accounts and whether each is signed in (no network).
    ///
    /// JSON: {accounts: [{name, provider, label, signed_in, detail}]}
    #[command(after_help = AFTER_HELP)]
    Status,
    /// Mirror contacts between iCloud and Google: a contact added, changed or deleted in one is in the other too (needs an iCloud and a Google account)
    ///
    /// Off until turned on. With no action, says how it stands. `on` first
    /// pairs the contacts already in both accounts (a shared email or phone
    /// and alike names), lists the doubtful ones for you (`pair` or `apart`;
    /// neither is copied meanwhile) and copies the rest; it asks first (type
    /// y), or needs --yes. Then a timer runs it every few minutes, or `run`
    /// now. A run that would delete more than 10 contacts, or more than half
    /// the linked ones, stops (too_many_deletions) until `run --yes`. Only
    /// fields both accounts have cross; photos, Google's groups and iCloud's
    /// other fields stay where they are.
    ///
    /// JSON: {on, linked, last_run, problem, warnings, waiting: [{icloud, google, why}]}
    #[command(after_help = AFTER_HELP, alias = "keep-same")]
    Mirror {
        #[command(subcommand)]
        action: Option<MirrorAction>,
    },
    /// Contacts that may be one person, in groups: those likely the same (`merge --all` merges them), those to check, and pairs across accounts.
    ///
    /// Likely: the same name and a shared phone or email; a shared phone or
    /// email and one name inside the other; or the same full name with
    /// nothing against it. Check: anything else found (one first name and
    /// different numbers, different names sharing a number). Contacts called
    /// different people (`duplicates apart`) are not shown again. A pair in
    /// two accounts is listed apart: mirroring pairs those.
    ///
    /// JSON: {groups: [{confidence: "likely"|"check", members: [{id, name}],
    /// keep, why, merged}], across: [{a, b, names, why}]}; `merged` is the
    /// card the group would become.
    #[command(after_help = AFTER_HELP)]
    Duplicates {
        #[command(subcommand)]
        action: Option<DuplicatesAction>,
    },
    /// Merge contacts into one, losing nothing: `merge --all` merges every likely group, `merge A B [C...]` the ones named.
    ///
    /// The card kept gets every phone, email, URL and address of the others,
    /// the fuller name (the other name, when more than a spelling of it,
    /// becomes the nickname or a note), fields it lacks, and the others'
    /// different ones and notes in its notes. Before anything is deleted,
    /// every card is saved as a vCard in <data>/merged-backup/<date>.vcf;
    /// then the kept card is updated; then the others are deleted. In one
    /// account the card with the most in it is kept; across two accounts,
    /// the first named or the one in --keep ACCOUNT. Asks in text mode on a
    /// terminal; anywhere else --yes.
    ///
    /// JSON: {merged: [{kept, name, deleted: [id], backup, warnings}]}
    #[command(after_help = AFTER_HELP)]
    Merge {
        #[arg(value_name = "CONTACT", required_unless_present = "all")]
        contacts: Vec<String>,
        /// Every group `duplicates` calls likely (within one account each).
        #[arg(long, conflicts_with = "contacts")]
        all: bool,
        /// Across two accounts: keep the one in this account.
        #[arg(long, value_name = "ACCOUNT")]
        keep: Option<String>,
        #[arg(long, short)]
        yes: bool,
    },
    /// Every contact, or those matching QUERY, sorted by last name.
    ///
    /// QUERY matches a name, nickname, company or email, or (digits only)
    /// a phone number.
    ///
    /// JSON: [contact], each {id, account, provider, name, first, middle,
    /// last, nickname, company, job_title, department, is_company, me,
    /// phones: [{label, value}], emails, urls, addresses: [{label, street,
    /// city, state, postal_code, country, country_code}], birthday, notes,
    /// has_photo}
    #[command(after_help = AFTER_HELP)]
    List {
        #[arg(value_name = "QUERY")]
        query: Option<String>,
        /// Only this account's contacts.
        #[arg(long, value_name = "NAME")]
        account: Option<String>,
    },
    /// One contact, with everything it holds.
    ///
    /// JSON: the contact, as in `list`.
    #[command(after_help = AFTER_HELP)]
    Show {
        #[arg(value_name = "CONTACT")]
        contact: String,
    },
    /// Add a contact to one account.
    ///
    /// It needs a name, a company, a phone or an email. With more than one
    /// account linked, --account says which.
    ///
    /// JSON: {action: "add", contact}
    #[command(after_help = AFTER_HELP)]
    Add {
        #[command(flatten)]
        fields: Fields,
        /// Show it as a company (by --company, not a person's name).
        #[arg(long)]
        is_company: bool,
        /// The account to add it to (needed with more than one linked).
        #[arg(long, value_name = "NAME")]
        account: Option<String>,
    },
    /// Change a contact's fields, add phones, emails and URLs, or remove them.
    ///
    /// What it does not name stays as it is, including what this tool does
    /// not show (photo, social profiles, related names, Google's labels and
    /// groups). An empty value (--nickname "") removes a field.
    ///
    /// JSON: {action: "edit", contact}
    #[command(after_help = AFTER_HELP)]
    Edit {
        #[arg(value_name = "CONTACT")]
        contact: String,
        #[command(flatten)]
        fields: Fields,
        /// Remove a phone number, matched by its digits (repeatable).
        #[arg(long = "remove-phone", value_name = "PHONE")]
        remove_phones: Vec<String>,
        /// Remove an email address (repeatable).
        #[arg(long = "remove-email", value_name = "EMAIL")]
        remove_emails: Vec<String>,
        /// Remove a URL (repeatable).
        #[arg(long = "remove-url", value_name = "URL")]
        remove_urls: Vec<String>,
    },
    /// Delete a contact from its account, on every device.
    ///
    /// iCloud has no Recently Deleted for contacts (only icloud.com's Data
    /// Recovery brings an earlier address book back); Google keeps deleted
    /// contacts in its Trash for 30 days. Asks in text mode on a terminal
    /// (stdin and stdout); anywhere else --yes is required (a usage error,
    /// exit 2, otherwise).
    ///
    /// JSON: {action: "delete", contact: {id, name}}
    #[command(after_help = AFTER_HELP)]
    Delete {
        #[arg(value_name = "CONTACT")]
        contact: String,
        /// Do not ask; required when nobody can be asked (not a terminal, or a machine output mode).
        #[arg(long, short)]
        yes: bool,
    },
    /// Write contacts as vCard 3.0: every one, or those named.
    ///
    /// To --output FILE, or to stdout: the vCards themselves in text mode
    /// (--styled when piped), inside the JSON otherwise. Photos are not
    /// included.
    ///
    /// JSON: {action: "export", count, file?, vcard?} (vcard when there is no --output)
    #[command(after_help = AFTER_HELP)]
    Export {
        #[arg(value_name = "CONTACT")]
        contacts: Vec<String>,
        #[arg(long, short, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Only this account's contacts (with no CONTACT named).
        #[arg(long, value_name = "NAME")]
        account: Option<String>,
    },
    /// Fetch every account's contacts into this computer's cache.
    ///
    /// JSON: {accounts: [{account, contacts, changed} or {account, error}]}
    #[command(after_help = AFTER_HELP)]
    Refresh {
        /// Only this account.
        #[arg(long, value_name = "NAME")]
        account: Option<String>,
    },
    /// Link, sign in to again, or unlink an iCloud or Google account.
    #[command(after_help = AFTER_HELP)]
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },
    /// Print the agent skill (SKILL.md) built into cloud-contacts, or install it for your coding agents.
    Skill {
        #[command(subcommand)]
        action: Option<SkillAction>,
    },
}

#[derive(Subcommand)]
enum DuplicatesAction {
    /// These are different people: never shown as duplicates of each other again
    Apart {
        #[arg(value_name = "CONTACT", num_args = 2.., required = true)]
        contacts: Vec<String>,
    },
}

#[derive(Subcommand)]
enum MirrorAction {
    /// Turn it on (pairs, then copies; asks first)
    On {
        #[arg(long, short)]
        yes: bool,
    },
    /// Turn it off: both accounts keep their contacts as they are
    Off,
    /// Mirror them now
    Run {
        /// Confirm a run that stopped at too_many_deletions
        #[arg(long, short)]
        yes: bool,
    },
    /// Call a waiting pair one person: they are linked, not copied
    Pair {
        #[arg(value_name = "ICLOUD_CONTACT")]
        icloud: String,
        #[arg(value_name = "GOOGLE_CONTACT")]
        google: String,
    },
    /// Call a waiting pair different people: each is copied to the other account
    Apart {
        #[arg(value_name = "ICLOUD_CONTACT")]
        icloud: String,
        #[arg(value_name = "GOOGLE_CONTACT")]
        google: String,
    },
}

#[derive(Subcommand)]
enum AccountAction {
    /// The linked accounts (as `status`).
    List,
    /// Link an account and sign in to it: iCloud through icloud-session (its
    /// window), Google in the browser through Google's Workspace CLI `gws`.
    ///
    /// JSON: {account, provider, note}
    Add {
        #[arg(value_parser = ["icloud", "google"])]
        provider: String,
        /// Its name, the prefix of its contacts' IDs [default: the provider].
        #[arg(long)]
        name: Option<String>,
    },
    /// Sign in to a linked account again.
    Login { name: String },
    /// Unlink an account: its contacts stay in the account, this computer
    /// forgets its cache (and, for Google, its sign-in).
    Remove {
        name: String,
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SkillAction {
    /// Copy SKILL.md to ~/.agents/skills/cloud-contacts/ and link it into ~/.claude/skills/ when Claude Code is installed
    Install,
    /// Remove what install created
    Remove,
}

const SKILL: agent_skill::Skill = agent_skill::Skill {
    name: "cloud-contacts",
    body: include_str!("../../../skills/cloud-contacts/SKILL.md"),
    cli: "cloud-contacts",
    version: env!("CARGO_PKG_VERSION"),
    adopts_legacy: false,
};

/// A failure: the contract's error object and exit code.
type Failure = CliError;

type Outcome = Result<Response, Failure>;

/// An error as the contract's code, exit code, message and hint: iCloud's
/// as every iCloud tool reports them, Google's and the accounts' own here.
pub fn failure(e: &Error) -> Failure {
    match e {
        Error::Session(e) => session_failure(e),
        Error::Google { account, kind, message } => match kind {
            Kind::Auth => CliError::new("account_unauthorized", EXIT_SIGN_IN, message.clone())
                .hint(format!("run `cloud-contacts account login {account}`")),
            Kind::Unavailable => CliError::new("account_unavailable", EXIT_UNAVAILABLE, message.clone()),
            Kind::NotFound => CliError::new("not_found", EXIT_NOT_FOUND, message.clone()),
            Kind::BadRequest | Kind::Conflict => CliError::new("bad_request", EXIT_ERROR, message.clone()),
        },
        Error::Config(m) => CliError::new("not_configured", EXIT_SIGN_IN, m.clone())
            .hint("link one with `cloud-contacts account add icloud` or `cloud-contacts account add google`"),
        e @ (Error::Refused { .. } | Error::Other(_)) => other(e.to_string()),
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        failure(&e)
    }
}

/// "1 contact", "N contacts".
fn contacts(n: usize) -> String {
    match n {
        1 => "1 contact".into(),
        n => format!("{n} contacts"),
    }
}

/// Runs the command line on the process arguments and returns the exit code.
pub fn run() -> u8 {
    let args = match output::parse::<Args>() {
        Ok(args) => args,
        Err(code) => return code,
    };
    let mode = args.out.mode();
    if let Some(home) = agent_skill::home() {
        agent_skill::refresh(&home, &SKILL);
    }
    if let Command::Skill { action: None } = args.command {
        print!("{}", SKILL.body);
        return 0;
    }
    let result = (|| {
        if let Command::Skill { action: Some(action) } = args.command {
            return skill(action);
        }
        let root = args.data_dir.or_else(Store::default_root).ok_or_else(no_data_dir)?;
        let (config, note) = config::first_run(config::icloud_signed_in)?;
        if let Some(n) = &note
            && mode == Mode::Human
        {
            eprintln!("{}", output::terminal_safe(n));
        }
        let book = Book::open(&config, root.clone(), Arc::new(SessionTransport::default()))?;
        let cli = Cli {
            mode,
            cached: args.cached,
            book,
            root,
            config,
        };
        let resp = cli.dispatch(args.command)?;
        Ok(match note {
            Some(n) if mode.is_machine() => resp.meta("notice", n),
            _ => resp,
        })
    })();
    match result {
        Ok(resp) => resp.print(mode),
        Err(e) => e.report(mode),
    }
}

fn skill(action: SkillAction) -> Outcome {
    let home = agent_skill::home().ok_or_else(|| other("no home directory to install the skill into"))?;
    let (result, done) = match action {
        SkillAction::Install => (agent_skill::install(&home, &SKILL), "Installed"),
        SkillAction::Remove => (agent_skill::remove(&home, &SKILL), "Removed"),
    };
    let out = result.map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => CliError::new("skill_unmanaged", EXIT_ERROR, e.to_string())
            .hint("move that path aside, then run `cloud-contacts skill install` again"),
        _ => CliError::new("io", EXIT_ERROR, e.to_string()),
    })?;
    let ids: Vec<String> = out.paths.iter().map(|p| p.display().to_string()).collect();
    let summary = match ids.len() {
        0 => format!("{done} nothing: the cloud-contacts skill was already in that state"),
        n => format!(
            "{done} the cloud-contacts skill: {n} path{}",
            if n == 1 { "" } else { "s" }
        ),
    };
    let mut human = vec![summary.clone()];
    human.extend(ids.iter().map(|p| format!("  {p}")));
    human.extend(out.notices.iter().map(|n| format!("note: {n}")));
    Ok(
        Response::new(json!({ "paths": out.paths, "notices": out.notices }), summary)
            .human(human.join("\n"))
            .ids(ids),
    )
}

struct Cli {
    mode: Mode,
    cached: bool,
    book: Book,
    root: PathBuf,
    config: config::Config,
}

impl Cli {
    fn dispatch(&self, command: Command) -> Outcome {
        match command {
            Command::Status
            | Command::Account {
                action: AccountAction::List,
            } => Ok(self.status()),
            Command::List { query, account } => self.list(query.as_deref(), account.as_deref()),
            Command::Show { contact } => self.show(&contact),
            Command::Add {
                fields,
                is_company,
                account,
            } => self.add(fields, is_company, account.as_deref()),
            Command::Edit {
                contact,
                fields,
                remove_phones,
                remove_emails,
                remove_urls,
            } => {
                // Removals first: one matches a value whatever its label, so
                // after the additions it would take away the value added
                // again under another label (how a label is changed).
                let mut changes = Vec::new();
                for (m, values) in [
                    (Multi::Phone, remove_phones),
                    (Multi::Email, remove_emails),
                    (Multi::Url, remove_urls),
                ] {
                    changes.extend(values.into_iter().map(|v| Change::Remove(m, v)));
                }
                changes.extend(self::changes(fields)?);
                self.edit(&contact, changes)
            }
            Command::Delete { contact, yes } => self.delete(&contact, yes),
            Command::Export {
                contacts,
                output,
                account,
            } => self.export(&contacts, output, account.as_deref()),
            Command::Refresh { account } => self.refresh(account.as_deref()),
            Command::Mirror { action } => self.mirror(action),
            Command::Duplicates { action: None } => self.duplicates(),
            Command::Duplicates {
                action: Some(DuplicatesAction::Apart { contacts }),
            } => self.apart(&contacts),
            Command::Merge { all: true, yes, .. } => self.merge_all(yes),
            Command::Merge {
                contacts, keep, yes, ..
            } => self.merge(&contacts, keep.as_deref(), yes),
            Command::Account { action } => self.account(action),
            Command::Skill { .. } => unreachable!("handled before the accounts open"),
        }
    }

    /// Every account's contacts after a fetch (or as the last fetch left
    /// them with `--cached`).
    fn fresh(&self, only: Option<&str>) -> Result<Shown, Failure> {
        Ok(self.book.read(only, self.cached)?)
    }

    fn status(&self) -> Response {
        let list = accounts::statuses(&self.config, &self.root);
        let human = if list.is_empty() {
            "No account linked: `cloud-contacts account add icloud` or `cloud-contacts account add google`".to_string()
        } else {
            list.iter()
                .map(|a| {
                    format!(
                        "{:<12} {:<7} {}{}",
                        a.name,
                        a.label,
                        if a.signed_in { "" } else { "not signed in: " },
                        a.detail
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let summary = match list.len() {
            1 => "1 account".to_string(),
            n => format!("{n} accounts"),
        };
        let ids = list.iter().map(|a| a.name.clone()).collect();
        let mut data = json!({ "accounts": list });
        let mut human = human;
        if let mirror::Pairing::Ready(..) = mirror::pairing_of(&self.book) {
            let st = mirror::load(&self.root).unwrap_or_default();
            data["mirror"] = mirror::status_json(&st);
            human.push_str(&format!("\nmirror: {}", mirror_line(&st)));
        }
        Response::new(data, summary).human(human).ids(ids)
    }

    fn list(&self, query: Option<&str>, account: Option<&str>) -> Outcome {
        let shown = self.fresh(account)?;
        let rows: Vec<&Entry> = shown
            .entries
            .iter()
            .filter(|e| query.is_none_or(|q| e.contact.matches(q)))
            .collect();
        let several = self.book.accounts.len() > 1;
        let data: Vec<Value> = rows.iter().map(|e| contact_json(e)).collect();
        let ids: Vec<String> = rows.iter().map(|e| e.id()).collect();
        let human = if rows.is_empty() {
            "No contacts.".to_string()
        } else {
            rows.iter().map(|e| line(e, several)).collect::<String>()
        };
        Ok(with_warnings(
            Response::new(data, contacts(rows.len()))
                .human(human.trim_end())
                .ids(ids),
            &shown,
            self.mode,
        ))
    }

    fn show(&self, query: &str) -> Outcome {
        let shown = self.fresh(None)?;
        let e = resolve(&shown, query)?;
        Ok(with_warnings(
            Response::new(contact_json(e), e.contact.display_name())
                .human(detail(e).trim_end())
                .ids(vec![e.id()]),
            &shown,
            self.mode,
        ))
    }

    /// The account a new contact goes to: the one named, or the only one.
    fn target(&self, account: Option<&str>) -> Result<String, Failure> {
        match (account, self.book.accounts.as_slice()) {
            (Some(name), _) => Ok(self.book.account(name)?.name.clone()),
            (None, [only]) => Ok(only.name.clone()),
            (None, []) => Err(failure(&Error::Config("no account is linked".into()))),
            (None, many) => Err(usage(format!(
                "add needs --account with more than one account linked: {}",
                many.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    fn add(&self, fields: Fields, is_company: bool, account: Option<&str>) -> Outcome {
        let mut changes = changes(fields)?;
        // Empty values mean "remove", which a new card has nothing to.
        changes.retain(|c| !matches!(c, Change::Set(_, v) if v.is_empty()));
        if !model::names_someone(&changes) {
            return Err(usage(
                "add needs a name (--first, --last, --company, ...), a --phone or an --email",
            ));
        }
        if is_company && !changes.iter().any(|c| matches!(c, Change::Set(Field::Company, _))) {
            return Err(usage("--is-company needs --company"));
        }
        let account = self.target(account)?;
        let saved = self.book.add(&account, &changes, is_company)?;
        let e = self.entry(&account, saved.value);
        Ok(done(
            "add",
            &e,
            &format!("Added {} to {}", e.contact.display_name(), account),
            saved.warning,
        ))
    }

    fn entry(&self, account: &str, contact: model::Contact) -> Entry {
        let provider = self
            .book
            .account(account)
            .map(|a| a.provider.clone())
            .unwrap_or_default();
        Entry {
            account: account.to_string(),
            provider,
            contact,
            me: false,
        }
    }

    fn edit(&self, query: &str, changes: Vec<Change>) -> Outcome {
        if changes.is_empty() {
            return Err(usage(
                "edit needs a field to set (--last, --phone, --remove-email, ...)",
            ));
        }
        let shown = self.fresh(None)?;
        let e = resolve(&shown, query)?.clone();
        let saved = self.book.update(&e, &changes)?;
        let after = Entry {
            contact: saved.value,
            ..e
        };
        Ok(done(
            "edit",
            &after,
            &format!("Changed {}", after.contact.display_name()),
            saved.warning,
        ))
    }

    fn delete(&self, query: &str, yes: bool) -> Outcome {
        let shown = self.fresh(None)?;
        let e = resolve(&shown, query)?.clone();
        let name = e.contact.display_name();
        confirm(
            self.mode,
            "delete",
            yes,
            &format!("Delete {name} from {} ({}), on every device?", e.label(), e.account),
        )?;
        let warning = self.book.delete(&e)?;
        let mut data = json!({ "action": "delete", "contact": { "id": e.id(), "account": e.account, "name": name } });
        warn(&mut data, warning);
        Ok(Response::new(data, format!("Deleted {name}")).ids(vec![e.id()]))
    }

    fn export(&self, queries: &[String], output: Option<PathBuf>, account: Option<&str>) -> Outcome {
        let shown = self.fresh(account)?;
        let chosen: Vec<&Entry> = if queries.is_empty() {
            shown.entries.iter().collect()
        } else {
            queries.iter().map(|q| resolve(&shown, q)).collect::<Result<_, _>>()?
        };
        let text: String = chosen.iter().map(|e| vcard::card(&e.contact)).collect();
        let count = chosen.len();
        let ids: Vec<String> = chosen.iter().map(|e| e.id()).collect();
        let resp = match output {
            Some(path) => {
                std::fs::write(&path, &text).map_err(|e| other(format!("cannot write {}: {e}", path.display())))?;
                Response::new(
                    json!({ "action": "export", "count": count, "file": path }),
                    format!("Wrote {} to {}", contacts(count), path.display()),
                )
            }
            // Text mode into a pipe or a file (`--styled`): the vCards as
            // they are, CRLF line ends and all.
            None if self.mode == Mode::Human && !io::stdout().is_terminal() => {
                for w in &shown.warnings {
                    warning(&format!("{}: {}", w.account, w.message));
                }
                io::stdout()
                    .write_all(text.as_bytes())
                    .map_err(|e| other(e.to_string()))?;
                return Ok(Response::silent());
            }
            None => {
                let human = text.replace('\r', "");
                Response::new(
                    json!({ "action": "export", "count": count, "vcard": text }),
                    format!("Exported {}", contacts(count)),
                )
                .human(human.trim_end())
            }
        };
        Ok(with_warnings(resp.ids(ids), &shown, self.mode))
    }

    fn refresh(&self, only: Option<&str>) -> Outcome {
        let results = self.book.sync(only)?;
        let mut rows = Vec::new();
        let mut lines = Vec::new();
        let (mut changed, mut failed) = (0, Vec::new());
        for (a, r) in &results {
            match r {
                Ok((report, _)) => {
                    changed += report.changed;
                    rows.push(json!({ "account": a.name, "contacts": report.contacts, "changed": report.changed }));
                    lines.push(format!(
                        "{}: {} ({} changed)",
                        a.name,
                        contacts(report.contacts),
                        report.changed
                    ));
                }
                Err(e) => {
                    let f = failure(e);
                    rows.push(json!({ "account": a.name, "error": { "code": f.code, "message": f.message } }));
                    lines.push(format!("{}: failed: {}", a.name, f.message));
                    failed.push(f);
                }
            }
        }
        if failed.len() == results.len()
            && let Some(f) = failed.into_iter().next()
        {
            return Err(f);
        }
        let summary = format!("Refreshed: {changed} changed");
        Ok(Response::new(json!({ "accounts": rows }), summary)
            .human(lines.join("\n"))
            .count(changed))
    }

    /// The two accounts mirrored, or why there are not two.
    fn pair(&self) -> Result<(&crate::book::Account, &crate::book::Account), Failure> {
        match mirror::pairing_of(&self.book) {
            mirror::Pairing::Ready(i, g) => Ok((i, g)),
            mirror::Pairing::Missing(p) => Err(CliError::new(
                "needs_both_accounts",
                EXIT_SIGN_IN,
                "mirroring contacts needs an iCloud and a Google account",
            )
            .hint(format!(
                "link the {} one with `cloud-contacts account add {p}`",
                crate::book::label(p)
            ))),
            mirror::Pairing::Ambiguous(m) => Err(CliError::new("ambiguous", EXIT_USAGE, m)
                .hint("unlink the Google accounts you don't want mirrored with iCloud")),
        }
    }

    fn mirror(&self, action: Option<MirrorAction>) -> Outcome {
        let (i, g) = self.pair()?;
        let root = &self.root;
        let status = |extra: Option<Value>, summary: String| -> Outcome {
            let st = mirror::load(root)?;
            let mut data = mirror::status_json(&st);
            if let Some(Value::Object(m)) = extra {
                for (k, v) in m {
                    data[k] = v;
                }
            }
            let human = format!("{summary}\n{}{}", mirror_line(&st), waiting_lines(&st));
            Ok(Response::new(data, summary).human(human.trim_end()))
        };
        let mut engine = mirror::Engine::new(root, &*i.provider_impl, &*g.provider_impl);
        match action {
            None => {
                let st = mirror::load(root)?;
                status(
                    None,
                    format!("Mirroring iCloud and Google: {}", if st.on { "on" } else { "off" }),
                )
            }
            Some(MirrorAction::Off) => {
                let was = mirror::turn_off(root)?;
                let warning = mirror::timer(false);
                let mut data = json!({});
                warn(&mut data, warning);
                status(
                    Some(data),
                    if was {
                        "Turned off: both accounts keep their contacts as they are"
                    } else {
                        "It was off"
                    }
                    .into(),
                )
            }
            Some(MirrorAction::On { yes }) => {
                let plan = engine.plan().map_err(run_failure)?;
                let question = format!(
                    "Mirror contacts between iCloud and Google? {} already in both will be linked, {} copied to Google and {} to iCloud; {} possible duplicates wait for you.",
                    plan.pairs.len(),
                    plan.to_google,
                    plan.to_icloud,
                    plan.doubtful.len()
                );
                confirm(self.mode, "mirror on", yes, &question)?;
                mirror::turn_on(root, &plan, &[])?;
                let mut data = json!({ "plan": plan });
                warn(&mut data, mirror::timer(true));
                match engine.run(false) {
                    Ok(report) => data["report"] = json!(report),
                    Err(e) => warn(&mut data, Some(e.to_string())),
                }
                status(Some(data), "Turned on: iCloud and Google contacts are mirrored".into())
            }
            Some(MirrorAction::Run { yes }) => {
                if !mirror::load(root)?.on {
                    return Err(CliError::new("off", EXIT_USAGE, "mirroring iCloud and Google is off")
                        .hint("turn it on with `cloud-contacts mirror on`"));
                }
                match engine.run(yes) {
                    Ok(report) => {
                        let summary = format!(
                            "Mirrored: {} changed, {} copied to Google, {} to iCloud, {} deleted",
                            report.changed, report.copied_to_google, report.copied_to_icloud, report.deleted
                        );
                        status(Some(json!({ "report": report })), summary)
                    }
                    Err(e @ mirror::RunError::Account(..)) => {
                        let msg = e.to_string();
                        if self.mode == Mode::Human {
                            warning(&msg);
                        }
                        Ok(status(Some(json!({ "report": null })), msg.clone())?
                            .meta("warnings", [msg])
                            .exit(EXIT_UNAVAILABLE))
                    }
                    Err(e) => Err(run_failure(e)),
                }
            }
            Some(MirrorAction::Pair { icloud, google }) => self.decide(i, g, &icloud, &google, mirror::Decision::Same),
            Some(MirrorAction::Apart { icloud, google }) => {
                self.decide(i, g, &icloud, &google, mirror::Decision::Different)
            }
        }
    }

    /// `pair` or `apart`: the person's call on two contacts, one in each
    /// account (an ID with or without its account's prefix).
    fn decide(
        &self,
        i: &crate::book::Account,
        g: &crate::book::Account,
        icloud: &str,
        google: &str,
        d: mirror::Decision,
    ) -> Outcome {
        let local =
            |id: &str, acc: &crate::book::Account| id.strip_prefix(&format!("{}:", acc.name)).unwrap_or(id).to_string();
        let (li, lg) = (local(icloud, i), local(google, g));
        mirror::decide(&self.root, &li, &lg, d)?;
        let what = match d {
            mirror::Decision::Same => "one person: linked, not copied",
            mirror::Decision::Different => "different people: each is copied to the other account",
        };
        let st = mirror::load(&self.root)?;
        Ok(Response::new(
            mirror::status_json(&st),
            format!("{}:{li} and {}:{lg} are {what}", i.name, g.name),
        ))
    }

    /// Pairs mirroring links, both ways.
    fn mirror_linked(&self) -> std::collections::BTreeSet<(String, String)> {
        mirror::load(&self.root)
            .map(|s| {
                s.links
                    .into_iter()
                    .flat_map(|l| [(l.icloud.clone(), l.google.clone()), (l.google, l.icloud)])
                    .collect()
            })
            .unwrap_or_default()
    }

    fn duplicates(&self) -> Outcome {
        let shown = self.fresh(None)?;
        let apart = dedupe::Apart::load(&self.root)?;
        let (groups, across) = dedupe::find(&shown.entries, &apart, &self.mirror_linked());
        let mut data_groups = Vec::new();
        let mut human = Vec::new();
        let likely = groups
            .iter()
            .filter(|g| g.confidence == dedupe::Confidence::Likely)
            .count();
        for (n, g) in groups.iter().enumerate() {
            let members: Vec<&Entry> = g.members.iter().map(|&m| &shown.entries[m]).collect();
            let (keep, merged) = preview(&members);
            if n == 0 && likely > 0 {
                human.push("Likely the same (merge them all: cloud-contacts merge --all):".to_string());
            }
            if n == likely {
                human.push("Check these:".to_string());
            }
            human.push(format!("  {}", merged.contact.display_name()));
            for e in &members {
                human.push(format!("    {} ({})", e.contact.display_name(), e.id()));
            }
            human.push(format!("    why: {}", g.why.join(", ")));
            if g.confidence == dedupe::Confidence::Check {
                let ids: Vec<String> = members.iter().map(|e| e.id()).collect();
                human.push(format!(
                    "    merge: cloud-contacts merge {0}   apart: cloud-contacts duplicates apart {0}",
                    ids.join(" ")
                ));
            }
            data_groups.push(json!({
                "confidence": g.confidence,
                "members": members.iter().map(|e| json!({ "id": e.id(), "name": e.contact.display_name() })).collect::<Vec<_>>(),
                "keep": members[keep].id(),
                "why": g.why,
                "merged": contact_json(&merged),
            }));
        }
        let data_across: Vec<Value> = across
            .iter()
            .map(|p| {
                let (a, b) = (&shown.entries[p.a], &shown.entries[p.b]);
                json!({ "a": a.id(), "b": b.id(), "names": [a.contact.display_name(), b.contact.display_name()], "why": p.why })
            })
            .collect();
        if !across.is_empty() {
            human.push("In both accounts (mirroring pairs these):".to_string());
            for p in &across {
                let (a, b) = (&shown.entries[p.a], &shown.entries[p.b]);
                human.push(format!(
                    "  {} ({})  and  {} ({})",
                    a.contact.display_name(),
                    a.id(),
                    b.contact.display_name(),
                    b.id()
                ));
            }
        }
        let n = groups.len();
        let summary = format!(
            "{likely} likely, {} to check{}",
            n - likely,
            if across.is_empty() {
                String::new()
            } else {
                format!(", {} across accounts", across.len())
            }
        );
        let human = if n == 0 && across.is_empty() {
            "No duplicates.".to_string()
        } else {
            format!("{}\n{summary}", human.join("\n"))
        };
        Ok(with_warnings(
            Response::new(json!({ "groups": data_groups, "across": data_across }), summary)
                .human(human)
                .count(n),
            &shown,
            self.mode,
        ))
    }

    fn apart(&self, contacts: &[String]) -> Outcome {
        let shown = self.fresh(None)?;
        let ids: Vec<String> = contacts
            .iter()
            .map(|c| resolve(&shown, c).map(Entry::id))
            .collect::<Result<_, _>>()?;
        dedupe::Apart::add(&self.root, &ids)?;
        Ok(Response::new(
            json!({ "apart": ids }),
            format!("{} contacts kept apart: not shown as duplicates again", ids.len()),
        ))
    }

    fn merge_all(&self, yes: bool) -> Outcome {
        let shown = self.fresh(None)?;
        let apart = dedupe::Apart::load(&self.root)?;
        let (groups, _) = dedupe::find(&shown.entries, &apart, &self.mirror_linked());
        let likely: Vec<Vec<&Entry>> = groups
            .iter()
            .filter(|g| g.confidence == dedupe::Confidence::Likely)
            .map(|g| g.members.iter().map(|&m| &shown.entries[m]).collect())
            .collect();
        if likely.is_empty() {
            return Ok(Response::new(
                json!({ "merged": [] }),
                "No likely duplicates to merge".to_string(),
            ));
        }
        let cards: usize = likely.iter().map(Vec::len).sum();
        let backup = dedupe::backup_path(&self.root);
        confirm(
            self.mode,
            "merge --all",
            yes,
            &format!(
                "Merge {cards} contacts into {}? Each card is saved first to {}.",
                likely.len(),
                backup.display()
            ),
        )?;
        let mut done = Vec::new();
        for group in &likely {
            let cards: Vec<&model::Contact> = group.iter().map(|e| &e.contact).collect();
            match dedupe::apply(&self.book, &self.root, group, dedupe::keeper(&cards), false) {
                Ok(d) => done.push(d),
                Err(e) => {
                    let mut f = failure(&e);
                    f.message = format!(
                        "merged {} of {} groups, then {}: {}",
                        done.len(),
                        likely.len(),
                        group[0].contact.display_name(),
                        f.message
                    );
                    return Err(f);
                }
            }
        }
        Ok(merged_response(done))
    }

    fn merge(&self, contacts: &[String], keep: Option<&str>, yes: bool) -> Outcome {
        if contacts.len() < 2 {
            return Err(usage("merge needs two contacts or more, or --all"));
        }
        let shown = self.fresh(None)?;
        let mut group: Vec<Entry> = Vec::new();
        for c in contacts {
            let e = resolve(&shown, c)?.clone();
            if group.iter().any(|g| g.id() == e.id()) {
                return Err(usage("merge needs different contacts"));
            }
            group.push(e);
        }
        let accounts: std::collections::BTreeSet<&str> = group.iter().map(|e| e.account.as_str()).collect();
        let keep_at = if accounts.len() == 1 {
            let cards: Vec<&model::Contact> = group.iter().map(|e| &e.contact).collect();
            dedupe::keeper(&cards)
        } else {
            if group.len() != 2 {
                return Err(usage("across two accounts, merge two contacts at a time"));
            }
            let linked = self.mirror_linked();
            if mirror::load(&self.root).unwrap_or_default().on
                && linked.contains(&(group[0].contact.id().to_string(), group[1].contact.id().to_string()))
            {
                return Err(usage(
                    "these two are one contact mirrored in iCloud and Google; there is nothing to merge",
                ));
            }
            match keep {
                Some(acc) if group[1].account == acc => 1,
                Some(acc) if group[0].account != acc => {
                    return Err(usage(format!("neither contact is in {acc}")));
                }
                _ => 0,
            }
        };
        let others: Vec<String> = group
            .iter()
            .enumerate()
            .filter(|(n, _)| *n != keep_at)
            .map(|(_, e)| format!("{} ({})", e.contact.display_name(), e.id()))
            .collect();
        confirm(
            self.mode,
            "merge",
            yes,
            &format!(
                "Merge {} into {} ({}), then delete {}? Each card is saved first to {}.",
                others.join(", "),
                group[keep_at].contact.display_name(),
                group[keep_at].id(),
                if others.len() == 1 { "it" } else { "them" },
                dedupe::backup_path(&self.root).display()
            ),
        )?;
        let refs: Vec<&Entry> = group.iter().collect();
        let done = dedupe::apply(&self.book, &self.root, &refs, keep_at, accounts.len() > 1)?;
        Ok(merged_response(vec![done]))
    }

    fn account(&self, action: AccountAction) -> Outcome {
        match action {
            AccountAction::List => Ok(self.status()),
            AccountAction::Add { provider, name } => {
                let (name, note) = accounts::add(name.as_deref(), &provider, &self.root)?;
                let mut text = format!("Linked {name}: {note}");
                // both kinds now: say how to mirror them
                let config = config::load()?;
                let kinds: std::collections::BTreeSet<&str> =
                    config.accounts.iter().map(|(n, c)| c.provider(n)).collect();
                let off = !mirror::load(&self.root).unwrap_or_default().on;
                let hint = (kinds.contains("icloud") && kinds.contains("google") && off)
                    .then_some("mirror them with `cloud-contacts mirror on`");
                if let Some(h) = hint {
                    text.push_str(&format!("\n{h}"));
                }
                Ok(Response::new(
                    json!({ "account": name, "provider": provider, "note": note, "hint": hint }),
                    format!("Linked {name}: {note}"),
                )
                .human(text))
            }
            AccountAction::Login { name } => {
                let note = accounts::login(&name, &self.root)?;
                Ok(Response::new(
                    json!({ "account": name, "note": note }),
                    format!("{name}: {note}"),
                ))
            }
            AccountAction::Remove { name, yes } => {
                if !self.config.accounts.contains_key(&name) {
                    return Err(failure(&Error::Config(format!("no account named {name}"))));
                }
                confirm(
                    self.mode,
                    "account remove",
                    yes,
                    &format!("Unlink {name}? Its contacts stay in the account; this computer forgets them."),
                )?;
                let note = accounts::remove(&name, &self.root)?;
                Ok(Response::new(json!({ "account": name, "note": note }), format!("Unlinked {name}")).human(note))
            }
        }
    }
}

/// `resp` with the accounts that failed: in `meta.warnings` for a machine,
/// as `warning:` lines on stderr for a person.
fn with_warnings(resp: Response, shown: &Shown, mode: Mode) -> Response {
    if shown.warnings.is_empty() {
        return resp;
    }
    if mode == Mode::Human {
        for w in &shown.warnings {
            warning(&format!("{}: {}", w.account, w.message));
        }
    }
    resp.meta("warnings", &shown.warnings)
}

/// Mirroring's state in a line.
fn mirror_line(st: &mirror::State) -> String {
    match mirror::shown(&[("icloud".into(), true), ("google".into(), true)], st, ago) {
        mirror::Shown::Switch { line, .. } => line,
        mirror::Shown::Nothing => String::new(),
    }
}

fn waiting_lines(st: &mirror::State) -> String {
    st.waiting
        .iter()
        .map(|w| {
            format!(
                "\n  maybe one person: icloud:{} and google:{} ({})\n    `cloud-contacts mirror pair icloud:{} google:{}`, or `apart`",
                w.icloud, w.google, w.why, w.icloud, w.google
            )
        })
        .collect()
}

/// "5 minutes ago" and the like, from seconds since the epoch.
pub fn ago(t: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match now.saturating_sub(t) {
        0..60 => "just now".into(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s @ 3600..86_400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// A run's failure as the contract's error.
fn run_failure(e: mirror::RunError) -> Failure {
    match e {
        mirror::RunError::TooManyDeletions(list) => CliError::new("too_many_deletions", EXIT_USAGE, {
            let names: Vec<String> = list.iter().take(20).map(|d| d.name.clone()).collect();
            format!(
                "this run would delete {} contacts ({}{}); nothing changed",
                list.len(),
                names.join(", "),
                if list.len() > 20 { ", ..." } else { "" }
            )
        })
        .hint("if those deletions are meant, confirm with `cloud-contacts mirror run --yes`"),
        mirror::RunError::Account(_, e) => failure(&e),
        mirror::RunError::Other(e) => failure(&e),
    }
}

/// The card a group would become, and which member is kept.
fn preview(members: &[&Entry]) -> (usize, Entry) {
    let cards: Vec<&model::Contact> = members.iter().map(|e| &e.contact).collect();
    let keep = dedupe::keeper(&cards);
    let target = dedupe::merge_all(&cards, keep);
    let mut contact = members[keep].contact.clone();
    // a preview: changes that don't apply leave the card as it is
    _ = contact.apply(&mirror::Shared::of(&members[keep].contact).changes_to(&target));
    (
        keep,
        Entry {
            contact,
            ..members[keep].clone()
        },
    )
}

/// What `merge` answers.
fn merged_response(done: Vec<dedupe::Done>) -> Response {
    let ids: Vec<String> = done.iter().map(|d| d.kept.clone()).collect();
    let backup = done.first().map(|d| d.backup.display().to_string()).unwrap_or_default();
    let mut lines: Vec<String> = done
        .iter()
        .map(|d| format!("Merged {} into {} ({})", d.deleted.len() + 1, d.name, d.kept))
        .collect();
    lines.push(format!("Both cards of each were saved first to {backup}"));
    for d in &done {
        for w in &d.warnings {
            lines.push(format!("warning: {w}"));
        }
    }
    let n = done.len();
    Response::new(
        json!({ "merged": done }),
        format!("Merged {n} group{}; backup: {backup}", if n == 1 { "" } else { "s" }),
    )
    .human(lines.join("\n"))
    .ids(ids)
}

/// What `add` and `edit` answer: the contact as saved.
fn done(action: &str, e: &Entry, text: &str, warning: Option<String>) -> Response {
    let mut data = json!({ "action": action, "contact": contact_json(e) });
    warn(&mut data, warning);
    Response::new(data, text.to_string()).ids(vec![e.id()])
}

/// The changes `fields` asks for, checked.
fn changes(f: Fields) -> Result<Vec<Change>, Failure> {
    let mut out = Vec::new();
    let birthday = f
        .birthday
        .map(|b| model::parse_birthday(&b))
        .transpose()
        .map_err(usage)?;
    for (field, v) in [
        (Field::First, f.first),
        (Field::Middle, f.middle),
        (Field::Last, f.last),
        (Field::Nickname, f.nickname),
        (Field::Company, f.company),
        (Field::JobTitle, f.job_title),
        (Field::Department, f.department),
        (Field::Birthday, birthday),
    ] {
        if let Some(v) = v {
            out.push(Change::Set(field, v.trim().to_owned()));
        }
    }
    if let Some(n) = f.notes {
        out.push(Change::Set(Field::Notes, n));
    }
    for (m, values) in [(Multi::Phone, f.phones), (Multi::Email, f.emails), (Multi::Url, f.urls)] {
        for v in values {
            let l = model::parse_labeled(&v, m.default_label());
            m.check(&l.value).map_err(usage)?;
            out.push(Change::Add(m, l));
        }
    }
    Ok(out)
}

/// Finds a contact by ID or `me`, else by name (ignoring case), email or
/// phone number, else by a unique part of its name.
fn resolve<'c>(shown: &'c Shown, query: &str) -> Result<&'c Entry, Failure> {
    let q = query.trim();
    if let Some(e) = shown.get(q) {
        return Ok(e);
    }
    // an account's own id, when only one contact has it
    if let [e] = shown.entries.iter().filter(|e| e.contact.id() == q).collect::<Vec<_>>()[..] {
        return Ok(e);
    }
    if q.eq_ignore_ascii_case("me") {
        return shown.entries.iter().find(|e| e.me).ok_or_else(|| {
            CliError::new("not_found", EXIT_NOT_FOUND, "no \"me\" card in the linked accounts")
                .hint("set My Card on an Apple device")
        });
    }
    let f = model::fold(q);
    let all = &shown.entries;
    let pick = |keep: &dyn Fn(&model::Contact) -> bool| all.iter().filter(|e| keep(&e.contact)).collect::<Vec<_>>();
    let mut matches = pick(&|c| model::fold(&c.display_name()) == f || model::fold(&c.person_name()) == f);
    if matches.is_empty() && q.contains('@') {
        matches = pick(&|c| c.values(Multi::Email).iter().any(|e| Multi::Email.matches(&e.value, q)));
    }
    let d = model::digits(q);
    if matches.is_empty() && d.len() >= 3 && !q.chars().any(char::is_alphabetic) {
        // By the number's tail, so +1 555 0100 finds 555-0100 and the reverse.
        matches = pick(&|c| {
            c.values(Multi::Phone).iter().any(|p| {
                let pd = model::digits(&p.value);
                pd.ends_with(&d) || model::same_number(&p.value, q)
            })
        });
    }
    if matches.is_empty() && !f.is_empty() {
        matches = pick(&|c| model::fold(&c.display_name()).contains(&f));
    }
    match matches.as_slice() {
        [e] => Ok(e),
        [] => Err(
            CliError::new("not_found", EXIT_NOT_FOUND, format!("no contact matches \"{query}\""))
                .hint("`cloud-contacts list QUERY` searches names, companies, emails and phone numbers"),
        ),
        many => Err(CliError::new(
            "ambiguous",
            EXIT_USAGE,
            format!("\"{query}\" matches {} contacts", many.len()),
        )
        .hint(format!(
            "name one exactly or use its ID:{}",
            many.iter()
                .take(20)
                .map(|e| format!("\n  {}  ({})", e.contact.display_name(), e.id()))
                .collect::<String>()
        ))),
    }
}

fn labeled_json(ls: Vec<model::Labeled>) -> Value {
    json!(
        ls.into_iter()
            .map(|l| json!({ "label": model::label_display(&l.label), "value": l.value }))
            .collect::<Vec<_>>()
    )
}

fn opt(s: &str) -> Value {
    if s.is_empty() { Value::Null } else { json!(s) }
}

/// A contact as `list` and `show` give it, with its account.
fn contact_json(e: &Entry) -> Value {
    let (c, me) = (&e.contact, e.me);
    let addresses: Vec<Value> = c
        .addresses()
        .into_iter()
        .map(|a| {
            json!({
                "label": model::label_display(&a.label),
                "street": opt(&a.street),
                "city": opt(&a.city),
                "state": opt(&a.state),
                "postal_code": opt(&a.postal_code),
                "country": opt(&a.country),
                "country_code": opt(&a.country_code),
            })
        })
        .collect();
    let birthday = c.field(Field::Birthday);
    json!({
        "id": e.id(),
        "account": e.account,
        "provider": e.provider,
        "name": c.display_name(),
        "first": opt(c.field(Field::First)),
        "middle": opt(c.field(Field::Middle)),
        "last": opt(c.field(Field::Last)),
        "nickname": opt(c.field(Field::Nickname)),
        "company": opt(c.field(Field::Company)),
        "job_title": opt(c.field(Field::JobTitle)),
        "department": opt(c.field(Field::Department)),
        "is_company": c.is_company(),
        "me": me,
        "phones": labeled_json(c.values(Multi::Phone)),
        "emails": labeled_json(c.values(Multi::Email)),
        "urls": labeled_json(c.values(Multi::Url)),
        "addresses": addresses,
        "birthday": opt(&model::birthday_display(birthday)),
        "notes": opt(c.field(Field::Notes)),
        "has_photo": c.has_photo(),
    })
}

/// One line of `list`; `several`: name the account too.
fn line(e: &Entry, several: bool) -> String {
    let c = &e.contact;
    let mut extra = Vec::new();
    let company = c.field(Field::Company);
    if !company.is_empty() && !c.is_company() {
        extra.push(company.to_owned());
    }
    if let Some(p) = c.values(Multi::Phone).into_iter().next() {
        extra.push(p.value);
    }
    if let Some(e) = c.values(Multi::Email).into_iter().next() {
        extra.push(e.value);
    }
    let account = if several {
        format!("  [{}]", e.account)
    } else {
        String::new()
    };
    if extra.is_empty() {
        format!("{}{account}\n", c.display_name())
    } else {
        format!("{}  ({}){account}\n", c.display_name(), extra.join(", "))
    }
}

fn detail(e: &Entry) -> String {
    let (c, me) = (&e.contact, e.me);
    let mut out = format!("{}{}\n", c.display_name(), if me { "  (me)" } else { "" });
    let row = |out: &mut String, label: &str, value: &str| {
        if !value.is_empty() {
            *out += &format!("  {:<11}{value}\n", format!("{label}:"));
        }
    };
    let job = c.work(", ");
    row(&mut out, "nickname", c.field(Field::Nickname));
    row(&mut out, "work", &job);
    for (m, noun) in [(Multi::Phone, "phone"), (Multi::Email, "email"), (Multi::Url, "url")] {
        for l in c.values(m) {
            row(
                &mut out,
                noun,
                &format!("{}  ({})", l.value, model::label_display(&l.label)),
            );
        }
    }
    for a in c.addresses() {
        let place = [a.street.replace('\n', ", "), a.city, a.state, a.postal_code, a.country]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        row(
            &mut out,
            "address",
            &format!("{place}  ({})", model::label_display(&a.label)),
        );
    }
    row(&mut out, "birthday", &model::birthday_display(c.field(Field::Birthday)));
    row(&mut out, "account", &format!("{} ({})", e.account, e.label()));
    row(&mut out, "id", &e.id());
    let notes = c.field(Field::Notes);
    if !notes.is_empty() {
        out += "\n";
        for l in notes.lines() {
            out += &format!("  {l}\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn failures_use_the_shared_codes_exits_and_hints() {
        use icloud_session::Error as S;
        let code = |e: Error| {
            let f = Failure::from(e);
            (f.code, f.exit)
        };
        // icloud-session's failures read as in every iCloud tool.
        assert_eq!(code(S::Network("x".into()).into()), ("network".into(), 5));
        let rejected = Failure::from(Error::from(S::Http {
            status: 421,
            body: String::new(),
        }));
        assert_eq!((rejected.code.as_str(), rejected.exit), ("session_rejected", 3));
        assert_eq!(rejected.hint, Some(icloud_app::cli::sign_in_hint()));
        // Contacts' own.
        let refused = Error::Refused {
            code: 412,
            message: "x".into(),
        };
        assert_eq!(code(refused), ("error".into(), 1));
        assert_eq!(code(Error::Other("x".into())), ("error".into(), 1));
        // Google's and the accounts'.
        let google = |kind| Error::Google {
            account: "work".into(),
            kind,
            message: "x".into(),
        };
        assert_eq!(code(google(Kind::Auth)), ("account_unauthorized".into(), 3));
        assert_eq!(
            Failure::from(google(Kind::Auth)).hint.as_deref(),
            Some("run `cloud-contacts account login work`")
        );
        assert_eq!(code(google(Kind::Unavailable)), ("account_unavailable".into(), 5));
        assert_eq!(code(google(Kind::NotFound)), ("not_found".into(), 4));
        assert_eq!(code(Error::Config("x".into())), ("not_configured".into(), 3));
    }
}
