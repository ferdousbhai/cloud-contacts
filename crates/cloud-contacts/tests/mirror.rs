//! Mirroring iCloud and Google, on two in-memory accounts: the merge
//! rules, the guards, turning on, a run killed after every write and run
//! again, and seeded random sequences of edits, additions and deletions on
//! both sides that must converge with nothing lost and nothing doubled.
//!
//! `MODEL_SEED=<n>` runs one seed of the random suite, `MODEL_CASES=<n>`
//! the first n (200 by default, with `--ignored`; 20 in a normal run).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cloud_contacts::account::{Provider, Saved, SyncReport};
use cloud_contacts::error::{Error, Result};
use cloud_contacts::mirror::{self, Decision, Engine, RunError, Shared};
use cloud_contacts::model::{Change, Contact, Field, Multi, parse_labeled};
use cloud_contacts::store::Cache;
use serde_json::json;

/// One clock for both accounts, so "later" means later.
static CLOCK: AtomicU64 = AtomicU64::new(0);

fn tick() -> String {
    let t = CLOCK.fetch_add(1, Ordering::SeqCst) % 86_400;
    format!("2026-11-18T{:02}:{:02}:{:02}Z", t / 3600, t / 60 % 60, t % 60)
}

/// An account in memory: ids, etags and modification times as a real one
/// keeps them; `fail` makes every call fail.
#[derive(Default)]
struct Mem {
    prefix: &'static str,
    contacts: Mutex<Vec<Contact>>,
    seq: AtomicU64,
    fail: AtomicBool,
}

impl Mem {
    fn new(prefix: &'static str) -> Self {
        Mem {
            prefix,
            ..Default::default()
        }
    }

    fn check(&self) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Other(format!("{} is out of reach", self.prefix)));
        }
        Ok(())
    }

    fn stamp(&self, c: &mut Contact) {
        c.0.insert(
            "etag".into(),
            json!(format!("e{}", self.seq.fetch_add(1, Ordering::SeqCst))),
        );
        c.0.insert("dateModified".into(), json!(tick()));
    }

    /// Another device's change, straight to the account.
    fn edit(&self, id: &str, changes: &[Change]) {
        let mut all = self.contacts.lock().unwrap();
        let c = all.iter_mut().find(|c| c.id() == id).unwrap();
        c.apply(changes).unwrap();
        self.stamp(c);
    }

    fn add_direct(&self, changes: &[Change]) -> String {
        self.add(changes, false).unwrap().value.id().to_string()
    }

    fn remove(&self, id: &str) {
        self.contacts.lock().unwrap().retain(|c| c.id() != id);
    }

    fn all(&self) -> Vec<Contact> {
        self.contacts.lock().unwrap().clone()
    }

    fn by_first(&self, first: &str) -> Vec<Contact> {
        self.all()
            .into_iter()
            .filter(|c| c.field(Field::First) == first)
            .collect()
    }
}

impl Provider for Mem {
    fn cache(&self) -> Result<Cache> {
        Ok(Cache::default())
    }

    fn sync(&self) -> Result<(SyncReport, Cache)> {
        self.check()?;
        let contacts = self.all();
        Ok((
            SyncReport::default(),
            Cache {
                contacts,
                ..Cache::default()
            },
        ))
    }

    fn add(&self, changes: &[Change], is_company: bool) -> Result<Saved> {
        self.check()?;
        let mut c = Contact::new(is_company);
        c.0.insert(
            "contactId".into(),
            json!(format!("{}{}", self.prefix, self.seq.fetch_add(1, Ordering::SeqCst))),
        );
        c.apply(changes).map_err(Error::Other)?;
        self.stamp(&mut c);
        self.contacts.lock().unwrap().push(c.clone());
        Ok(Saved {
            value: c,
            warning: None,
        })
    }

    fn update(&self, c: &Contact, changes: &[Change]) -> Result<Saved> {
        self.check()?;
        let mut all = self.contacts.lock().unwrap();
        let cur = all
            .iter_mut()
            .find(|o| o.id() == c.id())
            .ok_or_else(|| Error::Other("gone".into()))?;
        // as the providers do: each change judged as it is applied, over the
        // contact as it is now
        let mut changed = false;
        for ch in changes {
            if !cur.has(ch) {
                cur.apply(std::slice::from_ref(ch)).map_err(Error::Other)?;
                changed = true;
            }
        }
        if changed {
            let mut stamped = cur.clone();
            self.stamp(&mut stamped);
            *cur = stamped;
        }
        Ok(Saved {
            value: cur.clone(),
            warning: None,
        })
    }

    fn delete(&self, c: &Contact) -> Result<Option<String>> {
        self.check()?;
        self.remove(c.id());
        Ok(None)
    }
}

struct World {
    root: tempfile::TempDir,
    icloud: Mem,
    google: Mem,
}

impl World {
    fn new() -> Self {
        World {
            root: tempfile::tempdir().unwrap(),
            icloud: Mem::new("I"),
            google: Mem::new("G"),
        }
    }

    fn engine(&self) -> Engine<'_> {
        Engine::new(self.root.path(), &self.icloud, &self.google)
    }

    fn on(&self) -> mirror::Plan {
        let plan = self.engine().plan().unwrap();
        mirror::turn_on(self.root.path(), &plan, &[]).unwrap();
        plan
    }

    fn run(&self) -> std::result::Result<mirror::Report, RunError> {
        self.engine().run(false)
    }

    /// Both accounts hold the same contacts, linked one to one.
    fn assert_same(&self) {
        let s = mirror::load(self.root.path()).unwrap();
        let (i, g) = (self.icloud.all(), self.google.all());
        assert_eq!(s.links.len(), i.len(), "every iCloud contact linked: {:#?}", s.links);
        assert_eq!(s.links.len(), g.len(), "every Google contact linked");
        for l in &s.links {
            let a = i
                .iter()
                .find(|c| c.id() == l.icloud)
                .expect("linked iCloud contact exists");
            let b = g
                .iter()
                .find(|c| c.id() == l.google)
                .expect("linked Google contact exists");
            assert_eq!(Shared::of(a).sorted(), Shared::of(b).sorted(), "a linked pair differs");
        }
    }
}

fn person(first: &str) -> Vec<Change> {
    vec![Change::Set(Field::First, first.into())]
}

#[test]
fn copies_both_ways_and_edits_cross() {
    let w = World::new();
    let ann = w.icloud.add_direct(&[
        Change::Set(Field::First, "Ann".into()),
        Change::Add(Multi::Phone, parse_labeled("work=+1 555 0100", "MOBILE")),
    ]);
    w.google.add_direct(&person("Gus"));
    w.on();
    let r = w.run().unwrap();
    assert_eq!((r.copied_to_google, r.copied_to_icloud), (1, 1));
    w.assert_same();
    // an edit on one side crosses; the run after that changes nothing
    w.icloud.edit(&ann, &[Change::Set(Field::Last, "Lee".into())]);
    assert_eq!(w.run().unwrap().changed, 1);
    assert_eq!(w.google.by_first("Ann")[0].field(Field::Last), "Lee");
    assert_eq!(w.run().unwrap().changed, 0);
    w.assert_same();
    // a deletion on one side crosses
    let gus = w.google.by_first("Gus")[0].id().to_string();
    w.google.remove(&gus);
    assert_eq!(w.run().unwrap().deleted, 1);
    assert!(w.icloud.by_first("Gus").is_empty());
    w.assert_same();
}

#[test]
fn what_one_side_alone_keeps_is_never_written_to_the_other() {
    let w = World::new();
    let id = w.icloud.add_direct(&person("Ann"));
    w.icloud.edit(&id, &[]);
    {
        let mut all = w.icloud.contacts.lock().unwrap();
        all[0].0.insert("profiles".into(), json!([{"user": "ann"}]));
    }
    w.on();
    w.run().unwrap();
    let copy = &w.google.by_first("Ann")[0];
    assert!(copy.0.get("profiles").is_none());
    // and an edit on Google leaves iCloud's own field alone
    w.google.edit(copy.id(), &[Change::Set(Field::Nickname, "A".into())]);
    w.run().unwrap();
    let ann = &w.icloud.by_first("Ann")[0];
    assert_eq!(ann.field(Field::Nickname), "A");
    assert_eq!(ann.0["profiles"][0]["user"], "ann");
}

#[test]
fn turning_on_pairs_what_is_in_both_and_holds_doubts() {
    let w = World::new();
    let email = |e: &str| Change::Add(Multi::Email, parse_labeled(e, "HOME"));
    // surely one person: same name, same email
    w.icloud
        .add_direct(&[Change::Set(Field::First, "Ann".into()), email("ann@x")]);
    w.google
        .add_direct(&[Change::Set(Field::First, "Ann".into()), email("ANN@x")]);
    // a shared phone but different names: doubtful
    let phone = |p: &str| Change::Add(Multi::Phone, parse_labeled(p, "MOBILE"));
    let bob = w
        .icloud
        .add_direct(&[Change::Set(Field::First, "Bob".into()), phone("+1 555 0100")]);
    let rob = w
        .google
        .add_direct(&[Change::Set(Field::First, "Rob".into()), phone("555 0100")]);
    let plan = w.on();
    assert_eq!(plan.pairs.len(), 1);
    assert_eq!(plan.doubtful.len(), 1);
    assert_eq!((plan.to_google, plan.to_icloud), (0, 0));
    w.run().unwrap();
    // the pair is one contact on each side, not two; the doubt waits
    assert_eq!(w.icloud.by_first("Ann").len(), 1);
    assert_eq!(w.google.by_first("Ann").len(), 1);
    assert!(w.google.by_first("Bob").is_empty() && w.icloud.by_first("Rob").is_empty());
    // called different people: each is copied
    mirror::decide(w.root.path(), &bob, &rob, Decision::Different).unwrap();
    w.run().unwrap();
    assert_eq!(w.google.by_first("Bob").len(), 1);
    assert_eq!(w.icloud.by_first("Rob").len(), 1);
    w.assert_same();
}

#[test]
fn deciding_same_links_a_waiting_pair() {
    let w = World::new();
    let phone = |p: &str| Change::Add(Multi::Phone, parse_labeled(p, "MOBILE"));
    let bob = w
        .icloud
        .add_direct(&[Change::Set(Field::First, "Bob".into()), phone("+1 555 0100")]);
    let rob = w
        .google
        .add_direct(&[Change::Set(Field::First, "Rob".into()), phone("555 0100")]);
    w.on();
    mirror::decide(w.root.path(), &bob, &rob, Decision::Same).unwrap();
    w.run().unwrap();
    w.assert_same();
    assert_eq!(w.icloud.all().len(), 1, "joined, not copied");
}

/// 0.4.0 and 0.4.1 kept the state as keep-same.json: it is taken over,
/// links and all, so nothing is copied twice after the rename.
#[test]
fn the_state_of_keep_same_carries_over() {
    let w = World::new();
    for n in 0..2 {
        w.icloud.add_direct(&person(&format!("P{n}")));
    }
    w.on();
    w.run().unwrap();
    let before = mirror::load(w.root.path()).unwrap();
    assert!(before.on);
    let (new, old) = (w.root.path().join("mirror.json"), w.root.path().join("keep-same.json"));
    std::fs::rename(&new, &old).unwrap();
    let after = mirror::load(w.root.path()).unwrap();
    assert!(new.exists() && !old.exists());
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    w.run().unwrap();
    assert_eq!(w.google.all().len(), 2, "nothing copied twice");
    w.assert_same();
}

#[test]
fn too_many_deletions_stop_until_confirmed() {
    let w = World::new();
    for n in 0..4 {
        w.icloud.add_direct(&person(&format!("P{n}")));
    }
    w.on();
    w.run().unwrap();
    for c in w.icloud.all().iter().take(3) {
        w.icloud.remove(c.id());
    }
    match w.run() {
        Err(RunError::TooManyDeletions(d)) => assert_eq!(d.len(), 3),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.google.all().len(), 4, "nothing deleted");
    let s = mirror::load(w.root.path()).unwrap();
    assert!(s.problem.unwrap().contains("would delete 3"));
    w.engine().run(true).unwrap();
    assert_eq!(w.google.all().len(), 1);
    w.assert_same();
}

#[test]
fn a_failing_account_changes_nothing_on_the_other() {
    let w = World::new();
    w.icloud.add_direct(&person("Ann"));
    w.on();
    w.google.fail.store(true, Ordering::SeqCst);
    assert!(matches!(w.run(), Err(RunError::Account(mirror::Side::Google, _))));
    assert_eq!(w.icloud.all().len(), 1);
    w.google.fail.store(false, Ordering::SeqCst);
    w.run().unwrap();
    w.assert_same();
}

/// Killed right after each write in turn, then run again: the same end,
/// with no contact doubled or lost.
#[test]
fn a_run_killed_after_any_write_ends_the_same_when_run_again() {
    for crash in 1..12 {
        let w = World::new();
        for n in 0..3 {
            w.icloud.add_direct(&person(&format!("I{n}")));
            w.google.add_direct(&person(&format!("G{n}")));
        }
        w.on();
        w.run().unwrap();
        // edits, an addition and a deletion on both sides
        let i0 = w.icloud.by_first("I0")[0].id().to_string();
        w.icloud.edit(&i0, &[Change::Set(Field::Last, "Ex".into())]);
        let g1 = w.google.by_first("G1")[0].id().to_string();
        w.google
            .edit(&g1, &[Change::Add(Multi::Email, parse_labeled("g1@x", "HOME"))]);
        w.icloud.add_direct(&person("I9"));
        w.google.add_direct(&person("G9"));
        let i2 = w.icloud.by_first("G2")[0].id().to_string();
        w.icloud.remove(&i2);
        let mut e = w.engine();
        e.crash_after = Some(crash);
        let _ = e.run(false);
        w.run().unwrap();
        w.run().unwrap();
        w.assert_same();
        for name in ["I0", "I1", "I2", "G0", "G1", "I9", "G9"] {
            assert_eq!(w.icloud.by_first(name).len(), 1, "crash {crash}: {name} once in iCloud");
            assert_eq!(w.google.by_first(name).len(), 1, "crash {crash}: {name} once in Google");
        }
        assert!(
            w.google.by_first("G2").is_empty(),
            "crash {crash}: the deletion crossed"
        );
        assert_eq!(
            w.google.by_first("I0")[0].field(Field::Last),
            "Ex",
            "crash {crash}: the edit crossed"
        );
        assert!(
            w.icloud.by_first("G1")[0].values(Multi::Email).len() == 1,
            "crash {crash}"
        );
    }
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn random_change(rng: &mut Rng, c: &Contact) -> Vec<Change> {
    let phones = ["+1 555 0100", "+1-555-0100", "+1 555 0177", "555 0188"];
    let emails = ["a@x", "A@x", "b@x", "c@y"];
    match rng.below(7) {
        0 => vec![Change::Set(Field::Last, ["Lee", "Berg", ""][rng.below(3)].into())],
        1 => vec![Change::Set(Field::Company, ["Acme", "Globex", ""][rng.below(3)].into())],
        2 => vec![Change::Add(
            Multi::Phone,
            parse_labeled(phones[rng.below(4)], ["MOBILE", "WORK"][rng.below(2)]),
        )],
        3 => vec![Change::Add(Multi::Email, parse_labeled(emails[rng.below(4)], "HOME"))],
        4 => match c.values(Multi::Phone).first() {
            Some(p) => vec![Change::RemoveExact(Multi::Phone, p.clone(), 0)],
            None => vec![],
        },
        5 => vec![Change::Set(
            Field::Notes,
            ["", "met at the fair", "owes me"][rng.below(3)].into(),
        )],
        _ => vec![Change::Addresses(match rng.below(2) {
            0 => vec![],
            _ => vec![cloud_contacts::model::Address {
                city: ["Oslo", "Bali"][rng.below(2)].into(),
                ..Default::default()
            }],
        })],
    }
}

fn run_case(seed: u64) {
    let w = World::new();
    let mut rng = Rng(seed);
    let mut created = Vec::new();
    let mut deleted = Vec::new();
    let mut log = vec![format!("seed {seed}")];
    let mut turned_on = false;
    for step in 0..30 {
        let side = if rng.below(2) == 0 { &w.icloud } else { &w.google };
        let name = if std::ptr::eq(side, &w.icloud) {
            "icloud"
        } else {
            "google"
        };
        match rng.below(10) {
            0..=1 => {
                let first = format!("P{seed}x{step}");
                side.add_direct(&person(&first));
                log.push(format!("{step}: add {first} in {name}"));
                created.push(first);
            }
            2 => {
                let all = side.all();
                if all.is_empty() {
                    continue;
                }
                let c = &all[rng.below(all.len())];
                log.push(format!("{step}: delete {} in {name}", c.field(Field::First)));
                deleted.push(c.field(Field::First).to_string());
                side.remove(c.id());
            }
            3..=6 => {
                let all = side.all();
                if all.is_empty() {
                    continue;
                }
                let c = all[rng.below(all.len())].clone();
                let ch = random_change(&mut rng, &c);
                log.push(format!("{step}: edit {} in {name}: {ch:?}", c.field(Field::First)));
                side.edit(c.id(), &ch);
            }
            _ => {
                if !turned_on {
                    w.on();
                    turned_on = true;
                }
                log.push(format!("{step}: run"));
                if let Err(e) = w.engine().run(true) {
                    panic!("run failed: {e}\n{}", log.join("\n"));
                }
            }
        }
    }
    if !turned_on {
        w.on();
    }
    // doubtful pairs found when it was turned on wait for the person: here
    // they are called different people
    for p in mirror::load(w.root.path()).unwrap().waiting {
        mirror::decide(w.root.path(), &p.icloud, &p.google, Decision::Different).unwrap();
    }
    for _ in 0..2 {
        w.engine().run(true).unwrap();
    }
    let check = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        w.assert_same();
        for first in &created {
            let want = usize::from(!deleted.contains(first));
            assert_eq!(w.icloud.by_first(first).len(), want, "{first} in iCloud");
            assert_eq!(w.google.by_first(first).len(), want, "{first} in Google");
        }
        assert_eq!(w.engine().run(false).unwrap().changed, 0, "settled");
    }));
    if let Err(e) = check {
        eprintln!("{}", log.join("\n"));
        std::panic::resume_unwind(e);
    }
}

fn cases(default: u64) -> Vec<u64> {
    match std::env::var("MODEL_SEED") {
        Ok(s) => vec![s.parse().unwrap()],
        Err(_) => {
            let n = std::env::var("MODEL_CASES")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(default);
            (1..=n).collect()
        }
    }
}

#[test]
fn random_edits_on_both_sides_converge() {
    for seed in cases(20) {
        run_case(seed);
    }
}

#[test]
#[ignore = "deep: random exploration, run with bin/check --deep"]
fn random_edits_on_both_sides_converge_deep() {
    for seed in cases(300) {
        run_case(seed + 1000);
    }
}
