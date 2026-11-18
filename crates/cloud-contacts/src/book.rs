//! The address book: every linked account's contacts, side by side. Each
//! contact belongs to one account and its ID says which
//! (`icloud:2B7E…`, `google:people/c123`); a change goes to that account
//! alone. A read syncs every account at once, and one that fails is a
//! warning beside the others' contacts, not an error, unless every account
//! failed.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::account::{Provider, Saved, SyncReport};
use crate::config::{AccountConfig, Config};
use crate::error::{Error, Result};
use crate::google::Google;
use crate::icloud::ICloud;
use crate::icloud::api::SessionTransport;
use crate::model::{Change, Contact};
use crate::store::{Cache, Store};

/// One linked account, opened.
pub struct Account {
    pub name: String,
    /// "icloud" or "google".
    pub provider: String,
    pub provider_impl: Arc<dyn Provider>,
}

impl Account {
    /// "iCloud", "Google".
    pub fn label(&self) -> &'static str {
        label(&self.provider)
    }
}

pub fn label(provider: &str) -> &'static str {
    match provider {
        "icloud" => "iCloud",
        "google" => "Google",
        _ => "Unknown",
    }
}

/// A contact with the account it is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub account: String,
    pub provider: String,
    pub contact: Contact,
    /// iCloud's "me" card.
    pub me: bool,
}

impl Entry {
    /// The ID people and scripts use: `<account>:<the account's own id>`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.account, self.contact.id())
    }

    pub fn label(&self) -> &'static str {
        label(&self.provider)
    }
}

/// An account that failed while the others answered.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Warning {
    pub account: String,
    pub code: String,
    pub message: String,
}

/// What a read found: the contacts of every account that answered, sorted
/// as Contacts sorts them, and the accounts that did not.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Shown {
    pub entries: Vec<Entry>,
    pub warnings: Vec<Warning>,
}

impl Shown {
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id() == id)
    }
}

/// One account's fetch: its report and cache, or its failure.
pub type Fetched<'a> = (&'a Account, Result<(SyncReport, Cache)>);

pub struct Book {
    pub accounts: Vec<Account>,
}

/// `(account, the account's own id)` from an ID.
pub fn split_id(id: &str) -> Option<(&str, &str)> {
    id.split_once(':').filter(|(a, rest)| !a.is_empty() && !rest.is_empty())
}

impl Book {
    /// Opens the accounts of `config`; each keeps its cache in `root/<name>`.
    pub fn open(config: &Config, root: PathBuf, transport: Arc<SessionTransport>) -> Result<Book> {
        let accounts = config
            .accounts
            .iter()
            .map(|(name, cfg)| open_account(name, cfg, root.join(name), transport.clone()))
            .collect::<Result<_>>()?;
        Ok(Book { accounts })
    }

    pub fn account(&self, name: &str) -> Result<&Account> {
        self.accounts.iter().find(|a| a.name == name).ok_or_else(|| {
            Error::Config(format!(
                "no account named {name} (linked: {})",
                self.accounts
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
    }

    /// The accounts `only` names (every one when `None`).
    fn chosen(&self, only: Option<&str>) -> Result<Vec<&Account>> {
        if self.accounts.is_empty() {
            return Err(Error::Config("no account is linked".into()));
        }
        Ok(match only {
            Some(name) => vec![self.account(name)?],
            None => self.accounts.iter().collect(),
        })
    }

    /// Syncs each chosen account at once; answers each one's result.
    pub fn sync(&self, only: Option<&str>) -> Result<Vec<Fetched<'_>>> {
        let chosen = self.chosen(only)?;
        Ok(std::thread::scope(|scope| {
            let running: Vec<_> = chosen
                .iter()
                .map(|a| (*a, scope.spawn(|| a.provider_impl.sync())))
                .collect();
            running
                .into_iter()
                .map(|(a, h)| {
                    let r = h
                        .join()
                        .unwrap_or_else(|_| Err(Error::Other(format!("{}: the sync crashed", a.name))));
                    (a, r)
                })
                .collect()
        }))
    }

    /// The contacts after a sync of every chosen account (or, `cached`, as
    /// the last syncs left them). Fails only when no account answered.
    pub fn read(&self, only: Option<&str>, cached: bool) -> Result<Shown> {
        let results: Vec<(&Account, Result<Cache>)> = if cached {
            self.chosen(only)?
                .into_iter()
                .map(|a| (a, a.provider_impl.cache()))
                .collect()
        } else {
            self.sync(only)?
                .into_iter()
                .map(|(a, r)| (a, r.map(|(_, c)| c)))
                .collect()
        };
        gather(results)
    }

    pub fn add(&self, account: &str, changes: &[Change], is_company: bool) -> Result<Saved> {
        self.account(account)?.provider_impl.add(changes, is_company)
    }

    pub fn update(&self, e: &Entry, changes: &[Change]) -> Result<Saved> {
        self.account(&e.account)?.provider_impl.update(&e.contact, changes)
    }

    pub fn delete(&self, e: &Entry) -> Result<Option<String>> {
        self.account(&e.account)?.provider_impl.delete(&e.contact)
    }
}

/// Each account's cache (or failure) as one sorted book. With every account
/// failing, the first failure is the answer.
pub fn gather(results: Vec<(&Account, Result<Cache>)>) -> Result<Shown> {
    let mut shown = Shown::default();
    let mut failures = Vec::new();
    let total = results.len();
    for (a, r) in results {
        match r {
            Ok(cache) => shown.entries.extend(cache.contacts.iter().map(|c| Entry {
                account: a.name.clone(),
                provider: a.provider.clone(),
                me: cache.is_me(c),
                contact: c.clone(),
            })),
            Err(e) => {
                shown.warnings.push(Warning {
                    account: a.name.clone(),
                    code: crate::cli::failure(&e).code,
                    message: e.to_string(),
                });
                failures.push(e);
            }
        }
    }
    if total > 0 && failures.len() == total {
        return Err(failures.remove(0));
    }
    sort(&mut shown.entries);
    Ok(shown)
}

/// As Contacts sorts them (last name, first name), then by account.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| (e.contact.sort_key(), e.account.clone()));
}

fn open_account(name: &str, cfg: &AccountConfig, dir: PathBuf, t: Arc<SessionTransport>) -> Result<Account> {
    let provider = cfg.provider(name).to_string();
    let store = Store::new(dir);
    let provider_impl: Arc<dyn Provider> = match provider.as_str() {
        "icloud" => Arc::new(ICloud::new(t, store)),
        "google" => Arc::new(Google::new(name, cfg, store)),
        other => {
            return Err(Error::Config(format!(
                "account {name}: unknown provider \"{other}\" (known: icloud, google)"
            )));
        }
    };
    Ok(Account {
        name: name.to_string(),
        provider,
        provider_impl,
    })
}
