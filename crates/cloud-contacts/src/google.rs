//! A Google account, through Google's Workspace CLI `gws`
//! (github.com/googleworkspace/cli): every call is a raw People API request,
//! `gws people people <method> --params '<json>' [--json '<body>']`. gws owns
//! the tokens; Cloud Contacts never sees one.
//!
//! Isolation, as Cloud Calendar and Cloudmail do it: gws runs with its config
//! directory set to Cloud Contacts' own (`~/.config/cloud-contacts/gws/<account>`)
//! and no way to fall back to Application Default Credentials, so a gws setup
//! of your own is never read or changed. The "file" keyring backend keeps the
//! sign-in in that directory (gws on Linux keeps its key there either way).
//!
//! Sign-in: `gws auth login --scopes <SIGN_IN_SCOPES>` with the release's
//! Google OAuth client (see [`GOOGLE_CLIENT_ID`]): one browser sign-in.
//!
//! One sign-in for the Cloud apps: Cloudmail, Cloud Calendar and Cloud
//! Contacts sign in with the same client and each asks Google for what all
//! three use ([`SIGN_IN_SCOPES`]). Before opening the browser, an account that
//! isn't signed in looks at the other two apps' gws directories and, when one
//! address there works and covers [`SCOPES`], copies gws's two files from it
//! ([`Google::adopt`]). It is a copy: each app's directory stays its own, and
//! signing out of one app leaves the others signed in.
//!
//! Contacts are read into the shape iCloud's cards have (see `model`), so
//! the window, the command line and vCard export treat both accounts alike;
//! Google's own record rides along in the card under [`ORIGINAL`]. A write
//! turns the card back into a Google person on top of that record, so what
//! this app does not show (a phone's metadata, a second organization, other
//! fields) stays as Google has it. Google names a contact by its
//! `resourceName` (`people/c123`), which is its id here.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::account::{Provider, Saved, SyncReport, changes};
use crate::error::{Error, Kind, Result};
use crate::model::{Change, Contact};
use crate::store::{Cache, Store};

pub const COMMAND_ENV: &str = "CLOUD_CONTACTS_GWS_COMMAND";
/// The Google OAuth client to sign in with: Cloud Calendar's "Desktop app"
/// client (the People API is on in its project), whose secret Google treats
/// as public, so it ships in the binary as Cloud Calendar's and Cloudmail's
/// do. `CLOUD_CONTACTS_GOOGLE_CLIENT_ID` / `_SECRET` at run time, both set,
/// use another client instead.
pub const GOOGLE_CLIENT_ID: Option<&str> =
    Some("858534886670-37fmq8bl312beufkc420h9nvtjrb4l0d.apps.googleusercontent.com");
pub const GOOGLE_CLIENT_SECRET: Option<&str> = Some("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");
pub const CLIENT_ID_ENV: &str = "CLOUD_CONTACTS_GOOGLE_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "CLOUD_CONTACTS_GOOGLE_CLIENT_SECRET";
/// The program that opens the sign-in link (default: xdg-open).
pub const BROWSER_ENV: &str = "CLOUD_CONTACTS_BROWSER";
/// What Cloud Contacts uses: your contacts.
pub const SCOPES: [&str; 1] = ["https://www.googleapis.com/auth/contacts"];
/// What the browser sign-in asks for: [`SCOPES`], Cloudmail's (Gmail and its
/// filters) and Cloud Calendar's, so the sign-in serves all three apps. The
/// same list is in each of them.
pub const SIGN_IN_SCOPES: &str = "https://www.googleapis.com/auth/gmail.modify,https://www.googleapis.com/auth/gmail.settings.basic,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/calendar.calendarlist.readonly,https://www.googleapis.com/auth/contacts";
/// The other Cloud apps whose Google sign-in can be taken over: the directory
/// under `~/.config` that holds their `gws/<account>`, and the app's name.
const SIBLINGS: [(&str, &str); 2] = [("cloudmail", "Cloudmail"), ("cloud-calendar", "Cloud Calendar")];
/// The key gws encrypts `credentials.enc` with, beside it.
const KEY_FILE: &str = ".encryption_key";
pub const INSTALL_HINT: &str = "install Google's Workspace CLI: `npm install -g @googleworkspace/cli` (or a release binary from https://github.com/googleworkspace/cli/releases)";

/// The key in a card that holds Google's own record of the contact.
pub const ORIGINAL: &str = "googlePerson";
/// What is read of a contact.
const PERSON_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,organizations,biographies,urls,addresses,birthdays,photos,metadata,memberships,clientData";
/// What a write sets: every field this app shows. One left out of the
/// person written is removed.
const UPDATE_FIELDS: &str =
    "names,nicknames,emailAddresses,phoneNumbers,organizations,biographies,urls,addresses,birthdays,clientData";
/// The key of the client data that says a contact is shown by its company: Google keeps such data for the apps that
/// write it, and shows it nowhere.
const COMPANY_KEY: &str = "cloud-contacts.isCompany";

const TIMEOUT: Duration = Duration::from_secs(90);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// Pages of 1000 contacts read at most.
const MAX_PAGES: usize = 100;

pub struct Google {
    name: String,
    command: String,
    dir: PathBuf,
    /// None when neither this build nor the environment names one: gws
    /// then signs in with the client it is set up with (`gws auth setup`).
    client: Option<(String, String)>,
    store: Store,
}

/// Whether a sign-in's permissions include all of [`SCOPES`].
fn covers(scopes: &[String]) -> bool {
    SCOPES.iter().all(|s| scopes.iter().any(|g| g == s))
}

fn nonblank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Cloud Contacts' own gws directory for an account, beside its config file.
pub fn gws_dir(name: &str) -> PathBuf {
    crate::config::config_dir().join("gws").join(name)
}

impl Google {
    pub fn new(name: &str, cfg: &crate::config::AccountConfig, store: Store) -> Self {
        let command = nonblank(std::env::var(COMMAND_ENV).ok()).unwrap_or_else(|| "gws".into());
        let env = |k| nonblank(std::env::var(k).ok());
        let built_in = GOOGLE_CLIENT_ID
            .zip(GOOGLE_CLIENT_SECRET)
            .map(|(id, secret)| (id.to_string(), secret.to_string()));
        let client = env(CLIENT_ID_ENV).zip(env(CLIENT_SECRET_ENV)).or(built_in);
        Self {
            name: name.into(),
            command,
            dir: cfg.gws_dir.clone().unwrap_or_else(|| gws_dir(name)),
            client,
            store,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn signed_in(&self) -> bool {
        self.dir.join("credentials.enc").is_file()
    }

    fn fail(&self, kind: Kind, message: impl AsRef<str>) -> Error {
        Error::Google {
            account: self.name.clone(),
            kind,
            message: format!("Google ({}): {}", self.name, message.as_ref()),
        }
    }

    fn missing(&self) -> Error {
        self.fail(
            Kind::Unavailable,
            format!(
                "Google's Workspace CLI isn't installed (no `{}` on PATH); {INSTALL_HINT}",
                self.command
            ),
        )
    }

    fn gws(&self) -> Command {
        self.gws_in(&self.dir)
    }

    /// `gws` confined to a gws directory: this account's, or another Cloud
    /// app's to read its sign-in.
    fn gws_in(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.command);
        // gws also reads a .env from its working directory.
        cmd.current_dir(if dir.is_dir() {
            dir.to_path_buf()
        } else {
            std::env::temp_dir()
        })
        .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", dir)
        .env("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND", "file")
        .env(
            "GOOGLE_APPLICATION_CREDENTIALS",
            dir.join("no-application-default-credentials.json"),
        )
        .env_remove("GOOGLE_WORKSPACE_CLI_TOKEN")
        .env_remove("GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE");
        if let Some((id, secret)) = &self.client {
            cmd.env("GOOGLE_WORKSPACE_CLI_CLIENT_ID", id)
                .env("GOOGLE_WORKSPACE_CLI_CLIENT_SECRET", secret);
        }
        cmd
    }

    /// The sign-in gws keeps in `dir`, as `gws auth status` reports it after
    /// asking Google: its address and the permissions it has. None when
    /// there is none or Google no longer takes it.
    fn sign_in_at(&self, dir: &Path) -> Option<(String, Vec<String>)> {
        if !dir.join("credentials.enc").is_file() || !dir.join(KEY_FILE).is_file() {
            return None;
        }
        let Run::Done { status, stdout, .. } = run_command(self.gws_in(dir).args(["auth", "status"]), TIMEOUT) else {
            return None;
        };
        let stdout = String::from_utf8_lossy(&stdout);
        let v: Value = serde_json::from_str(&stdout[stdout.find('{')?..]).ok()?;
        if !status.success() || v["token_valid"] != true {
            return None;
        }
        let scopes = v["scopes"]
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        nonblank(v["user"].as_str().map(str::to_string)).map(|user| (user.to_lowercase(), scopes))
    }

    /// Whether this account's own sign-in works and covers what Cloud
    /// Contacts uses.
    pub fn works(&self) -> bool {
        self.sign_in_at(&self.dir).is_some_and(|(_, scopes)| covers(&scopes))
    }

    /// Takes over the Google sign-in another Cloud app has on this computer,
    /// in place of a browser sign-in: when the sign-ins there that work,
    /// cover [`SCOPES`] and aren't of an address another account here is
    /// signed in to are all of one address, gws's two files are copied from
    /// the first into this account's directory. Returns that address and the
    /// app's name. Only with the built-in client, which the three apps share,
    /// and never into a gws directory named in the config (the person's own).
    pub fn adopt(&self) -> Option<(String, &'static str)> {
        if self.client.as_ref().map(|(id, _)| id.as_str()) != GOOGLE_CLIENT_ID || self.dir != gws_dir(&self.name) {
            return None;
        }
        let accounts = |gws: PathBuf| {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(gws)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect();
            dirs.sort();
            dirs
        };
        let config = crate::config::config_dir();
        let mut found: Vec<(String, PathBuf, &'static str)> = Vec::new();
        for (folder, app) in SIBLINGS {
            for dir in accounts(config.parent()?.join(folder).join("gws")) {
                if let Some((address, scopes)) = self.sign_in_at(&dir)
                    && covers(&scopes)
                {
                    found.push((address, dir, app));
                }
            }
        }
        if found.is_empty() {
            return None;
        }
        let here: Vec<String> = accounts(config.join("gws"))
            .iter()
            .filter(|dir| **dir != self.dir)
            .filter_map(|dir| self.sign_in_at(dir).map(|(address, _)| address))
            .collect();
        found.retain(|(address, ..)| !here.contains(address));
        let (address, from, app) = found.first()?.clone();
        if found.iter().any(|(other, ..)| *other != address) {
            return None;
        }
        crate::config::private_dir(&self.dir).ok()?;
        // The key first: a sign-in that can't be read is "not signed in", never another's.
        let _ = std::fs::remove_file(self.dir.join("credentials.enc"));
        for file in [KEY_FILE, "credentials.enc"] {
            let tmp = self.dir.join(format!("{file}.adopting"));
            let copied = std::fs::copy(from.join(file), &tmp).and_then(|_| std::fs::rename(&tmp, self.dir.join(file)));
            if copied.is_err() {
                let _ = std::fs::remove_file(&tmp);
                let _ = std::fs::remove_file(self.dir.join("credentials.enc"));
                return None;
            }
        }
        // Access tokens gws cached for the sign-in that was here are not this one's.
        let _ = std::fs::remove_file(self.dir.join("token_cache.json"));
        Some((address, app))
    }

    /// Google's browser sign-in through `gws auth login`: gws prints a link,
    /// opened here in the browser, and waits for Google to send the browser
    /// back.
    pub fn login(&self) -> Result<()> {
        crate::config::private_dir(&self.dir).map_err(|e| {
            self.fail(
                Kind::Unavailable,
                format!("could not create {}: {e}", self.dir.display()),
            )
        })?;
        let mut cmd = self.gws();
        cmd.current_dir(&self.dir)
            .args(["auth", "login", "--scopes", SIGN_IN_SCOPES])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                self.missing()
            } else {
                self.fail(Kind::Unavailable, e.to_string())
            }
        })?;
        let opened = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watch = |pipe: Box<dyn Read + Send>| {
            let opened = opened.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut all = String::new();
                for line in std::io::BufReader::new(pipe).lines().map_while(std::result::Result::ok) {
                    if let Some(url) = line
                        .split_whitespace()
                        .find(|w| w.starts_with("https://accounts.google.com/"))
                    {
                        if !opened.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            open_browser(url);
                        }
                        eprintln!("{line}");
                    }
                    all.push_str(&line);
                    all.push('\n');
                }
                all
            })
        };
        let out = child.stdout.take().map(|p| watch(Box::new(p)));
        let err = child.stderr.take().map(|p| watch(Box::new(p)));
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(self.fail(Kind::Auth, "the Google sign-in wasn't finished within 10 minutes"));
                }
                Err(e) => return Err(self.fail(Kind::Unavailable, e.to_string())),
            }
        };
        let _ = out.and_then(|h| h.join().ok());
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        if !status.success() || !self.signed_in() {
            let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
            return Err(self.fail(
                Kind::Auth,
                format!("the Google sign-in didn't finish ({status}) {last}"),
            ));
        }
        Ok(())
    }

    /// Signs out on this computer: removes Cloud Contacts' gws directory for
    /// the account (only the default one: a directory named in the config
    /// was the person's own).
    pub fn forget(&self) -> std::io::Result<bool> {
        if self.dir != gws_dir(&self.name) {
            return Ok(false);
        }
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `gws people people <method...> --params <params> [--json <body>]`.
    fn call(&self, method: &[&str], params: Value, body: Option<&Value>) -> Result<Value> {
        self.call_in("people", method, params, body)
    }

    /// `gws people <resource> <method...> ...`: `people` or `contactGroups`.
    fn call_in(&self, resource: &str, method: &[&str], params: Value, body: Option<&Value>) -> Result<Value> {
        if !self.signed_in() {
            return Err(self.fail(
                Kind::Auth,
                format!("not signed in; run `cloud-contacts account login {}`", self.name),
            ));
        }
        let what = format!("{resource} {}", method.join(" "));
        let mut cmd = self.gws();
        cmd.args(["people", resource])
            .args(method)
            .arg("--params")
            .arg(params.to_string());
        if let Some(b) = body {
            cmd.arg("--json").arg(b.to_string());
        }
        let (status, stdout, stderr) = match run_command(&mut cmd, TIMEOUT) {
            Run::Done { status, stdout, stderr } => (status, stdout, stderr),
            Run::Missing => return Err(self.missing()),
            Run::TimedOut => {
                return Err(self.fail(
                    Kind::Unavailable,
                    format!("`gws people {what}` took longer than {}s", TIMEOUT.as_secs()),
                ));
            }
            Run::Failed(e) => {
                return Err(self.fail(Kind::Unavailable, format!("could not run {}: {e}", self.command)));
            }
        };
        let parsed: std::result::Result<Value, _> = serde_json::from_slice(&stdout);
        if status.success() {
            if String::from_utf8_lossy(&stdout).trim().is_empty() {
                return Ok(Value::Null);
            }
            return parsed.map_err(|e| {
                self.fail(
                    Kind::Unavailable,
                    format!("`gws people {what}` answered something that isn't JSON ({e})"),
                )
            });
        }
        Err(self.gws_error(status.code(), &parsed.unwrap_or(Value::Null), &stderr, &what))
    }

    /// gws's exit statuses: 1 API error (Google's HTTP status in the JSON),
    /// 2 auth, 3 validation, 4 discovery (Google unreachable), 5 other; the
    /// error JSON goes to stdout.
    fn gws_error(&self, code: Option<i32>, out: &Value, stderr: &str, what: &str) -> Error {
        let err = &out["error"];
        let mut message = err["message"].as_str().unwrap_or_default().trim().to_string();
        if message.is_empty() {
            message = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
        }
        if message.is_empty() {
            message = format!("`gws people {what}` failed");
        }
        let message: String = message.lines().next().unwrap_or("").chars().take(300).collect();
        let again = format!("run `cloud-contacts account login {}` to sign in again", self.name);
        let status = err["status"].as_str().unwrap_or_default();
        match (code, err["code"].as_i64().unwrap_or(0)) {
            (Some(2), _) | (Some(1), 401) => self.fail(
                Kind::Auth,
                format!("the Google sign-in expired or was revoked ({message}); {again}"),
            ),
            (Some(1), 403) if message.to_ascii_lowercase().contains("insufficient") => {
                self.fail(Kind::Auth, format!("not allowed ({message}); {again}"))
            }
            (Some(1), 400) if status == "FAILED_PRECONDITION" || message.to_ascii_lowercase().contains("etag") => {
                self.fail(Kind::Conflict, message)
            }
            (Some(1), 404 | 410) => self.fail(Kind::NotFound, message),
            (Some(1), 400 | 403) => self.fail(Kind::BadRequest, message),
            (Some(1), 429) => self.fail(
                Kind::Unavailable,
                format!("Google is rate limiting ({message}); try again shortly"),
            ),
            (Some(4), _) => self.fail(Kind::Unavailable, format!("couldn't reach Google ({message})")),
            (Some(3), _) => self.fail(
                Kind::Unavailable,
                format!("gws rejected `{what}` ({message}); is gws up to date?"),
            ),
            _ => self.fail(Kind::Unavailable, message),
        }
    }

    /// Every contact of the account (`connections`), as Google has them.
    fn people(&self) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..MAX_PAGES {
            let mut params = json!({
                "resourceName": "people/me",
                "personFields": PERSON_FIELDS,
                "pageSize": 1000,
            });
            if let Some(t) = &token {
                params["pageToken"] = json!(t);
            }
            let page = self.call(&["connections", "list"], params, None)?;
            out.extend(page["connections"].as_array().cloned().unwrap_or_default());
            token = nonblank(page["nextPageToken"].as_str().map(str::to_string));
            match &token {
                None => return Ok(out),
                Some(t) if !seen.insert(t.clone()) => {
                    return Err(self.fail(Kind::Unavailable, "Google repeated a page of contacts"));
                }
                Some(_) => {}
            }
        }
        Err(self.fail(Kind::Unavailable, "more contacts than this app reads (100,000)"))
    }

    /// One contact as Google has it now; `None` when it is gone.
    fn person(&self, id: &str) -> Result<Option<Value>> {
        match self.call(
            &["get"],
            json!({ "resourceName": id, "personFields": PERSON_FIELDS }),
            None,
        ) {
            Ok(p) => Ok(Some(p)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn card(&self, person: &Value) -> Result<Contact> {
        card_of(person).ok_or_else(|| self.fail(Kind::Unavailable, "Google sent a contact without its resourceName"))
    }

    /// Folds a write's result into the cache. The write happened, so a
    /// failure here is a warning, not an error.
    fn record(&self, change: impl FnOnce(&mut Cache)) -> Option<String> {
        let save = || -> std::io::Result<()> {
            let _lock = self.store.lock()?;
            let mut cache = self.store.cache()?;
            change(&mut cache);
            self.store.save_cache(&mut cache)
        };
        save()
            .err()
            .map(|e| format!("saved in Google; local cache not updated: {e}"))
    }

    fn put(&self, person: &Value) -> Result<Saved> {
        let value = self.card(person)?;
        let warning = self.record(|cache| cache.put(value.clone()));
        Ok(Saved { value, warning })
    }

    fn write(&self, base: &Contact, changes: &[Change]) -> Result<Value> {
        let mut card = base.clone();
        card.apply(changes).map_err(Error::Other)?;
        self.call(
            &["updateContact"],
            json!({
                "resourceName": base.id(),
                "updatePersonFields": UPDATE_FIELDS,
                "personFields": PERSON_FIELDS,
            }),
            Some(&person_of(&card)),
        )
    }
}

impl Provider for Google {
    fn cache(&self) -> Result<Cache> {
        Ok(self.store.cache()?)
    }

    fn sync(&self) -> Result<(SyncReport, Cache)> {
        let people = self.people()?;
        let contacts = people.iter().map(|p| self.card(p)).collect::<Result<Vec<_>>>()?;
        let _lock = self.store.lock()?;
        let before = self.store.cache()?;
        let mut cache = Cache {
            contacts,
            ..Cache::default()
        };
        cache.sort();
        let changed = changes(&before, &cache);
        self.store.save_cache(&mut cache)?;
        let report = SyncReport {
            contacts: cache.contacts.len(),
            changed,
        };
        Ok((report, cache))
    }

    fn add(&self, changes: &[Change], is_company: bool) -> Result<Saved> {
        let mut card = Contact::new(is_company);
        card.apply(changes).map_err(Error::Other)?;
        let person = self.call(
            &["createContact"],
            json!({ "personFields": PERSON_FIELDS }),
            Some(&person_of(&card)),
        )?;
        self.put(&person)
    }

    /// As iCloud's: when the contact changed elsewhere since it was read
    /// (a stale etag), the changes are applied once more to a fresh copy;
    /// those it already has are dropped, and with none left nothing is
    /// written.
    fn update(&self, c: &Contact, changes: &[Change]) -> Result<Saved> {
        let person = match self.write(c, changes) {
            Err(e) if e.is_conflict() => {
                let fresh = self
                    .person(c.id())?
                    .ok_or_else(|| Error::Other(format!("\"{}\" has been deleted", c.display_name())))?;
                let fresh = self.card(&fresh)?;
                let mut card = fresh.clone();
                let mut left = Vec::new();
                for ch in changes {
                    if !card.has(ch) {
                        card.apply(std::slice::from_ref(ch)).map_err(Error::Other)?;
                        left.push(ch.clone());
                    }
                }
                if left.is_empty() {
                    let warning = self.record(|cache| cache.put(fresh.clone()));
                    return Ok(Saved { value: fresh, warning });
                }
                self.write(&fresh, &left)?
            }
            other => other?,
        };
        self.put(&person)
    }

    /// The kept contact joins every contact group the merged ones were in
    /// (a card does not carry groups, and a deleted contact leaves its own).
    fn carry_over(&self, kept: &Contact, others: &[&Contact]) -> Result<Option<String>> {
        let has = groups(kept);
        let mut wanted: Vec<String> = others
            .iter()
            .flat_map(|c| groups(c))
            .filter(|g| !has.contains(g))
            .collect();
        wanted.sort();
        wanted.dedup();
        for group in wanted {
            self.call_in(
                "contactGroups",
                &["members", "modify"],
                json!({ "resourceName": group }),
                Some(&json!({ "resourceNamesToAdd": [kept.id()] })),
            )?;
        }
        Ok(None)
    }

    fn delete(&self, c: &Contact) -> Result<Option<String>> {
        match self.call(&["deleteContact"], json!({ "resourceName": c.id() }), None) {
            Ok(_) => {}
            // already gone: deleted, as asked
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e),
        }
        Ok(self.record(|cache| cache.remove(c.id())))
    }
}

fn open_browser(url: &str) {
    let program = nonblank(std::env::var(BROWSER_ENV).ok()).unwrap_or_else(|| "xdg-open".into());
    let _ = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// How a command-line tool ended.
enum Run {
    Done {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: String,
    },
    Missing,
    TimedOut,
    Failed(std::io::Error),
}

/// Runs a prepared command (stdout and stderr piped), killing it after `timeout`.
fn run_command(cmd: &mut Command, timeout: Duration) -> Run {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Run::Missing,
        Err(e) => return Run::Failed(e),
    };
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Run::TimedOut;
            }
            Err(e) => return Run::Failed(e),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default())
        .trim()
        .to_string();
    Run::Done { status, stdout, stderr }
}

// ---- Google's people and the cards this app reads ----

/// Google's types and the labels iCloud's cards (and so this app) use.
const PHONE_TYPES: &[(&str, &str)] = &[
    ("mobile", "MOBILE"),
    ("home", "HOME"),
    ("work", "WORK"),
    ("main", "MAIN"),
    ("homeFax", "HOME FAX"),
    ("workFax", "WORK FAX"),
    ("pager", "PAGER"),
    ("other", "OTHER"),
];
const EMAIL_TYPES: &[(&str, &str)] = &[("home", "HOME"), ("work", "WORK"), ("other", "OTHER")];
const URL_TYPES: &[(&str, &str)] = &[
    ("homePage", "HOMEPAGE"),
    ("home", "HOME"),
    ("work", "WORK"),
    ("other", "OTHER"),
];

/// A Google type as a card's label: the standard ones as iCloud writes them,
/// a custom one as typed.
fn label_of(t: &str, table: &[(&str, &str)]) -> String {
    table
        .iter()
        .find(|(g, _)| *g == t)
        .map_or_else(|| t.to_string(), |(_, l)| l.to_string())
}

/// A card's label as a Google type (a custom label is a custom type).
fn type_of(label: &str, table: &[(&str, &str)]) -> String {
    table
        .iter()
        .find(|(_, l)| l.eq_ignore_ascii_case(label))
        .map_or_else(|| label.to_string(), |(g, _)| g.to_string())
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}

/// The entry marked primary, else the first.
fn primary(list: &Value) -> Option<&Value> {
    let list = list.as_array()?;
    list.iter().find(|e| e["metadata"]["primary"] == true).or(list.first())
}

fn put(card: &mut Map<String, Value>, k: &str, v: &str) {
    if !v.trim().is_empty() {
        card.insert(k.into(), json!(v));
    }
}

fn birthday_card(date: &Value) -> Option<String> {
    let (m, d) = (date["month"].as_u64()?, date["day"].as_u64()?);
    let y = date["year"].as_u64().filter(|y| *y > 0).unwrap_or(1604);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn birthday_date(b: &str) -> Option<Value> {
    let mut parts = b.split('-').map(|p| p.parse::<u64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    Some(if y == 1604 {
        json!({ "month": m, "day": d })
    } else {
        json!({ "year": y, "month": m, "day": d })
    })
}

fn labeled(list: &Value, table: &[(&str, &str)]) -> Value {
    json!(
        list.as_array()
            .into_iter()
            .flatten()
            .filter(|e| !s(e, "value").trim().is_empty())
            .map(|e| {
                let mut m = Map::new();
                let t = s(e, "type");
                if !t.is_empty() {
                    m.insert("label".into(), json!(label_of(t, table)));
                }
                m.insert("field".into(), json!(s(e, "value")));
                Value::Object(m)
            })
            .collect::<Vec<_>>()
    )
}

/// A Google person as a card (see the module's header).
pub fn card_of(p: &Value) -> Option<Contact> {
    let id = s(p, "resourceName");
    if id.is_empty() {
        return None;
    }
    let mut c = Map::new();
    c.insert("contactId".into(), json!(id));
    put(&mut c, "etag", s(p, "etag"));
    if let Some(n) = primary(&p["names"]) {
        put(&mut c, "firstName", s(n, "givenName"));
        put(&mut c, "middleName", s(n, "middleName"));
        put(&mut c, "lastName", s(n, "familyName"));
        put(&mut c, "prefix", s(n, "honorificPrefix"));
        put(&mut c, "suffix", s(n, "honorificSuffix"));
        // a name typed whole, which Google did not split
        if ["givenName", "middleName", "familyName"]
            .iter()
            .all(|k| s(n, k).is_empty())
        {
            put(&mut c, "firstName", s(n, "unstructuredName"));
        }
    }
    if let Some(n) = primary(&p["nicknames"]) {
        put(&mut c, "nickName", s(n, "value"));
    }
    if let Some(o) = primary(&p["organizations"]) {
        put(&mut c, "companyName", s(o, "name"));
        put(&mut c, "jobTitle", s(o, "title"));
        put(&mut c, "department", s(o, "department"));
    }
    if let Some(b) = primary(&p["biographies"]) {
        put(&mut c, "notes", s(b, "value"));
    }
    if let Some(b) = primary(&p["birthdays"]).and_then(|b| birthday_card(&b["date"])) {
        c.insert("birthday".into(), json!(b));
    }
    // Google has no "show as a company" flag (a People API person is names, organizations and the rest): a contact
    // with no name is the company it names, as Google's own Contacts shows it, unless this app marked it
    // (`COMPANY_KEY`): a company's card that names a person, as iCloud has them, or a card with no name that is not
    // a company's.
    let named = ["firstName", "middleName", "lastName", "prefix", "suffix", "nickName"]
        .iter()
        .any(|k| c.contains_key(*k));
    let marked = originals(p, "clientData").into_iter().find_map(company_mark);
    c.insert(
        "isCompany".into(),
        json!(c.contains_key("companyName") && marked.unwrap_or(!named)),
    );
    for (from, to, table) in [
        ("phoneNumbers", "phones", PHONE_TYPES),
        ("emailAddresses", "emailAddresses", EMAIL_TYPES),
        ("urls", "urls", URL_TYPES),
    ] {
        let list = labeled(&p[from], table);
        if list.as_array().is_some_and(|l| !l.is_empty()) {
            c.insert(to.into(), list);
        }
    }
    let addresses: Vec<Value> = p["addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let mut e = Map::new();
            let t = s(a, "type");
            if !t.is_empty() {
                e.insert("label".into(), json!(label_of(t, EMAIL_TYPES)));
            }
            e.insert("field".into(), address_field(a));
            Value::Object(e)
        })
        .collect();
    if !addresses.is_empty() {
        c.insert("streetAddresses".into(), json!(addresses));
    }
    if let Some(photo) = primary(&p["photos"]).filter(|ph| ph["default"] != true && ph["url"].is_string()) {
        c.insert("photo".into(), json!({ "url": photo["url"] }));
    }
    c.insert(ORIGINAL.into(), p.clone());
    Some(Contact(c))
}

/// A Google address as a card's street address field.
fn address_field(a: &Value) -> Value {
    let mut street = s(a, "streetAddress").to_string();
    if !s(a, "extendedAddress").is_empty() {
        street = format!("{street}\n{}", s(a, "extendedAddress")).trim().to_string();
    }
    let street = if street.is_empty() && s(a, "city").is_empty() {
        // only a formatted value: shown as the street
        s(a, "formattedValue").to_string()
    } else {
        street
    };
    let mut f = Map::new();
    for (k, v) in [
        ("street", street.as_str()),
        ("city", s(a, "city")),
        ("state", s(a, "region")),
        ("postalCode", s(a, "postalCode")),
        ("country", s(a, "country")),
        ("countryCode", s(a, "countryCode")),
    ] {
        if !v.is_empty() {
            f.insert(k.into(), json!(v));
        }
    }
    Value::Object(f)
}

/// The list `key` of Google's record of a card's contact.
/// What an entry of a person's client data says of the card being a company's, where it is this app's mark.
fn company_mark(entry: &Value) -> Option<bool> {
    (s(entry, "key") == COMPANY_KEY).then(|| s(entry, "value") == "true")
}

fn originals<'a>(orig: &'a Value, key: &str) -> Vec<&'a Value> {
    orig[key].as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

/// The contact groups a Google contact is in, from Google's record of it.
fn groups(c: &Contact) -> Vec<String> {
    c.0.get(ORIGINAL)
        .and_then(|p| p["memberships"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| m["contactGroupMembership"]["contactGroupResourceName"].as_str())
        .map(str::to_owned)
        .collect()
}

/// A card as the Google person to write, on top of Google's record of it
/// (none for a new contact): each entry Google had whose value the card
/// still holds is sent back as it was, metadata and all.
pub fn person_of(card: &Contact) -> Value {
    let orig = card.0.get(ORIGINAL).cloned().unwrap_or(Value::Null);
    let mut p = Map::new();
    for k in ["resourceName", "etag"] {
        if let Some(v) = orig.get(k) {
            p.insert(k.into(), v.clone());
        }
    }
    let field = |k: &str| card.get(k).trim().to_string();

    // the name: Google's own entry, with the parts the card has
    let name_parts = [
        ("givenName", field("firstName")),
        ("middleName", field("middleName")),
        ("familyName", field("lastName")),
        ("honorificPrefix", field("prefix")),
        ("honorificSuffix", field("suffix")),
    ];
    if name_parts.iter().any(|(_, v)| !v.is_empty()) {
        let mut n = primary(&orig["names"])
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for k in ["displayName", "displayNameLastFirst", "unstructuredName"] {
            n.remove(k);
        }
        for (k, v) in name_parts {
            if v.is_empty() {
                n.remove(k);
            } else {
                n.insert(k.into(), json!(v));
            }
        }
        p.insert("names".into(), json!([n]));
    }
    let nick = field("nickName");
    if !nick.is_empty() {
        let mut n = primary(&orig["nicknames"])
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        n.insert("value".into(), json!(nick));
        p.insert("nicknames".into(), json!([n]));
    }
    // the first organization from the card, the others as Google has them
    let org_parts = [
        ("name", field("companyName")),
        ("title", field("jobTitle")),
        ("department", field("department")),
    ];
    let first_org = primary(&orig["organizations"]);
    let mut orgs: Vec<Value> = originals(&orig, "organizations")
        .into_iter()
        .filter(|o| Some(*o) != first_org)
        .cloned()
        .collect();
    if org_parts.iter().any(|(_, v)| !v.is_empty()) {
        let mut o = first_org.and_then(Value::as_object).cloned().unwrap_or_default();
        for (k, v) in org_parts {
            if v.is_empty() {
                o.remove(k);
            } else {
                o.insert(k.into(), json!(v));
            }
        }
        orgs.insert(0, Value::Object(o));
    }
    if !orgs.is_empty() {
        p.insert("organizations".into(), json!(orgs));
    }
    // other apps' client data as it was, and this app's mark on a company
    let mut client: Vec<Value> = originals(&orig, "clientData")
        .into_iter()
        .filter(|d| company_mark(d).is_none())
        .cloned()
        .collect();
    // marked where the name alone would say otherwise, and a company's card always
    let named = ["firstName", "middleName", "lastName", "prefix", "suffix", "nickName"]
        .iter()
        .any(|k| !field(k).is_empty());
    let unnamed_company = !named && !field("companyName").is_empty();
    if card.is_company() || unnamed_company {
        client.push(json!({ "key": COMPANY_KEY, "value": card.is_company().to_string() }));
    }
    if !client.is_empty() {
        p.insert("clientData".into(), json!(client));
    }
    let notes = card.get("notes");
    if !notes.trim().is_empty() {
        p.insert(
            "biographies".into(),
            json!([{ "value": notes, "contentType": "TEXT_PLAIN" }]),
        );
    }
    if let Some(date) = birthday_date(card.get("birthday")) {
        let kept = primary(&orig["birthdays"]).filter(|b| b["date"] == date).cloned();
        p.insert(
            "birthdays".into(),
            json!([kept.unwrap_or_else(|| json!({ "date": date }))]),
        );
    }
    for (from, to, table) in [
        ("phones", "phoneNumbers", PHONE_TYPES),
        ("emailAddresses", "emailAddresses", EMAIL_TYPES),
        ("urls", "urls", URL_TYPES),
    ] {
        let mut unused = originals(&orig, to);
        let list: Vec<Value> = card
            .0
            .get(from)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let value = e["field"].as_str()?.trim();
                let label = e["label"].as_str().unwrap_or_default();
                let same = |o: &&Value| {
                    s(o, "value").trim() == value && label_of(s(o, "type"), table).eq_ignore_ascii_case(label)
                };
                if let Some(i) = unused.iter().position(same) {
                    return Some(unused.remove(i).clone());
                }
                let mut m = Map::new();
                m.insert("value".into(), json!(value));
                if !label.is_empty() {
                    m.insert("type".into(), json!(type_of(label, table)));
                }
                Some(Value::Object(m))
            })
            .collect();
        if !list.is_empty() {
            p.insert(to.into(), json!(list));
        }
    }
    let mut unused = originals(&orig, "addresses");
    let addresses: Vec<Value> = card
        .0
        .get("streetAddresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|e| {
            let label = e["label"].as_str().unwrap_or_default();
            let same = |o: &&Value| {
                address_field(o) == e["field"] && label_of(s(o, "type"), EMAIL_TYPES).eq_ignore_ascii_case(label)
            };
            if let Some(i) = unused.iter().position(same) {
                return unused.remove(i).clone();
            }
            let f = &e["field"];
            let mut m = Map::new();
            for (k, from) in [
                ("streetAddress", "street"),
                ("city", "city"),
                ("region", "state"),
                ("postalCode", "postalCode"),
                ("country", "country"),
                ("countryCode", "countryCode"),
            ] {
                if !s(f, from).is_empty() {
                    m.insert(k.into(), f[from].clone());
                }
            }
            if !label.is_empty() {
                m.insert("type".into(), json!(type_of(label, EMAIL_TYPES)));
            }
            Value::Object(m)
        })
        .collect();
    if !addresses.is_empty() {
        p.insert("addresses".into(), json!(addresses));
    }
    Value::Object(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Field, Multi, parse_labeled};

    fn person() -> Value {
        json!({
            "resourceName": "people/c1",
            "etag": "E1",
            "names": [{"metadata": {"primary": true}, "displayName": "Ann Lee", "givenName": "Ann", "familyName": "Lee"}],
            "phoneNumbers": [
                {"metadata": {"primary": true, "source": {"id": "x"}}, "value": "+1 555 0100", "type": "mobile", "formattedType": "Mobile"},
                {"value": "555 0199", "type": "Boat"}
            ],
            "emailAddresses": [{"value": "ann@example.com", "type": "work"}],
            "organizations": [{"name": "Acme", "title": "CEO"}, {"name": "Old Co"}],
            "birthdays": [{"date": {"month": 2, "day": 29}, "text": "Feb 29"}],
            "addresses": [{"type": "home", "streetAddress": "1 Main St", "city": "Springfield", "region": "IL"}],
            "photos": [{"url": "https://p/default", "default": true}],
            "metadata": {"sources": [{"type": "CONTACT"}]},
        })
    }

    #[test]
    fn a_person_reads_as_a_card() {
        let c = card_of(&person()).unwrap();
        assert_eq!(c.id(), "people/c1");
        assert_eq!(c.etag(), "E1");
        assert_eq!(c.display_name(), "Ann Lee");
        assert_eq!(c.field(Field::Company), "Acme");
        assert_eq!(c.field(Field::Birthday), "1604-02-29");
        let phones = c.values(Multi::Phone);
        assert_eq!((phones[0].label.as_str(), phones[1].label.as_str()), ("MOBILE", "Boat"));
        assert_eq!(c.values(Multi::Email)[0].label, "WORK");
        assert_eq!(c.addresses()[0].state, "IL");
        assert!(!c.has_photo(), "Google's default picture is no photo");
        assert!(!c.is_company(), "a person at a company");
        assert!(card_of(&json!({})).is_none());
    }

    #[test]
    fn a_company_with_a_persons_name_stays_one_through_google() {
        let mut card = Contact::new(true);
        card.apply(&[
            Change::Set(Field::First, "Bo".into()),
            Change::Set(Field::Company, "Acme".into()),
        ])
        .unwrap();
        let mut written = person_of(&card);
        assert_eq!(written["clientData"], json!([{"key": COMPANY_KEY, "value": "true"}]));
        written["resourceName"] = json!("people/c9");
        // another app's data beside it comes back as it was
        written["clientData"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"key": "other", "value": "1"}));
        let read = card_of(&written).unwrap();
        assert!(read.is_company());
        assert_eq!(read.display_name(), "Acme");
        assert_eq!(
            person_of(&read)["clientData"],
            json!([{"key": "other", "value": "1"}, {"key": COMPANY_KEY, "value": "true"}])
        );
        // a person writes no mark
        let mut person = Contact::new(false);
        person.apply(&[Change::Set(Field::First, "Bo".into())]).unwrap();
        assert!(person_of(&person).get("clientData").is_none());
        // made a person's card again, with no name: marked so, or its company alone would read as a company's
        let mut unnamed = Contact::new(true);
        unnamed.apply(&[Change::Set(Field::Company, "Acme".into())]).unwrap();
        unnamed.apply(&[Change::Company(false)]).unwrap();
        let mut written = person_of(&unnamed);
        assert_eq!(written["clientData"], json!([{"key": COMPANY_KEY, "value": "false"}]));
        written["resourceName"] = json!("people/c10");
        assert!(!card_of(&written).unwrap().is_company());
    }

    #[test]
    fn a_company_with_no_name_reads_as_a_company() {
        let company = json!({"resourceName": "people/c2", "organizations": [{"name": "Acme Hardware"}]});
        let c = card_of(&company).unwrap();
        assert!(c.is_company());
        assert_eq!(c.display_name(), "Acme Hardware");
        assert_eq!(c.work(" · "), "", "the company is the name, not a line under it");
        // a nickname is what such a contact is shown by, with its company under it
        let mut nicknamed = company.clone();
        nicknamed["nicknames"] = json!([{"value": "The Shop"}]);
        let c = card_of(&nicknamed).unwrap();
        assert!(!c.is_company());
        assert_eq!(
            (c.display_name().as_str(), c.work(" · ").as_str()),
            ("The Shop", "Acme Hardware")
        );
    }

    #[test]
    fn an_untouched_card_writes_back_what_google_had() {
        let p = person_of(&card_of(&person()).unwrap());
        assert_eq!(p["etag"], "E1");
        assert_eq!(p["phoneNumbers"], person()["phoneNumbers"]);
        assert_eq!(p["emailAddresses"], person()["emailAddresses"]);
        assert_eq!(p["organizations"], person()["organizations"]);
        assert_eq!(p["birthdays"], person()["birthdays"]);
        assert_eq!(p["addresses"], person()["addresses"]);
        // derived fields Google computes are not sent
        assert!(p["names"][0].get("displayName").is_none());
        assert_eq!(p["names"][0]["metadata"]["primary"], true);
    }

    #[test]
    fn an_edit_changes_only_what_it_names() {
        let mut c = card_of(&person()).unwrap();
        c.apply(&[
            Change::Set(Field::Last, "Lee-Smith".into()),
            Change::Set(Field::Company, String::new()),
            Change::Set(Field::JobTitle, String::new()),
            Change::Remove(Multi::Phone, "555 0199".into()),
            Change::Add(Multi::Email, parse_labeled("ann@home.example", "HOME")),
            Change::Set(Field::Birthday, "1990-05-01".into()),
        ])
        .unwrap();
        let p = person_of(&c);
        assert_eq!(p["names"][0]["familyName"], "Lee-Smith");
        assert_eq!(p["names"][0]["givenName"], "Ann");
        // the kept number keeps Google's metadata
        assert_eq!(p["phoneNumbers"].as_array().unwrap().len(), 1);
        assert_eq!(p["phoneNumbers"][0]["metadata"]["source"]["id"], "x");
        assert_eq!(
            p["emailAddresses"][1],
            json!({"value": "ann@home.example", "type": "home"})
        );
        // the first organization emptied; the second stays
        assert_eq!(p["organizations"], json!([{"name": "Old Co"}]));
        assert_eq!(p["birthdays"], json!([{"date": {"year": 1990, "month": 5, "day": 1}}]));
    }

    #[test]
    fn a_new_contact_has_no_google_record() {
        let mut c = Contact::new(false);
        c.apply(&[
            Change::Set(Field::First, "Bo".into()),
            Change::Add(Multi::Phone, parse_labeled("work=+1 555", "MOBILE")),
            Change::Add(Multi::Url, parse_labeled("blog=https://bo.example", "HOMEPAGE")),
        ])
        .unwrap();
        let p = person_of(&c);
        assert!(p.get("resourceName").is_none() && p.get("etag").is_none());
        assert_eq!(p["names"], json!([{"givenName": "Bo"}]));
        assert_eq!(p["phoneNumbers"], json!([{"value": "+1 555", "type": "work"}]));
        assert_eq!(p["urls"], json!([{"value": "https://bo.example", "type": "blog"}]));
    }
}
