//! Mirroring contacts between iCloud and Google: opt-in, and only with one
//! iCloud and one Google account linked. A contact added, changed or deleted
//! in one is added, changed or deleted in the other.
//!
//! State, in `<data>/mirror.json` under a lock (`mirror.lock`; Cloud Contacts
//! 0.4.0 and 0.4.1 called it `keep-same.json`, moved on the first load):
//!
//! - `links`: which iCloud contact is which Google contact, each with `base`,
//!   the fields both model (see [`Shared`]) as they were when the two were
//!   last mirrored: the merge base.
//! - `pending`: a copy being made, recorded before it is written, so a run
//!   killed after the write but before the link is saved finds the copy and
//!   links it instead of copying again.
//! - `waiting`: doubtful pairs found when it was turned on, for the person to
//!   call the same contact (`pair`) or different ones (`apart`); neither is
//!   copied meanwhile.
//!
//! A run fetches both accounts (either failing: the run is a warning and
//! nothing changes), then:
//!
//! 1. A linked contact gone from one side is deleted from the other, and
//!    unlinked; more than 10 such deletions, or more than half the linked
//!    contacts, stop the run (`too_many_deletions`) until it is run with
//!    `yes`.
//! 2. Each linked pair is merged field by field against its base: a field
//!    changed on one side is changed on the other; phones, emails, URLs and
//!    addresses changed on both are joined, without duplicates (by their
//!    digits, case, or parts); a single field changed on both takes the
//!    later change, by each side's modification time (unknown: iCloud's, and
//!    a warning). Only these fields are written, and each write keeps what
//!    the account holds beyond them (iCloud's other fields, Google's groups
//!    and photos).
//! 3. A contact on one side with no link is copied to the other and linked.
//!
//! The link and base are saved only after the writes they record succeeded,
//! so a rerun after a kill neither duplicates nor deletes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::account::Provider;
use crate::error::{Error, Result};
use crate::model::{self, Address, Change, Contact, Field, Labeled, Multi};

/// The fields both iCloud and Google model, and so the only ones copied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    pub first: String,
    pub middle: String,
    pub last: String,
    pub prefix: String,
    pub suffix: String,
    pub nickname: String,
    pub company: String,
    pub job_title: String,
    pub department: String,
    pub birthday: String,
    pub notes: String,
    pub phones: Vec<Labeled>,
    pub emails: Vec<Labeled>,
    pub urls: Vec<Labeled>,
    pub addresses: Vec<Address>,
    /// Shown by its company: a company's card. Not said by a state saved before this was kept.
    #[serde(default)]
    pub company_card: Option<bool>,
}

const SCALARS: [Field; 11] = [
    Field::First,
    Field::Middle,
    Field::Last,
    Field::Prefix,
    Field::Suffix,
    Field::Nickname,
    Field::Company,
    Field::JobTitle,
    Field::Department,
    Field::Birthday,
    Field::Notes,
];

impl Shared {
    pub fn of(c: &Contact) -> Shared {
        let f = |field| c.field(field).trim().to_string();
        Shared {
            first: f(Field::First),
            middle: f(Field::Middle),
            last: f(Field::Last),
            prefix: f(Field::Prefix),
            suffix: f(Field::Suffix),
            nickname: f(Field::Nickname),
            company: f(Field::Company),
            job_title: f(Field::JobTitle),
            department: f(Field::Department),
            birthday: f(Field::Birthday),
            // notes keep their own spacing
            notes: c.field(Field::Notes).to_string(),
            phones: c.values(Multi::Phone),
            emails: c.values(Multi::Email),
            urls: c.values(Multi::Url),
            addresses: c.addresses(),
            company_card: Some(c.is_company()),
        }
    }

    /// The same with its lists sorted: list order is each account's own and
    /// does not cross.
    pub fn sorted(&self) -> Shared {
        let mut s = self.clone();
        for m in [Multi::Phone, Multi::Email, Multi::Url] {
            s.values_mut(m)
                .sort_by(|a, b| (&a.value, &a.label).cmp(&(&b.value, &b.label)));
        }
        s.addresses
            .sort_by_key(|a| (a.street.clone(), a.city.clone(), a.postal_code.clone(), a.label.clone()));
        s
    }

    pub fn scalar(&self, f: Field) -> &str {
        match f {
            Field::First => &self.first,
            Field::Middle => &self.middle,
            Field::Last => &self.last,
            Field::Prefix => &self.prefix,
            Field::Suffix => &self.suffix,
            Field::Nickname => &self.nickname,
            Field::Company => &self.company,
            Field::JobTitle => &self.job_title,
            Field::Department => &self.department,
            Field::Birthday => &self.birthday,
            Field::Notes => &self.notes,
        }
    }

    fn scalar_mut(&mut self, f: Field) -> &mut String {
        match f {
            Field::First => &mut self.first,
            Field::Middle => &mut self.middle,
            Field::Last => &mut self.last,
            Field::Prefix => &mut self.prefix,
            Field::Suffix => &mut self.suffix,
            Field::Nickname => &mut self.nickname,
            Field::Company => &mut self.company,
            Field::JobTitle => &mut self.job_title,
            Field::Department => &mut self.department,
            Field::Birthday => &mut self.birthday,
            Field::Notes => &mut self.notes,
        }
    }

    pub fn values(&self, m: Multi) -> &Vec<Labeled> {
        match m {
            Multi::Phone => &self.phones,
            Multi::Email => &self.emails,
            Multi::Url => &self.urls,
        }
    }

    fn values_mut(&mut self, m: Multi) -> &mut Vec<Labeled> {
        match m {
            Multi::Phone => &mut self.phones,
            Multi::Email => &mut self.emails,
            Multi::Url => &mut self.urls,
        }
    }

    /// The changes that make a contact holding `self` hold `to`. Removals
    /// first: a label is changed by removing and adding.
    pub fn changes_to(&self, to: &Shared) -> Vec<Change> {
        let mut out = Vec::new();
        for m in [Multi::Phone, Multi::Email, Multi::Url] {
            let mut seen: HashMap<&Labeled, usize> = HashMap::new();
            for l in self.values(m) {
                let nth = seen.entry(l).or_insert(0);
                if !to.values(m).contains(l) {
                    out.push(Change::RemoveExact(m, l.clone(), *nth));
                } else {
                    *nth += 1;
                }
            }
        }
        for f in SCALARS {
            if self.scalar(f) != to.scalar(f) {
                out.push(Change::Set(f, to.scalar(f).to_string()));
            }
        }
        for m in [Multi::Phone, Multi::Email, Multi::Url] {
            for l in to.values(m) {
                if !self.values(m).contains(l) {
                    out.push(Change::Add(m, l.clone()));
                }
            }
        }
        if self.addresses != to.addresses {
            out.push(Change::Addresses(to.addresses.clone()));
        }
        if let Some(company) = to.company_card.filter(|_| self.company_card != to.company_card) {
            out.push(Change::Company(company));
        }
        out
    }

    /// Whether the contact names someone (a copy needs a name, a company,
    /// a phone or an email).
    fn names_someone(&self) -> bool {
        Shared::default().changes_to(self).iter().any(|c| {
            matches!(
                c,
                Change::Set(
                    Field::First | Field::Middle | Field::Last | Field::Nickname | Field::Company,
                    _
                ) | Change::Add(Multi::Phone | Multi::Email, _)
            )
        })
    }
}

/// Whether two phones, emails or URLs are the same one: a phone by its
/// digits as the app matches numbers (`555 0100` is `+1 555 0100`), an email
/// ignoring case, a URL without a trailing slash.
fn same_value(m: Multi, a: &str, b: &str) -> bool {
    match m {
        Multi::Phone => model::same_number(a, b) || a.trim() == b.trim(),
        Multi::Email => a.trim().eq_ignore_ascii_case(b.trim()),
        Multi::Url => a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/'),
    }
}

fn same_address(a: &Address, b: &Address) -> bool {
    let key = |a: &Address| {
        [&a.street, &a.city, &a.state, &a.postal_code, &a.country]
            .iter()
            .map(|s| model::fold(s.trim()))
            .collect::<Vec<_>>()
    };
    key(a) == key(b)
}

/// Which side's single field wins when both changed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Later {
    ICloud,
    Google,
    /// A side's time is unknown: iCloud's is kept, with a warning.
    Unknown,
}

/// A three-way merge of one list, entries matched by `same`: kept when
/// both sides have it, or one side added it (not in `base`); a removal on
/// either side wins over an unchanged copy. Of one entry on both sides, the
/// side that changed it (against `base`) gives its text, else iCloud.
fn merge_list<T: Clone + PartialEq>(base: Option<&[T]>, i: &[T], g: &[T], same: impl Fn(&T, &T) -> bool) -> Vec<T> {
    let base = base.unwrap_or_default();
    let find = |list: &[T], e: &T| list.iter().find(|o| same(o, e)).cloned();
    let mut out: Vec<T> = Vec::new();
    let mut seen: Vec<T> = Vec::new();
    for e in i.iter().chain(g.iter()) {
        if seen.iter().any(|o| same(o, e)) {
            continue;
        }
        seen.push(e.clone());
        let (in_b, in_i, in_g) = (find(base, e), find(i, e), find(g, e));
        let keep = (in_i.is_some() && in_g.is_some()) || (in_b.is_none() && (in_i.is_some() || in_g.is_some()));
        if !keep {
            continue;
        }
        let chosen = match (in_i, in_g) {
            (Some(x), Some(y)) if x != y && in_b.as_ref() == Some(&x) => y,
            (Some(x), _) => x,
            (None, Some(y)) => y,
            (None, None) => continue,
        };
        out.push(chosen);
    }
    out
}

/// The merge of a linked pair: what both should hold, and the warnings.
pub fn merge(base: Option<&Shared>, i: &Shared, g: &Shared, later: Later) -> (Shared, Vec<String>) {
    // a pair just linked (no base yet) is joined as merging a duplicate
    // does: nothing either side says is lost
    if base.is_none() {
        return (crate::dedupe::merge_into(i, g), Vec::new());
    }
    let mut out = i.clone();
    let mut warnings = Vec::new();
    for f in SCALARS {
        let (iv, gv) = (i.scalar(f), g.scalar(f));
        if iv == gv {
            continue;
        }
        let bv = base.map(|b| b.scalar(f));
        let pick_g = match bv {
            Some(b) if b == iv => true,
            Some(b) if b == gv => false,
            // with no base, a field only one side has is taken
            None if iv.is_empty() => true,
            None if gv.is_empty() => false,
            _ => match later {
                Later::Google => true,
                Later::ICloud => false,
                Later::Unknown => {
                    warnings.push(format!(
                        "{} changed on both sides; kept iCloud's ({iv:?} over Google's {gv:?})",
                        field_name(f)
                    ));
                    false
                }
            },
        };
        if pick_g {
            *out.scalar_mut(f) = gv.to_string();
        }
    }
    for m in [Multi::Phone, Multi::Email, Multi::Url] {
        *out.values_mut(m) = merge_list(
            base.map(|b| b.values(m).as_slice()),
            i.values(m),
            g.values(m),
            |a, b| same_value(m, &a.value, &b.value),
        );
    }
    out.addresses = merge_list(
        base.map(|b| b.addresses.as_slice()),
        &i.addresses,
        &g.addresses,
        same_address,
    );
    // one of two values: where the sides differ, the one that is not the base's is the change. A base that does
    // not say leaves iCloud's, the one account that kept it until now.
    if i.company_card != g.company_card && base.is_some_and(|b| b.company_card == i.company_card) {
        out.company_card = g.company_card;
    }
    (out, warnings)
}

fn field_name(f: Field) -> &'static str {
    match f {
        Field::First => "first name",
        Field::Middle => "middle name",
        Field::Last => "last name",
        Field::Prefix => "name prefix",
        Field::Suffix => "name suffix",
        Field::Nickname => "nickname",
        Field::Company => "company",
        Field::JobTitle => "job title",
        Field::Department => "department",
        Field::Birthday => "birthday",
        Field::Notes => "notes",
    }
}

/// When a contact last changed, as `YYYY-MM-DDTHH:MM:SS` in UTC, if its
/// account says: iCloud's `dateModified`, Google's source `updateTime`.
pub fn modified(c: &Contact) -> Option<String> {
    let norm = |s: &str| -> Option<String> {
        let s = s.trim();
        (s.len() >= 19 && s.ends_with('Z') && s.as_bytes()[10] == b'T').then(|| s[..19].to_string())
    };
    if let Some(p) = c.0.get(crate::google::ORIGINAL) {
        return p["metadata"]["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["type"] == "CONTACT")
            .filter_map(|s| s["updateTime"].as_str().and_then(norm))
            .max();
    }
    norm(c.get("dateModified"))
}

fn later(i: &Contact, g: &Contact) -> Later {
    match (modified(i), modified(g)) {
        (Some(a), Some(b)) if b > a => Later::Google,
        (Some(_), Some(_)) => Later::ICloud,
        _ => Later::Unknown,
    }
}

// ---- state ----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub icloud: String,
    pub google: String,
    /// As both were when last mirrored; none right after pairing.
    #[serde(default)]
    pub base: Option<Shared>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    ICloud,
    Google,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    /// The side the copy is made on.
    pub to: Side,
    /// The contact copied (an id on the other side).
    pub from: String,
    pub shared: Shared,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pair {
    pub icloud: String,
    pub google: String,
    /// Why they may be one person.
    pub why: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub on: bool,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub pending: Vec<Pending>,
    #[serde(default)]
    pub waiting: Vec<Pair>,
    /// Pairs the person called different people.
    #[serde(default)]
    pub apart: Vec<(String, String)>,
    /// Seconds since the epoch.
    #[serde(default)]
    pub last_run: Option<u64>,
    /// Why the last run stopped, if it did.
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// The window's offer was declined: it is not shown again.
    #[serde(default)]
    pub offer_declined: bool,
}

pub fn state_path(root: &Path) -> PathBuf {
    root.join("mirror.json")
}

pub fn load(root: &Path) -> Result<State> {
    moved_from_keep_same(root)?;
    Ok(icloud_app::store::read_json(&state_path(root))?.unwrap_or_default())
}

/// Cloud Contacts 0.4.0 and 0.4.1 kept the state as `keep-same.json`: it
/// becomes `mirror.json`, links and all. A rename, made only while
/// `mirror.json` is not there: of two processes at it, the second finds the
/// old file gone, so the state is never replaced (and no lock is needed,
/// which `load` could not take inside `update`'s).
fn moved_from_keep_same(root: &Path) -> Result<()> {
    let new = state_path(root);
    if new.exists() {
        return Ok(());
    }
    match std::fs::rename(root.join("keep-same.json"), &new) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::from(e)),
        _ => Ok(()),
    }
}

fn save(root: &Path, s: &State) -> Result<()> {
    Ok(icloud_app::store::write_json(&state_path(root), s)?)
}

/// Changes the state under its lock.
pub fn update<T>(root: &Path, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
    let _lock = icloud_app::store::lock(root, "mirror.lock")?;
    let mut s = load(root)?;
    let out = f(&mut s)?;
    save(root, &s)?;
    Ok(out)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- pairing and duplicates ----

/// Why two contacts may be one person: a shared email, a shared phone
/// number, the same full name.
pub fn evidence(a: &Contact, b: &Contact) -> Vec<String> {
    let mut why = Vec::new();
    for e in a.values(Multi::Email) {
        if b.values(Multi::Email)
            .iter()
            .any(|o| Multi::Email.matches(&o.value, &e.value))
        {
            why.push(format!("email {}", e.value.trim()));
            break;
        }
    }
    for p in a.values(Multi::Phone) {
        if model::digits(&p.value).len() >= 7
            && b.values(Multi::Phone)
                .iter()
                .any(|o| model::same_number(&o.value, &p.value))
        {
            why.push(format!("phone {}", p.value.trim()));
            break;
        }
    }
    if same_name(a, b) && !a.person_name().is_empty() {
        why.push(format!("name {}", a.display_name()));
    }
    why
}

fn same_name(a: &Contact, b: &Contact) -> bool {
    let (x, y) = (model::fold(&a.display_name()), model::fold(&b.display_name()));
    !x.is_empty() && x == y
}

/// Names alike enough for a shared email or phone to make a pair certain:
/// the same, or one containing the other (Ann Lee, Ann Lee-Smith).
fn similar_name(a: &Contact, b: &Contact) -> bool {
    let (x, y) = (model::fold(&a.display_name()), model::fold(&b.display_name()));
    !x.is_empty() && !y.is_empty() && (x == y || x.contains(&y) || y.contains(&x))
}

/// Contacts already in both accounts, when mirroring is turned on: certain
/// pairs (a shared email or phone and alike names, and no other candidate
/// for either), and doubtful ones for the person.
pub fn pairing(icloud: &[Contact], google: &[Contact], state: &State) -> (Vec<Pair>, Vec<Pair>) {
    let linked_i: BTreeSet<&str> = state.links.iter().map(|l| l.icloud.as_str()).collect();
    let linked_g: BTreeSet<&str> = state.links.iter().map(|l| l.google.as_str()).collect();
    let apart: BTreeSet<(&str, &str)> = state.apart.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut candidates = Vec::new();
    for i in icloud.iter().filter(|c| !linked_i.contains(c.id())) {
        for g in google.iter().filter(|c| !linked_g.contains(c.id())) {
            if apart.contains(&(i.id(), g.id())) {
                continue;
            }
            let why = evidence(i, g);
            if why.is_empty() {
                continue;
            }
            let strong = why.iter().any(|w| !w.starts_with("name ")) && similar_name(i, g);
            candidates.push((i, g, why, strong));
        }
    }
    let count = |id: &str, side: Side| {
        candidates
            .iter()
            .filter(|(i, g, ..)| match side {
                Side::ICloud => i.id() == id,
                Side::Google => g.id() == id,
            })
            .count()
    };
    let (mut sure, mut doubtful) = (Vec::new(), Vec::new());
    for (i, g, why, strong) in &candidates {
        let pair = Pair {
            icloud: i.id().to_string(),
            google: g.id().to_string(),
            why: why.join(", "),
        };
        if *strong && count(i.id(), Side::ICloud) == 1 && count(g.id(), Side::Google) == 1 {
            sure.push(pair);
        } else {
            doubtful.push(pair);
        }
    }
    (sure, doubtful)
}

// ---- the run ----

/// What a run did.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Report {
    pub changed: usize,
    pub copied_to_icloud: usize,
    pub copied_to_google: usize,
    pub deleted: usize,
    pub linked: usize,
    pub warnings: Vec<String>,
}

/// A deletion a run would make, for `too_many_deletions`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Deletion {
    pub side: Side,
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub enum RunError {
    /// An account could not be read: a warning; nothing changed.
    Account(Side, Error),
    TooManyDeletions(Vec<Deletion>),
    Other(Error),
}

impl From<Error> for RunError {
    fn from(e: Error) -> Self {
        RunError::Other(e)
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Account(Side::ICloud, e) => write!(f, "iCloud could not be read, so nothing changed: {e}"),
            RunError::Account(Side::Google, e) => write!(f, "Google could not be read, so nothing changed: {e}"),
            RunError::TooManyDeletions(d) => write!(
                f,
                "this run would delete {} contacts, more than mirroring deletes without asking; nothing changed",
                d.len()
            ),
            RunError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// The deletions a run makes without `yes`: at most 10, and at most half
/// the linked contacts.
pub fn too_many(deletions: usize, linked: usize) -> bool {
    deletions > 10 || (deletions > 0 && deletions * 2 > linked)
}

pub struct Engine<'a> {
    pub root: &'a Path,
    pub icloud: &'a dyn Provider,
    pub google: &'a dyn Provider,
    /// Tests: stop (as a kill would) right after this many writes.
    pub crash_after: Option<usize>,
    writes: usize,
}

impl<'a> Engine<'a> {
    pub fn new(root: &'a Path, icloud: &'a dyn Provider, google: &'a dyn Provider) -> Self {
        Engine {
            root,
            icloud,
            google,
            crash_after: None,
            writes: 0,
        }
    }

    fn side(&self, s: Side) -> &'a dyn Provider {
        match s {
            Side::ICloud => self.icloud,
            Side::Google => self.google,
        }
    }

    /// Counts a write that happened; a test's crash comes right after one.
    fn wrote(&mut self) -> Result<()> {
        self.writes += 1;
        if self.crash_after.is_some_and(|n| self.writes >= n) {
            return Err(Error::Other("crash (test)".into()));
        }
        Ok(())
    }

    /// Fetches both accounts.
    pub fn fetch(&self) -> std::result::Result<(Vec<Contact>, Vec<Contact>), RunError> {
        let (i, g) = std::thread::scope(|s| {
            let i = s.spawn(|| self.icloud.sync());
            let g = s.spawn(|| self.google.sync());
            (i.join(), g.join())
        });
        let i = i
            .unwrap_or_else(|_| Err(Error::Other("crashed".into())))
            .map_err(|e| RunError::Account(Side::ICloud, e))?;
        let g = g
            .unwrap_or_else(|_| Err(Error::Other("crashed".into())))
            .map_err(|e| RunError::Account(Side::Google, e))?;
        Ok((i.1.contacts, g.1.contacts))
    }

    /// One run (see the module's header); `yes` confirms more deletions
    /// than a run makes by itself. A failure is recorded as the state's
    /// problem.
    pub fn run(&mut self, yes: bool) -> std::result::Result<Report, RunError> {
        let _lock = icloud_app::store::lock(self.root, "mirror.lock").map_err(Error::from)?;
        let result = self.run_locked(yes);
        let mut s = load(self.root)?;
        match &result {
            Ok(r) => {
                s.last_run = Some(now());
                s.problem = None;
                s.warnings = r.warnings.clone();
            }
            Err(e) => s.problem = Some(e.to_string()),
        }
        save(self.root, &s)?;
        result
    }

    fn run_locked(&mut self, yes: bool) -> std::result::Result<Report, RunError> {
        let (icloud, google) = self.fetch()?;
        let mut s = load(self.root)?;
        let mut report = Report::default();
        let by = |list: &[Contact]| -> HashMap<String, Contact> {
            list.iter().map(|c| (c.id().to_string(), c.clone())).collect()
        };
        let (mut ic, mut gg) = (by(&icloud), by(&google));

        // copies made by a run cut short: link them, not copy again
        for p in std::mem::take(&mut s.pending) {
            let linked = |id: &str, side| {
                s.links.iter().any(|l| match side {
                    Side::ICloud => l.icloud == id,
                    Side::Google => l.google == id,
                })
            };
            let target = match p.to {
                Side::ICloud => &ic,
                Side::Google => &gg,
            };
            let found = target
                .values()
                .filter(|c| !linked(c.id(), p.to))
                // but for being a company's card, which a copy noted before that was kept does not say
                .find(|c| {
                    let copy = Shared {
                        company_card: p.shared.company_card,
                        ..Shared::of(c)
                    };
                    copy == p.shared
                })
                .map(|c| c.id().to_string());
            if let Some(found) = found {
                let (icloud, google) = match p.to {
                    Side::ICloud => (found, p.from.clone()),
                    Side::Google => (p.from.clone(), found),
                };
                s.links.push(Link {
                    icloud,
                    google,
                    base: Some(p.shared.clone()),
                });
            }
        }
        save(self.root, &s)?;

        // 1. deletions
        let mut deletions = Vec::new();
        for l in &s.links {
            match (ic.get(&l.icloud), gg.get(&l.google)) {
                (Some(c), None) => deletions.push(Deletion {
                    side: Side::ICloud,
                    id: l.icloud.clone(),
                    name: c.display_name(),
                }),
                (None, Some(c)) => deletions.push(Deletion {
                    side: Side::Google,
                    id: l.google.clone(),
                    name: c.display_name(),
                }),
                _ => {}
            }
        }
        if !yes && too_many(deletions.len(), s.links.len()) {
            return Err(RunError::TooManyDeletions(deletions));
        }
        for d in &deletions {
            let map = match d.side {
                Side::ICloud => &mut ic,
                Side::Google => &mut gg,
            };
            if let Some(c) = map.remove(&d.id) {
                self.side(d.side).delete(&c)?;
                report.deleted += 1;
                s.links.retain(|l| match d.side {
                    Side::ICloud => l.icloud != d.id,
                    Side::Google => l.google != d.id,
                });
                save(self.root, &s)?;
                self.wrote()?;
            }
        }
        // both gone
        s.links
            .retain(|l| ic.contains_key(&l.icloud) || gg.contains_key(&l.google));

        // 2. linked pairs
        for n in 0..s.links.len() {
            let (iid, gid) = (s.links[n].icloud.clone(), s.links[n].google.clone());
            let (Some(i), Some(g)) = (ic.get(&iid).cloned(), gg.get(&gid).cloned()) else {
                continue;
            };
            let (si, sg) = (Shared::of(&i), Shared::of(&g));
            let (merged, warnings) = merge(s.links[n].base.as_ref(), &si, &sg, later(&i, &g));
            report
                .warnings
                .extend(warnings.into_iter().map(|w| format!("{}: {w}", i.display_name())));
            let (to_i, to_g) = (si.changes_to(&merged), sg.changes_to(&merged));
            if !to_i.is_empty() {
                let saved = self.icloud.update(&i, &to_i)?;
                ic.insert(iid.clone(), saved.value);
                self.wrote()?;
            }
            if !to_g.is_empty() {
                let saved = self.google.update(&g, &to_g)?;
                gg.insert(gid.clone(), saved.value);
                self.wrote()?;
            }
            if !to_i.is_empty() || !to_g.is_empty() {
                report.changed += 1;
            }
            if s.links[n].base.as_ref() != Some(&merged) {
                s.links[n].base = Some(merged);
                save(self.root, &s)?;
            }
        }

        // 3. copies
        let waiting_i: BTreeSet<String> = s.waiting.iter().map(|p| p.icloud.clone()).collect();
        let waiting_g: BTreeSet<String> = s.waiting.iter().map(|p| p.google.clone()).collect();
        for (from_side, to_side) in [(Side::ICloud, Side::Google), (Side::Google, Side::ICloud)] {
            let (source, waiting) = match from_side {
                Side::ICloud => (&icloud, &waiting_i),
                Side::Google => (&google, &waiting_g),
            };
            for c in source {
                let id = c.id();
                let linked = s.links.iter().any(|l| match from_side {
                    Side::ICloud => l.icloud == id,
                    Side::Google => l.google == id,
                });
                let gone = match from_side {
                    Side::ICloud => !ic.contains_key(id),
                    Side::Google => !gg.contains_key(id),
                };
                let shared = Shared::of(c);
                if linked || gone || waiting.contains(id) || !shared.names_someone() {
                    continue;
                }
                s.pending.push(Pending {
                    to: to_side,
                    from: id.to_string(),
                    shared: shared.clone(),
                });
                save(self.root, &s)?;
                // Shown by its company in the copy too (iCloud keeps that as a flag; Google has none, and reads a
                // contact with no name as one).
                let saved = self
                    .side(to_side)
                    .add(&Shared::default().changes_to(&shared), c.is_company())?;
                self.wrote()?;
                let new = saved.value.id().to_string();
                let (icloud, google) = match to_side {
                    Side::ICloud => (new, id.to_string()),
                    Side::Google => (id.to_string(), new),
                };
                s.pending.retain(|p| p.from != id);
                s.links.push(Link {
                    icloud,
                    google,
                    base: Some(shared),
                });
                save(self.root, &s)?;
                match to_side {
                    Side::ICloud => report.copied_to_icloud += 1,
                    Side::Google => report.copied_to_google += 1,
                }
            }
        }
        report.linked = s.links.len();
        save(self.root, &s)?;
        Ok(report)
    }
}

/// What turning it on finds, before anything changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    /// Contacts in both accounts that are surely one person: linked, not copied.
    pub pairs: Vec<Pair>,
    /// Maybe one person: for the person to decide; neither is copied meanwhile.
    pub doubtful: Vec<Pair>,
    /// Contacts that will be copied to the other account.
    pub to_google: usize,
    pub to_icloud: usize,
}

/// The person's call on a doubtful pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Same,
    Different,
}

impl Engine<'_> {
    /// What turning on would do (fetches both accounts; changes nothing).
    pub fn plan(&self) -> std::result::Result<Plan, RunError> {
        let (icloud, google) = self.fetch()?;
        let s = load(self.root)?;
        let (pairs, doubtful) = pairing(&icloud, &google, &s);
        let held = |id: &str, side: Side| {
            s.links.iter().any(|l| match side {
                Side::ICloud => l.icloud == id,
                Side::Google => l.google == id,
            }) || pairs.iter().chain(&doubtful).any(|p| match side {
                Side::ICloud => p.icloud == id,
                Side::Google => p.google == id,
            })
        };
        let copies = |list: &[Contact], side| {
            list.iter()
                .filter(|c| !held(c.id(), side) && Shared::of(c).names_someone())
                .count()
        };
        Ok(Plan {
            to_google: copies(&icloud, Side::ICloud),
            to_icloud: copies(&google, Side::Google),
            pairs,
            doubtful,
        })
    }
}

/// Turns it on with `plan`'s pairs linked and the person's `decisions` on
/// doubtful ones (any left undecided wait). Links kept from before start
/// again with no base: both sides' values are joined, not taken as edits.
pub fn turn_on(root: &Path, plan: &Plan, decisions: &[(String, String, Decision)]) -> Result<()> {
    update(root, |s| {
        s.on = true;
        s.problem = None;
        for l in &mut s.links {
            l.base = None;
        }
        for p in &plan.pairs {
            s.links.push(Link {
                icloud: p.icloud.clone(),
                google: p.google.clone(),
                base: None,
            });
        }
        for p in &plan.doubtful {
            if !s.waiting.iter().any(|w| w.icloud == p.icloud && w.google == p.google) {
                s.waiting.push(p.clone());
            }
        }
        for (i, g, d) in decisions {
            decide_in(s, i, g, *d);
        }
        Ok(())
    })
}

fn decide_in(s: &mut State, icloud: &str, google: &str, d: Decision) {
    s.waiting.retain(|w| !(w.icloud == icloud && w.google == google));
    match d {
        Decision::Same => {
            // a contact is in one pair: other doubts about either go
            s.waiting.retain(|w| w.icloud != icloud && w.google != google);
            s.links.retain(|l| l.icloud != icloud && l.google != google);
            s.links.push(Link {
                icloud: icloud.to_string(),
                google: google.to_string(),
                base: None,
            });
        }
        Decision::Different => {
            s.apart.push((icloud.to_string(), google.to_string()));
        }
    }
}

/// The person's call on a pair (waiting or not).
pub fn decide(root: &Path, icloud: &str, google: &str, d: Decision) -> Result<()> {
    update(root, |s| {
        decide_in(s, icloud, google, d);
        Ok(())
    })
}

/// Turns it off; links stay, so turning it on again pairs nothing twice.
pub fn turn_off(root: &Path) -> Result<bool> {
    update(root, |s| Ok(std::mem::replace(&mut s.on, false)))
}

/// The two accounts mirrored, from the linked ones.
pub enum Pairing<'a> {
    /// No account of this provider ("icloud" or "google") is linked.
    Missing(&'static str),
    /// More than one of a provider: which to pair is not clear.
    Ambiguous(String),
    Ready(&'a crate::book::Account, &'a crate::book::Account),
}

pub fn pairing_of(book: &crate::book::Book) -> Pairing<'_> {
    let of = |p: &str| book.accounts.iter().filter(|a| a.provider == p).collect::<Vec<_>>();
    let (i, g) = (of("icloud"), of("google"));
    match (i.as_slice(), g.as_slice()) {
        ([], _) => Pairing::Missing("icloud"),
        (_, []) => Pairing::Missing("google"),
        ([i], [g]) => Pairing::Ready(i, g),
        (_, many) => Pairing::Ambiguous(format!(
            "mirroring pairs one iCloud and one Google account, and {} Google accounts are linked ({})",
            many.len(),
            many.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The systemd user timer that runs it every few minutes.
pub const TIMER: &str = "cloud-contacts-mirror.timer";
/// What 0.4.0 and 0.4.1 called the timer: turned off with the new one.
pub const OLD_TIMER: &str = "cloud-contacts-keep-same.timer";

/// Starts or stops the timer (`systemctl --user`, or
/// `CLOUD_CONTACTS_SYSTEMCTL`); a failure is a warning: it is on either way,
/// and `mirror run` runs it.
pub fn timer(on: bool) -> Option<String> {
    let systemctl = std::env::var("CLOUD_CONTACTS_SYSTEMCTL").unwrap_or_else(|_| "systemctl".into());
    // 0.4.0 and 0.4.1's timer, if it was ever turned on: off, whatever comes of it
    _ = std::process::Command::new(&systemctl)
        .args(["--user", "disable", "--now", OLD_TIMER])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let verb = if on { "enable" } else { "disable" };
    let out = std::process::Command::new(&systemctl)
        .args(["--user", verb, "--now", TIMER])
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!(
            "could not {verb} {TIMER}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Some(format!("could not {verb} {TIMER}: {e}")),
    }
}

/// An iCloud or Google account was unlinked: mirroring stops
/// (both sides' contacts stay as they are, and the links are kept for a
/// later turn on). Answers what to tell the person, when it was on.
pub fn account_removed(root: &Path) -> Option<String> {
    let was_on = turn_off(root).ok()?;
    if !was_on {
        return None;
    }
    let mut note = "mirroring iCloud and Google contacts is off now; both keep their contacts as they are".to_string();
    if let Some(w) = timer(false) {
        note.push_str(&format!(" ({w})"));
    }
    Some(note)
}

/// The status `mirror` and `status` show.
pub fn status_json(s: &State) -> Value {
    json!({
        "on": s.on,
        "linked": s.links.len(),
        "last_run": s.last_run,
        "problem": s.problem,
        "warnings": s.warnings,
        "waiting": s.waiting,
    })
}

/// What the window shows about mirroring, decided from what is linked and
/// the state: nothing without both an iCloud and a Google account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// Not both kinds of account linked: no mention at all.
    Nothing,
    /// The switch, and under it this line; `offer`: the banner too.
    Switch { on: bool, line: String, offer: bool },
}

/// `accounts`: each linked account's provider and whether it is signed in.
pub fn shown(accounts: &[(String, bool)], s: &State, ago: impl Fn(u64) -> String) -> Shown {
    let has = |p: &str| accounts.iter().any(|(q, _)| q == p);
    if !has("icloud") || !has("google") {
        return Shown::Nothing;
    }
    let signed_out: Vec<&str> = accounts
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(p, _)| if p == "icloud" { "iCloud" } else { "Google" })
        .collect();
    let line = if let Some(who) = signed_out.first() {
        format!("{who} is signed out: sign in again for changes to cross")
    } else if !s.on {
        "Off".to_string()
    } else if let Some(p) = &s.problem {
        p.clone()
    } else if !s.waiting.is_empty() {
        match s.waiting.len() {
            1 => "1 possible duplicate waits for you".to_string(),
            n => format!("{n} possible duplicates wait for you"),
        }
    } else {
        match s.last_run {
            Some(t) => format!("On · last mirrored {}", ago(t)),
            None => "On".to_string(),
        }
    };
    Shown::Switch {
        on: s.on,
        offer: !s.on && !s.offer_declined && signed_out.is_empty(),
        line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_labeled;

    fn shared(v: Value) -> Shared {
        Shared::of(&Contact(v.as_object().unwrap().clone()))
    }

    #[test]
    fn a_card_made_a_companys_on_one_side_is_one_on_both() {
        let person = shared(json!({"firstName": "Bo", "companyName": "Acme"}));
        let company = shared(json!({"firstName": "Bo", "companyName": "Acme", "isCompany": true}));
        for later in [Later::ICloud, Later::Google, Later::Unknown] {
            // changed on either side, it is the change; changed back, that is
            let merged = |base, i, g| merge(Some(base), i, g, later).0.company_card;
            assert_eq!(merged(&person, &company, &person), Some(true));
            assert_eq!(merged(&person, &person, &company), Some(true));
            assert_eq!(merged(&company, &person, &company), Some(false));
            assert_eq!(merged(&company, &company, &person), Some(false));
        }
        assert_eq!(person.changes_to(&company), [Change::Company(true)]);
        let mut card = Contact::new(false);
        card.apply(&Shared::default().changes_to(&company)).unwrap();
        assert!(card.is_company());
        // a state saved before the flag was kept does not say: iCloud's stands, either way
        let old: Shared = serde_json::from_value(json!({
            "first": "Bo", "middle": "", "last": "", "prefix": "", "suffix": "", "nickname": "", "company": "Acme",
            "job_title": "", "department": "", "birthday": "", "notes": "", "phones": [], "emails": [], "urls": [],
            "addresses": []
        }))
        .unwrap();
        assert_eq!(old.company_card, None);
        assert_eq!(
            merge(Some(&old), &company, &person, Later::Unknown).0.company_card,
            Some(true)
        );
        assert_eq!(
            merge(Some(&old), &person, &company, Later::Unknown).0.company_card,
            Some(false)
        );
    }

    #[test]
    fn a_field_changed_on_one_side_crosses() {
        let base = shared(json!({"firstName": "Ann", "lastName": "Lee"}));
        let i = shared(json!({"firstName": "Ann", "lastName": "Lee-Smith"}));
        let g = base.clone();
        let (m, w) = merge(Some(&base), &i, &g, Later::Unknown);
        assert_eq!(m.last, "Lee-Smith");
        assert!(w.is_empty());
        let (m, _) = merge(Some(&base), &g, &i, Later::Unknown);
        assert_eq!(m.last, "Lee-Smith", "from Google too");
    }

    #[test]
    fn a_field_changed_on_both_takes_the_later_or_icloud_with_a_warning() {
        let base = shared(json!({"jobTitle": "A"}));
        let i = shared(json!({"jobTitle": "B"}));
        let g = shared(json!({"jobTitle": "C"}));
        assert_eq!(merge(Some(&base), &i, &g, Later::Google).0.job_title, "C");
        assert_eq!(merge(Some(&base), &i, &g, Later::ICloud).0.job_title, "B");
        let (m, w) = merge(Some(&base), &i, &g, Later::Unknown);
        assert_eq!((m.job_title.as_str(), w.len()), ("B", 1));
    }

    #[test]
    fn lists_changed_on_both_are_joined_and_removals_win() {
        let p = |v: &str| json!({"label": "MOBILE", "field": v});
        let base = shared(json!({"phones": [p("+1 555 0100"), p("+1 555 0199")]}));
        // iCloud adds one and removes 0199; Google adds another, the same
        // number as iCloud's addition written differently, and keeps 0199
        let i = shared(json!({"phones": [p("+1 555 0100"), p("+1 555 0177")]}));
        let g = shared(json!({"phones": [p("+1 555 0100"), p("+1 555 0199"), p("+1-555-0177"), p("+1 555 0188")]}));
        let (m, _) = merge(Some(&base), &i, &g, Later::Unknown);
        let values: Vec<&str> = m.phones.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(values, ["+1 555 0100", "+1 555 0177", "+1 555 0188"]);
    }

    #[test]
    fn with_no_base_both_sides_are_kept() {
        let i = shared(json!({"firstName": "Ann", "emailAddresses": [{"label": "HOME", "field": "a@x"}]}));
        let g = shared(
            json!({"firstName": "Ann", "lastName": "Lee", "emailAddresses": [{"label": "WORK", "field": "A@X"}, {"field": "b@x"}]}),
        );
        let (m, _) = merge(None, &i, &g, Later::Unknown);
        assert_eq!(m.last, "Lee");
        assert_eq!(m.emails.len(), 2, "a@x once, iCloud's label: {:?}", m.emails);
        assert_eq!(m.emails[0].label, "HOME");
    }

    #[test]
    fn changes_make_one_side_the_other() {
        let mut c = Contact(
            json!({"firstName": "Ann", "phones": [{"label": "HOME", "field": "1"}], "photo": {"url": "u"}})
                .as_object()
                .unwrap()
                .clone(),
        );
        let mut to = Shared::of(&c);
        to.last = "Lee".into();
        to.phones = vec![parse_labeled("work=1", "MOBILE")];
        to.addresses = vec![Address {
            city: "Oslo".into(),
            ..Default::default()
        }];
        c.apply(&Shared::of(&c).changes_to(&to)).unwrap();
        assert_eq!(Shared::of(&c), to);
        assert_eq!(c.0["photo"]["url"], "u", "what is not shared stays");
        assert!(Shared::of(&c).changes_to(&to).is_empty());
    }

    #[test]
    fn modification_times() {
        let i = Contact(
            json!({"dateModified": "2026-11-18T08:00:00Z"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let g = Contact(
            json!({crate::google::ORIGINAL: {"metadata": {"sources": [{"type": "CONTACT", "updateTime": "2026-11-18T09:00:00.123Z"}]}}})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!(later(&i, &g), Later::Google);
        assert_eq!(later(&g, &i), Later::ICloud);
        assert_eq!(later(&i, &Contact::new(false)), Later::Unknown);
    }

    #[test]
    fn the_deletion_guard() {
        assert!(!too_many(0, 0));
        assert!(!too_many(1, 2));
        assert!(too_many(2, 3));
        assert!(!too_many(10, 100));
        assert!(too_many(11, 100));
    }

    #[test]
    fn what_the_window_shows() {
        let s = State::default();
        let ago = |_| "now".to_string();
        let one = [("icloud".to_string(), true)];
        assert_eq!(shown(&one, &s, ago), Shown::Nothing);
        let both = [("icloud".to_string(), true), ("google".to_string(), true)];
        assert_eq!(
            shown(&both, &s, ago),
            Shown::Switch {
                on: false,
                line: "Off".into(),
                offer: true
            }
        );
        let declined = State {
            offer_declined: true,
            ..State::default()
        };
        assert!(matches!(
            shown(&both, &declined, ago),
            Shown::Switch { offer: false, .. }
        ));
        let out = [("icloud".to_string(), true), ("google".to_string(), false)];
        assert_eq!(
            shown(&out, &s, ago),
            Shown::Switch {
                on: false,
                line: "Google is signed out: sign in again for changes to cross".into(),
                offer: false
            }
        );
        let on = State {
            on: true,
            last_run: Some(1),
            ..State::default()
        };
        assert_eq!(
            shown(&both, &on, ago),
            Shown::Switch {
                on: true,
                line: "On · last mirrored now".into(),
                offer: false
            }
        );
    }
}
