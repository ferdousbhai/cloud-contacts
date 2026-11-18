//! What every kind of account does, the same way for iCloud and Google:
//! keep a cache of its contacts, sync it, and write a change straight to the
//! account. Accounts never sync with each other: a contact lives in one
//! account, and an edit goes to that account alone.

use std::collections::HashMap;

use crate::error::Result;
use crate::model::{Change, Contact};
use crate::store::Cache;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub contacts: usize,
    /// Cards added, changed or gone since the last sync.
    pub changed: usize,
}

/// The contact a write saved, and why the local cache may not show it.
#[derive(Debug)]
pub struct Saved {
    pub value: Contact,
    pub warning: Option<String>,
}

pub trait Provider: Send + Sync {
    /// The cache as last saved.
    fn cache(&self) -> Result<Cache>;
    /// Fetches every contact, saves them, and returns the new cache.
    fn sync(&self) -> Result<(SyncReport, Cache)>;
    /// Adds a contact made of `changes`; `is_company` shows it by its
    /// company name.
    fn add(&self, changes: &[Change], is_company: bool) -> Result<Saved>;
    /// Writes `changes` to the contact `c` (as cached), keeping what another
    /// device changed meanwhile that the changes do not touch.
    fn update(&self, c: &Contact, changes: &[Change]) -> Result<Saved>;
    /// Deletes the contact `c`; answers why the cache may still show it.
    fn delete(&self, c: &Contact) -> Result<Option<String>>;
}

/// Cards added, changed (another etag) or gone between two caches.
pub fn changes(before: &Cache, after: &Cache) -> usize {
    fn etags(c: &Cache) -> HashMap<&str, &str> {
        c.contacts.iter().map(|c| (c.id(), c.etag())).collect()
    }
    let (was, now) = (etags(before), etags(after));
    let changed_or_new = now.iter().filter(|(id, etag)| was.get(*id) != Some(*etag)).count();
    let gone = was.keys().filter(|id| !now.contains_key(*id)).count();
    changed_or_new + gone
}
