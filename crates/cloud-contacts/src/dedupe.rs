//! Duplicates: contacts that may be one person, found in groups, and merged
//! into one card without losing anything.
//!
//! **Finding.** Contacts are indexed by email, by phone (its last seven
//! digits, then compared as [`model::same_number`] does, so a local
//! `0408 400701` meets `+358 40 8400701`) and by full name (case,
//! punctuation, spaces and accents aside: `Cold Room` is `Coldroom`). A
//! pair found is *likely* or *check*:
//!
//! - likely: a shared phone or email and the same name, or one name inside
//!   the other (`Marco` in `Marco Gomez`); or the same full name of two
//!   parts or more (or the same company) with nothing against it (no
//!   different birthdays, no phones of both that differ, no emails of both
//!   that differ);
//! - check: a shared phone or email and different names; the same
//!   one-word name and no shared phone or email; the same full name with
//!   something against it.
//!
//! One name inside another (`Jack` and `Jack (Club)`) with nothing shared
//! is no reason at all: first names are common.
//!
//! Groups are the sets joined by likely pairs (A~B and B~C make one group of
//! three). A check pair joins nothing: it is listed on its own, to merge or
//! keep apart, unless both are in one likely group already. Pairs the
//! person called different people ([`Apart`]) are not found. Only contacts
//! of one account form groups; a pair across accounts is listed apart, by
//! the same rules, as mirroring pairs those.
//!
//! **Merging** ([`merge_into`]) never loses what either card says: phones,
//! emails, URLs and addresses are joined, without the same one twice; the
//! fuller name is kept, and the other one, when it is more than a spelling
//! of it, becomes the nickname (or a line in the notes); a single field
//! only one card has is taken; one both have differently stays as the kept
//! card has it, with the other's in the notes ("Also: Company: X"). Notes
//! are joined. The kept card is the one with more in it (then the more
//! recently changed); what only it holds (its photo, dates, related names,
//! anything this app does not show) stays as it is.
//!
//! **Order** ([`apply`]): both cards are first appended, as vCards, to
//! `<data>/merged-backup/<date>.vcf` (and synced to disk); then the kept card
//! is updated; only then are the others deleted. A crash or a failure at any
//! step leaves every contact in place, and the backup holds both as they
//! were.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::book::Entry;
use crate::error::{Error, Result};
use crate::mirror::Shared;
use crate::model::{self, Address, Change, Contact, Field, Labeled, Multi};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Likely,
    Check,
}

/// Contacts that may be one person: indexes into the entries given to
/// [`find`], and why.
#[derive(Debug, Clone)]
pub struct Group {
    pub members: Vec<usize>,
    pub confidence: Confidence,
    pub why: Vec<String>,
}

/// A pair across two accounts (mirroring pairs these, merging does not).
#[derive(Debug, Clone)]
pub struct Across {
    pub a: usize,
    pub b: usize,
    pub why: Vec<String>,
}

// ---- names and values, compared as people read them ----

/// Letters and digits only, lowercased: `St. Pierre` and `St-Pierre` are
/// one name.
pub fn compact(s: &str) -> String {
    model::fold(s)
        .chars()
        .map(plain)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// A Latin letter without its accent (`é` is `e`); any other as it is.
fn plain(c: char) -> char {
    const LETTERS: [(char, &str); 15] = [
        ('a', "àáâãäåāăą"),
        ('c', "çćč"),
        ('d', "ďđ"),
        ('e', "èéêëēėęě"),
        ('g', "ğ"),
        ('i', "ìíîïīįı"),
        ('l', "ľĺł"),
        ('n', "ñńň"),
        ('o', "òóôõöøōő"),
        ('r', "řŕ"),
        ('s', "śšş"),
        ('t', "ţť"),
        ('u', "ùúûüūůűų"),
        ('y', "ýÿ"),
        ('z', "žźż"),
    ];
    LETTERS
        .iter()
        .find(|(_, accented)| accented.contains(c))
        .map_or(c, |(base, _)| *base)
}

fn words(s: &str) -> Vec<String> {
    model::fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The name a card is known by for matching: the person's name, else the
/// company's.
fn name_of(c: &Contact) -> String {
    let n = c.person_name();
    if n.is_empty() {
        c.field(Field::Company).trim().to_string()
    } else {
        n
    }
}

fn shared_values(a: &Contact, b: &Contact) -> Vec<String> {
    let mut why = Vec::new();
    for e in a.values(Multi::Email) {
        if b.values(Multi::Email)
            .iter()
            .any(|o| o.value.trim().eq_ignore_ascii_case(e.value.trim()))
        {
            why.push(format!("email {}", e.value.trim()));
        }
    }
    for p in a.values(Multi::Phone) {
        if b.values(Multi::Phone)
            .iter()
            .any(|o| model::same_number(&o.value, &p.value))
        {
            why.push(format!("phone {}", p.value.trim()));
        }
    }
    why
}

/// Both have one of these and none in common.
fn differ(a: &Contact, b: &Contact, m: Multi) -> bool {
    let (va, vb) = (a.values(m), b.values(m));
    !va.is_empty()
        && !vb.is_empty()
        && !va.iter().any(|x| {
            vb.iter().any(|y| match m {
                Multi::Phone => model::same_number(&x.value, &y.value),
                _ => x.value.trim().eq_ignore_ascii_case(y.value.trim()),
            })
        })
}

/// One name inside the other, as words in order (`Marco` in
/// `Marco Gomez`), or as letters (`StPierre` in `Brandon St. Pierre`).
fn name_within(a: &str, b: &str) -> bool {
    let (ca, cb) = (compact(a), compact(b));
    if ca.is_empty() || cb.is_empty() {
        return false;
    }
    let (wa, wb) = (words(a), words(b));
    let (short, long) = if wa.len() <= wb.len() { (&wa, &wb) } else { (&wb, &wa) };
    let in_words = long.windows(short.len()).any(|w| w == short.as_slice());
    in_words || ca.contains(&cb) || cb.contains(&ca)
}

/// Why `a` and `b` may be one person, and how sure: `None` when nothing
/// joins them (see the module's header).
pub fn edge(a: &Contact, b: &Contact) -> Option<(Confidence, Vec<String>)> {
    let (na, nb) = (name_of(a), name_of(b));
    let same_name = !compact(&na).is_empty() && compact(&na) == compact(&nb);
    let shared = shared_values(a, b);
    let within = name_within(&na, &nb);
    let conflict = differ(a, b, Multi::Phone)
        || differ(a, b, Multi::Email)
        || (!a.field(Field::Birthday).is_empty()
            && !b.field(Field::Birthday).is_empty()
            && a.field(Field::Birthday) != b.field(Field::Birthday));
    // either way round: `Cold Room` and `Coldroom` are one full name
    let full_name = words(&na).len().max(words(&nb).len()) >= 2 || (a.is_company() && b.is_company());
    let mut why = shared.clone();
    if same_name {
        why.push(format!("name {}", na.trim()));
    } else if within && !shared.is_empty() {
        why.push(format!("names {} and {}", na.trim(), nb.trim()));
    }
    if !shared.is_empty() {
        let conf = if same_name || within {
            Confidence::Likely
        } else {
            Confidence::Check
        };
        return Some((conf, why));
    }
    if !same_name {
        // one name inside another, alone, is nothing: first names are common
        return None;
    }
    Some((
        if full_name && !conflict {
            Confidence::Likely
        } else {
            Confidence::Check
        },
        why,
    ))
}

/// The keys a card is looked up by, so a large book is not compared pair by
/// pair.
fn keys(c: &Contact) -> Vec<String> {
    let mut k: Vec<String> = c
        .values(Multi::Email)
        .iter()
        .map(|v| format!("e:{}", v.value.trim().to_lowercase()))
        .collect();
    k.extend(c.values(Multi::Phone).iter().filter_map(|v| {
        let d = model::digits(&v.value);
        (d.len() >= 7).then(|| format!("p:{}", &d[d.len() - 7..]))
    }));
    let name = compact(&name_of(c));
    if !name.is_empty() {
        k.push(format!("n:{name}"));
    }
    k.retain(|x| !x.ends_with(':'));
    k.sort();
    k.dedup();
    k
}

/// Groups within each account, and pairs across accounts. `apart` says
/// which pairs (by full id) were called different people; `linked` which
/// cross-account pairs mirroring already links.
pub fn find(entries: &[Entry], apart: &Apart, linked: &BTreeSet<(String, String)>) -> (Vec<Group>, Vec<Across>) {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (n, e) in entries.iter().enumerate() {
        for k in keys(&e.contact) {
            index.entry(k).or_default().push(n);
        }
    }
    let mut seen = BTreeSet::new();
    let mut edges: Vec<(usize, usize, Confidence, Vec<String>)> = Vec::new();
    let mut across = Vec::new();
    for bucket in index.values() {
        // a key hundreds share (a company's switchboard) is no reason alone
        if bucket.len() > 200 {
            continue;
        }
        for (x, &a) in bucket.iter().enumerate() {
            for &b in &bucket[x + 1..] {
                let (a, b) = (a.min(b), a.max(b));
                if !seen.insert((a, b)) {
                    continue;
                }
                let (ea, eb) = (&entries[a], &entries[b]);
                if apart.has(&ea.id(), &eb.id()) {
                    continue;
                }
                let Some((conf, why)) = edge(&ea.contact, &eb.contact) else {
                    continue;
                };
                if ea.account == eb.account {
                    edges.push((a, b, conf, why));
                } else if !linked.contains(&(ea.contact.id().to_string(), eb.contact.id().to_string())) {
                    across.push(Across { a, b, why });
                }
            }
        }
    }
    // groups: joined by likely pairs only
    let mut parent: Vec<usize> = (0..entries.len()).collect();
    fn root(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for (a, b, conf, _) in &edges {
        if *conf != Confidence::Likely {
            continue;
        }
        let (ra, rb) = (root(&mut parent, *a), root(&mut parent, *b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();
    let mut pairs: Vec<Group> = Vec::new();
    for (a, b, conf, why) in edges {
        if conf == Confidence::Check {
            // a pair of its own, unless one likely group holds both already
            if root(&mut parent, a) != root(&mut parent, b) {
                pairs.push(Group {
                    members: vec![a, b],
                    confidence: Confidence::Check,
                    why,
                });
            }
            continue;
        }
        let r = root(&mut parent, a);
        let g = groups.entry(r).or_insert_with(|| Group {
            members: Vec::new(),
            confidence: Confidence::Likely,
            why: Vec::new(),
        });
        for m in [a, b] {
            if !g.members.contains(&m) {
                g.members.push(m);
            }
        }
        for w in why {
            if !g.why.contains(&w) {
                g.why.push(w);
            }
        }
    }
    let mut out: Vec<Group> = groups.into_values().chain(pairs).collect();
    for g in &mut out {
        g.members.sort_by_key(|&m| entries[m].contact.sort_key());
    }
    out.sort_by_key(|g| (g.confidence, entries[g.members[0]].contact.sort_key()));
    across.sort_by_key(|p| entries[p.a].contact.sort_key());
    (out, across)
}

// ---- which card is kept ----

/// How much a card holds: its fields, entries and photo.
fn weight(c: &Contact) -> usize {
    let s = Shared::of(c);
    let scalars = [
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
    ]
    .iter()
    .filter(|f| !s.scalar(**f).is_empty())
    .count();
    scalars + s.phones.len() + s.emails.len() + s.urls.len() + s.addresses.len() + usize::from(c.has_photo())
}

/// When a card was last changed, as the account says (ISO text sorts).
fn modified(c: &Contact) -> String {
    let own = c.get("dateModified");
    if !own.is_empty() {
        return own.to_string();
    }
    c.0.get(crate::google::ORIGINAL)
        .and_then(|p| p["metadata"]["sources"].as_array())
        .and_then(|s| s.iter().filter_map(|x| x["updateTime"].as_str()).max())
        .unwrap_or_default()
        .to_string()
}

/// Of a group's cards, the one kept: the one with more in it, then the more
/// recently changed.
pub fn keeper(cards: &[&Contact]) -> usize {
    (0..cards.len())
        .max_by(|&a, &b| {
            (weight(cards[a]), modified(cards[a]))
                .cmp(&(weight(cards[b]), modified(cards[b])))
                // of two the same, the first
                .then(b.cmp(&a))
        })
        .unwrap_or(0)
}

// ---- the merge ----

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
            .map(|s| compact(s))
            .collect::<Vec<_>>()
    };
    key(a) == key(b)
}

fn person_parts(s: &Shared) -> [&str; 5] {
    [&s.prefix, &s.first, &s.middle, &s.last, &s.suffix]
}

fn full_name(s: &Shared) -> String {
    person_parts(s)
        .iter()
        .filter(|p| !p.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The fuller of two names: more parts, then more letters.
fn fuller(a: &Shared, b: &Shared) -> bool {
    let rank = |s: &Shared| (words(&full_name(s)).len(), compact(&full_name(s)).chars().count());
    rank(b) > rank(a)
}

fn also(lines: &mut Vec<String>, line: String) {
    if !lines.contains(&line) {
        lines.push(line);
    }
}

/// `kept` and `other` as one card: see the module's header. `kept` is the
/// card that stays; the result is what it should hold.
pub fn merge_into(kept: &Shared, other: &Shared) -> Shared {
    let mut out = kept.clone();
    let mut extra: Vec<String> = Vec::new();

    // the fuller name is kept; the other, when more than a spelling of it,
    // becomes the nickname or a line in the notes
    let alt = if fuller(kept, other) {
        out.prefix = other.prefix.clone();
        out.first = other.first.clone();
        out.middle = other.middle.clone();
        out.last = other.last.clone();
        out.suffix = other.suffix.clone();
        full_name(kept)
    } else {
        full_name(other)
    };

    // single fields: taken when missing, else the other's in the notes
    for (f, name) in [
        (Field::Nickname, "Nickname"),
        (Field::Company, "Company"),
        (Field::Department, "Department"),
        (Field::JobTitle, "Job title"),
        (Field::Birthday, "Birthday"),
    ] {
        let (mine, theirs) = (out.scalar(f).to_string(), other.scalar(f).trim().to_string());
        if theirs.is_empty() {
            continue;
        }
        if mine.trim().is_empty() {
            set(&mut out, f, theirs);
        } else if compact(&mine) != compact(&theirs) || (compact(&mine).is_empty() && mine.trim() != theirs) {
            also(&mut extra, format!("Also: {name}: {theirs}"));
        }
    }

    let chosen = full_name(&out);
    if !alt.trim().is_empty() && compact(&alt) != compact(&chosen) && !name_within(&alt, &chosen) {
        if out.nickname.trim().is_empty() {
            out.nickname = alt.trim().to_string();
        } else if compact(&out.nickname) != compact(&alt) {
            also(&mut extra, format!("Also: {}", alt.trim()));
        }
    }

    // notes: both, once
    let (mine, theirs) = (out.notes.trim().to_string(), other.notes.trim().to_string());
    let mut notes = if theirs.is_empty() || mine.contains(&theirs) {
        mine
    } else if mine.is_empty() || theirs.contains(&mine) {
        theirs
    } else {
        format!("{mine}\n\n{theirs}")
    };
    let extra: Vec<String> = extra.into_iter().filter(|l| !notes.contains(l.as_str())).collect();
    if !extra.is_empty() {
        if !notes.is_empty() {
            notes.push_str("\n\n");
        }
        notes.push_str(&extra.join("\n"));
    }
    out.notes = notes;

    // every phone, email, URL and address, each once
    for m in [Multi::Phone, Multi::Email, Multi::Url] {
        let list = match m {
            Multi::Phone => &mut out.phones,
            Multi::Email => &mut out.emails,
            Multi::Url => &mut out.urls,
        };
        for l in other.values(m) {
            if !list.iter().any(|o: &Labeled| same_value(m, &o.value, &l.value)) {
                list.push(l.clone());
            }
        }
    }
    for a in &other.addresses {
        if !out.addresses.iter().any(|o| same_address(o, a)) {
            out.addresses.push(a.clone());
        }
    }
    out
}

fn set(s: &mut Shared, f: Field, v: String) {
    match f {
        Field::First => s.first = v,
        Field::Middle => s.middle = v,
        Field::Last => s.last = v,
        Field::Prefix => s.prefix = v,
        Field::Suffix => s.suffix = v,
        Field::Nickname => s.nickname = v,
        Field::Company => s.company = v,
        Field::JobTitle => s.job_title = v,
        Field::Department => s.department = v,
        Field::Birthday => s.birthday = v,
        Field::Notes => s.notes = v,
    }
}

/// A whole group merged into `cards[keep]`.
pub fn merge_all(cards: &[&Contact], keep: usize) -> Shared {
    let mut out = Shared::of(cards[keep]);
    for (n, c) in cards.iter().enumerate() {
        if n != keep {
            out = merge_into(&out, &Shared::of(c));
        }
    }
    out
}

// ---- pairs called different people ----

/// Pairs the person called different people, by full id, in
/// `<data>/apart.json`: never reported again.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Apart {
    #[serde(default)]
    pub pairs: BTreeSet<(String, String)>,
}

impl Apart {
    fn key(a: &str, b: &str) -> (String, String) {
        if a <= b {
            (a.into(), b.into())
        } else {
            (b.into(), a.into())
        }
    }

    pub fn has(&self, a: &str, b: &str) -> bool {
        self.pairs.contains(&Self::key(a, b))
    }

    pub fn load(root: &Path) -> Result<Apart> {
        Ok(icloud_app::store::read_json(&root.join("apart.json"))?.unwrap_or_default())
    }

    /// Remembers every pair of `ids` as different people.
    pub fn add(root: &Path, ids: &[String]) -> Result<()> {
        let _lock = icloud_app::store::lock(root, "apart.lock")?;
        let mut a = Apart::load(root)?;
        for (x, i) in ids.iter().enumerate() {
            for j in &ids[x + 1..] {
                a.pairs.insert(Self::key(i, j));
            }
        }
        Ok(icloud_app::store::write_json(&root.join("apart.json"), &a)?)
    }
}

// ---- doing it ----

/// What a merge writes through: the book, or a stand-in in tests.
pub trait Writer {
    fn update(&self, e: &Entry, changes: &[Change]) -> Result<Contact>;
    fn delete(&self, e: &Entry) -> Result<Option<String>>;
    /// After the kept card is saved: whatever else the account keeps of the
    /// others that a card cannot carry (Google's contact groups).
    fn carry(&self, _kept: &Entry, _others: &[&Entry]) -> Result<Option<String>> {
        Ok(None)
    }
}

impl Writer for crate::book::Book {
    fn update(&self, e: &Entry, changes: &[Change]) -> Result<Contact> {
        Ok(crate::book::Book::update(self, e, changes)?.value)
    }
    fn delete(&self, e: &Entry) -> Result<Option<String>> {
        crate::book::Book::delete(self, e)
    }
    fn carry(&self, kept: &Entry, others: &[&Entry]) -> Result<Option<String>> {
        let account = self.account(&kept.account)?;
        let others: Vec<&Contact> = others.iter().map(|e| &e.contact).collect();
        account.provider_impl.carry_over(&kept.contact, &others)
    }
}

/// What one merge did.
#[derive(Debug, Clone, Serialize)]
pub struct Done {
    pub kept: String,
    pub name: String,
    pub deleted: Vec<String>,
    pub backup: PathBuf,
    pub warnings: Vec<String>,
}

/// `<data>/merged-backup/<YYYY-MM-DD>.vcf`, today's (UTC).
pub fn backup_path(root: &Path) -> PathBuf {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    root.join("merged-backup").join(format!("{}.vcf", civil(days as i64)))
}

/// A day count since 1970-01-01 as `YYYY-MM-DD` (Howard Hinnant's).
fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", if m <= 2 { y + 1 } else { y })
}

/// Appends `cards` as vCards to the backup and syncs it to disk.
fn back_up(path: &Path, cards: &[&Contact]) -> Result<()> {
    std::fs::create_dir_all(path.parent().expect("a file in a folder"))?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let mut text = String::new();
    for c in cards {
        text.push_str(&crate::vcard::card(c));
    }
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Merges `group` (entries of one account) into `group[keep]`: the backup,
/// then the kept card, then the others deleted, in that order (see the
/// module's header). A failure before the deletions leaves every contact;
/// one during them leaves those not yet deleted, merged into the kept one
/// already, so running it again deletes them.
/// `across` allows the contacts to be in two accounts (`merge A B
/// --keep ACCOUNT`); `merge --all` never merges across.
pub fn apply(w: &dyn Writer, root: &Path, group: &[&Entry], keep: usize, across: bool) -> Result<Done> {
    if group.len() < 2 {
        return Err(Error::Other("a merge needs two contacts or more".into()));
    }
    if !across && group.iter().any(|e| e.account != group[keep].account) {
        return Err(Error::Other(
            "these are in different accounts; mirroring pairs those (`cloud-contacts mirror`)".into(),
        ));
    }
    let cards: Vec<&Contact> = group.iter().map(|e| &e.contact).collect();
    let target = merge_all(&cards, keep);
    let backup = backup_path(root);
    back_up(&backup, &cards)?;
    let kept = group[keep];
    let changes = Shared::of(&kept.contact).changes_to(&target);
    let saved = if changes.is_empty() {
        kept.contact.clone()
    } else {
        w.update(kept, &changes)?
    };
    let others: Vec<&Entry> = group
        .iter()
        .enumerate()
        .filter(|(n, _)| *n != keep)
        .map(|(_, e)| *e)
        .collect();
    let mut warnings = Vec::new();
    let kept_after = Entry {
        contact: saved.clone(),
        ..kept.clone()
    };
    if let Some(warning) = w.carry(&kept_after, &others)? {
        warnings.push(warning);
    }
    let mut deleted = Vec::new();
    for o in &others {
        if let Some(warning) = w.delete(o)? {
            warnings.push(warning);
        }
        deleted.push(o.id());
    }
    Ok(Done {
        kept: kept.id(),
        name: saved.display_name(),
        deleted,
        backup,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_labeled;

    fn card(changes: &[Change]) -> Contact {
        let mut c = Contact::new(false);
        c.apply(changes).unwrap();
        c
    }
    fn first(s: &str) -> Change {
        Change::Set(Field::First, s.into())
    }
    fn last(s: &str) -> Change {
        Change::Set(Field::Last, s.into())
    }
    fn phone(p: &str) -> Change {
        Change::Add(Multi::Phone, parse_labeled(p, "MOBILE"))
    }
    fn email(e: &str) -> Change {
        Change::Add(Multi::Email, parse_labeled(e, "HOME"))
    }
    fn field(f: Field, v: &str) -> Change {
        Change::Set(f, v.into())
    }

    #[test]
    fn a_local_number_is_its_international_form() {
        assert!(model::same_number("0408 400701", "+358408400701"));
        assert!(model::same_number("00358 40 8400701", "040 8400701"));
        assert!(!model::same_number("0408 400701", "+358 40 8400702"));
        let a = card(&[first("Aino"), phone("0408 400701")]);
        let b = card(&[first("Aino"), last("Virta"), phone("+358408400701")]);
        assert_eq!(edge(&a, &b).unwrap().0, Confidence::Likely);
        let m = merge_into(&Shared::of(&a), &Shared::of(&b));
        assert_eq!(m.phones.len(), 1, "one number, not two");
    }

    #[test]
    fn the_fuller_name_is_kept_and_nothing_is_lost() {
        let a = card(&[first("Tomas"), phone("+1 555 0100 222")]);
        let b = card(&[
            first("Tomas"),
            last("Reyes"),
            phone("555 0100 222"),
            email("tomas@example.com"),
        ]);
        let (conf, _) = edge(&a, &b).unwrap();
        assert_eq!(conf, Confidence::Likely);
        let m = merge_into(&Shared::of(&a), &Shared::of(&b));
        assert_eq!((m.first.as_str(), m.last.as_str()), ("Tomas", "Reyes"));
        assert_eq!(m.nickname, "", "Tomas is inside Tomas Reyes: no nickname");
        assert_eq!(m.emails.len(), 1);
        assert_eq!(m.phones.len(), 1);
    }

    #[test]
    fn punctuation_does_not_make_another_name() {
        let a = card(&[first("Remy"), last("St-Laurent"), email("remy@example.org")]);
        let b = card(&[first("Remy"), last("St. Laurent"), email("REMY@example.org")]);
        assert_eq!(edge(&a, &b).unwrap().0, Confidence::Likely);
        let m = merge_into(&Shared::of(&a), &Shared::of(&b));
        assert_eq!((m.nickname.as_str(), m.notes.as_str()), ("", ""));
    }

    #[test]
    fn a_name_in_another_script_becomes_the_nickname_or_a_note() {
        let a = card(&[first("Dmitriy"), last("Orlov"), phone("+7 900 111 2233")]);
        let b = card(&[first("Дмитрий"), phone("8 900 111 2233")]);
        let m = merge_into(&Shared::of(&a), &Shared::of(&b));
        assert_eq!(m.nickname, "Дмитрий");
        let c = card(&[first("Dmitriy"), last("Orlov"), field(Field::Nickname, "Dima")]);
        let m = merge_into(&Shared::of(&c), &Shared::of(&b));
        assert_eq!(m.nickname, "Dima");
        assert!(m.notes.contains("Also: Дмитрий"), "{}", m.notes);
    }

    #[test]
    fn a_field_both_have_differently_goes_to_the_notes() {
        let a = card(&[
            first("Lena"),
            last("Park"),
            field(Field::Company, "Northwind"),
            field(Field::Notes, "Met at the fair"),
        ]);
        let b = card(&[
            first("Lena"),
            last("Park"),
            field(Field::Company, "Contoso"),
            field(Field::JobTitle, "Engineer"),
            field(Field::Notes, "Met at the fair"),
        ]);
        let m = merge_into(&Shared::of(&a), &Shared::of(&b));
        assert_eq!(m.company, "Northwind");
        assert_eq!(m.job_title, "Engineer", "taken: the kept card had none");
        assert_eq!(m.notes, "Met at the fair\n\nAlso: Company: Contoso");
        // merged again, nothing doubles
        let again = merge_into(&m, &Shared::of(&b));
        assert_eq!(again.notes, m.notes);
    }

    #[test]
    fn notes_are_joined_once() {
        let a = card(&[first("Ola"), field(Field::Notes, "Likes tea")]);
        let b = card(&[first("Ola"), field(Field::Notes, "Has a dog")]);
        assert_eq!(
            merge_into(&Shared::of(&a), &Shared::of(&b)).notes,
            "Likes tea\n\nHas a dog"
        );
    }

    fn entries(cards: Vec<Contact>) -> Vec<Entry> {
        cards
            .into_iter()
            .map(|contact| Entry {
                account: "icloud".into(),
                provider: "icloud".into(),
                contact,
                me: false,
            })
            .collect()
    }

    #[test]
    fn four_cards_of_one_person_are_one_group() {
        let e = entries(vec![
            card(&[
                first("Joost"),
                Change::Set(Field::Middle, "Pieter".into()),
                last("Brink"),
                phone("+31 6 1234 5678"),
            ]),
            card(&[
                first("Joost"),
                Change::Set(Field::Middle, "Pieter".into()),
                last("Brink"),
                email("joost@example.nl"),
            ]),
            card(&[
                first("Joost"),
                Change::Set(Field::Middle, "Pieter".into()),
                last("Brink"),
                phone("06 1234 5678"),
                email("joost@example.nl"),
            ]),
            card(&[
                first("Joost"),
                Change::Set(Field::Middle, "Pieter".into()),
                last("Brink"),
            ]),
            card(&[first("Someone"), last("Else")]),
        ]);
        let (groups, across) = find(&e, &Apart::default(), &BTreeSet::new());
        assert!(across.is_empty());
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].members.len(), 4);
        assert_eq!(groups[0].confidence, Confidence::Likely);
        let cards: Vec<&Contact> = groups[0].members.iter().map(|&m| &e[m].contact).collect();
        let keep = keeper(&cards);
        assert_eq!(cards[keep].values(Multi::Email).len(), 1, "the fullest card is kept");
        let m = merge_all(&cards, keep);
        assert_eq!((m.phones.len(), m.emails.len()), (1, 1));
    }

    #[test]
    fn the_same_first_name_alone_is_a_check_and_apart_is_remembered() {
        let e = entries(vec![
            card(&[first("Nadia"), phone("+44 7700 900111")]),
            card(&[first("nádia"), phone("+44 7700 900222")]),
        ]);
        let (groups, _) = find(&e, &Apart::default(), &BTreeSet::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].confidence, Confidence::Check);
        let dir = tempfile::tempdir().unwrap();
        Apart::add(dir.path(), &[e[0].id(), e[1].id()]).unwrap();
        let apart = Apart::load(dir.path()).unwrap();
        assert!(find(&e, &apart, &BTreeSet::new()).0.is_empty());
    }

    /// A first name inside fuller names is no reason on its own: four Sams
    /// with their own numbers are four people.
    #[test]
    fn first_names_inside_others_are_not_duplicates() {
        let e = entries(vec![
            card(&[first("Sam"), phone("+61 400 000 001")]),
            card(&[first("Sam"), last("Gasparro"), phone("+61 400 000 002")]),
            card(&[first("Sam (Dep Owner)"), phone("+61 400 000 003")]),
            card(&[first("Sam (Bitcoin Trader)"), phone("+61 400 000 004")]),
            card(&[first("Jack"), phone("+1 555 020 0001")]),
            card(&[first("Jack (Club)"), phone("+1 555 020 0002")]),
        ]);
        let (groups, across) = find(&e, &Apart::default(), &BTreeSet::new());
        assert!(groups.is_empty(), "{groups:?}");
        assert!(across.is_empty());
        assert!(edge(&e[4].contact, &e[5].contact).is_none());
    }

    /// A check pair joins nothing: the likely group stays as it is, and the
    /// third card is a pair of its own.
    #[test]
    fn a_check_pair_never_pulls_a_third_card_in() {
        let e = entries(vec![
            card(&[first("Lee"), last("Ford"), phone("+1 555 030 0001")]),
            card(&[
                first("Lee"),
                last("Ford"),
                phone("555 030 0001"),
                email("lee@example.com"),
            ]),
            card(&[first("Toronto"), last("Office"), email("lee@example.com")]),
        ]);
        let (groups, _) = find(&e, &Apart::default(), &BTreeSet::new());
        assert_eq!(groups.len(), 2, "{groups:?}");
        let likely = groups.iter().find(|g| g.confidence == Confidence::Likely).unwrap();
        assert_eq!(likely.members.len(), 2);
        assert!(!likely.members.contains(&2));
        let check = groups.iter().find(|g| g.confidence == Confidence::Check).unwrap();
        assert_eq!(check.members, vec![1, 2]);
    }

    #[test]
    fn spaces_punctuation_and_accents_make_no_other_name() {
        assert_eq!(compact("Cold Room"), compact("Coldroom"));
        assert_eq!(compact("José Núñez"), compact("Jose Nunez"));
        assert_eq!(compact("Zoë-Łucja"), compact("zoe lucja"));
        let (one, two) = (card(&[first("Coldroom")]), card(&[first("Cold"), last("Room")]));
        assert_eq!(edge(&one, &two).map(|e| e.0), Some(Confidence::Likely));
        assert_eq!(
            edge(&two, &one).map(|e| e.0),
            Some(Confidence::Likely),
            "either way round"
        );
    }

    #[test]
    fn the_same_full_name_with_different_numbers_is_a_check() {
        let e = entries(vec![
            card(&[first("Sam"), last("Lee"), phone("+1 555 010 0001")]),
            card(&[first("Sam"), last("Lee"), phone("+1 555 010 0002")]),
        ]);
        assert_eq!(
            find(&e, &Apart::default(), &BTreeSet::new()).0[0].confidence,
            Confidence::Check
        );
    }

    struct Fake {
        log: std::cell::RefCell<Vec<String>>,
        fail_update: bool,
        fail_delete: bool,
    }
    impl Writer for Fake {
        fn update(&self, e: &Entry, changes: &[Change]) -> Result<Contact> {
            self.log.borrow_mut().push(format!("update {}", e.id()));
            if self.fail_update {
                return Err(Error::Other("offline".into()));
            }
            let mut c = e.contact.clone();
            c.apply(changes).map_err(Error::Other)?;
            Ok(c)
        }
        fn delete(&self, e: &Entry) -> Result<Option<String>> {
            self.log.borrow_mut().push(format!("delete {}", e.id()));
            if self.fail_delete {
                return Err(Error::Other("offline".into()));
            }
            Ok(None)
        }
    }

    #[test]
    fn the_backup_comes_first_and_nothing_is_deleted_before_the_kept_card_is_saved() {
        let e = entries(vec![
            card(&[
                first("Ines"),
                phone("+351 912 345 678"),
                field(Field::Notes, "Neighbour"),
            ]),
            card(&[
                first("Ines"),
                last("Costa"),
                phone("912 345 678"),
                email("ines@example.pt"),
            ]),
        ]);
        let group: Vec<&Entry> = e.iter().collect();
        let dir = tempfile::tempdir().unwrap();
        // the kept card can't be saved: nothing is deleted, the backup is there
        let w = Fake {
            log: Default::default(),
            fail_update: true,
            fail_delete: false,
        };
        assert!(apply(&w, dir.path(), &group, 1, false).is_err());
        assert_eq!(*w.log.borrow(), vec![format!("update {}", e[1].id())]);
        let backup = std::fs::read_to_string(backup_path(dir.path())).unwrap();
        assert_eq!(backup.matches("BEGIN:VCARD").count(), 2);
        assert!(backup.contains("Costa") && backup.contains("912 345 678"));
        // all well: update, then delete
        let w = Fake {
            log: Default::default(),
            fail_update: false,
            fail_delete: false,
        };
        let done = apply(&w, dir.path(), &group, 1, false).unwrap();
        assert_eq!(
            *w.log.borrow(),
            vec![format!("update {}", e[1].id()), format!("delete {}", e[0].id())]
        );
        assert_eq!(done.deleted, vec![e[0].id()]);
        assert_eq!(
            std::fs::read_to_string(&done.backup)
                .unwrap()
                .matches("BEGIN:VCARD")
                .count(),
            4
        );
    }

    #[test]
    fn a_merge_across_accounts_is_refused() {
        let mut e = entries(vec![card(&[first("Kai")]), card(&[first("Kai")])]);
        e[1].account = "google".into();
        let group: Vec<&Entry> = e.iter().collect();
        let dir = tempfile::tempdir().unwrap();
        let w = Fake {
            log: Default::default(),
            fail_update: false,
            fail_delete: false,
        };
        assert!(apply(&w, dir.path(), &group, 0, false).is_err());
        assert!(w.log.borrow().is_empty());
        assert!(!backup_path(dir.path()).exists());
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(20_411), "2025-11-19");
    }
}
