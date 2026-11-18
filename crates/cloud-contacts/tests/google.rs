//! A Google account beside the iCloud one: the built binary against the fake
//! iCloud server and a fake `gws` (tests/fake-gws) keeping its address book
//! in a file. No Google or Apple account, no browser, a temporary home.

#[path = "../examples/fake_contacts.rs"]
#[allow(dead_code)]
mod fake;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;

use icloud_app::testing::{code, command, envelope, stderr, stdout};
use serde_json::{Value, json};

struct Env {
    base: String,
    icloud: Arc<fake::State>,
    dir: tempfile::TempDir,
}

fn gws_state(dir: &std::path::Path) -> PathBuf {
    dir.join("gws-state.json")
}

/// The iCloud fake, a Google address book of two, and both accounts linked.
fn start() -> Env {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let icloud = Arc::new(fake::State::seeded());
    let s = icloud.clone();
    std::thread::spawn(move || fake::serve_with(listener, s));
    let env = Env {
        base,
        icloud,
        dir: tempfile::tempdir().unwrap(),
    };
    env.set_google(json!({
        "people/c1": {
            "resourceName": "people/c1", "etag": "g1",
            "names": [{"metadata": {"primary": true}, "givenName": "Gus", "familyName": "Grant", "displayName": "Gus Grant"}],
            "phoneNumbers": [{"metadata": {"primary": true, "source": {"type": "CONTACT", "id": "x"}}, "value": "+1 555 0101", "type": "mobile"}],
            "emailAddresses": [{"value": "gus@example.com", "type": "work"}],
            "memberships": [{"contactGroupMembership": {"contactGroupResourceName": "contactGroups/friends"}}],
        },
        "people/c2": {
            "resourceName": "people/c2", "etag": "g2",
            "names": [{"givenName": "Ann", "familyName": "Lee"}],
            "organizations": [{"name": "Google Ann Co"}],
        },
    }));
    env.ok(&["status"]);
    let added = env.json(&["account", "add", "google"]);
    assert_eq!(added["account"], "google");
    assert_eq!(added["hint"], "keep them the same with `cloud-contacts keep-same on`");
    env
}

impl Env {
    fn set_google(&self, people: Value) {
        let state = json!({ "people": people, "seq": 100, "calls": [] });
        std::fs::write(gws_state(self.dir.path()), state.to_string()).unwrap();
    }

    fn google(&self) -> Value {
        serde_json::from_slice(&std::fs::read(gws_state(self.dir.path())).unwrap()).unwrap()
    }

    fn edit_google(&self, f: impl FnOnce(&mut Value)) {
        let mut s = self.google();
        f(&mut s);
        std::fs::write(gws_state(self.dir.path()), s.to_string()).unwrap();
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = command(env!("CARGO_BIN_EXE_cloud-contacts"), &self.base, self.dir.path());
        c.env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env(
                "CLOUD_CONTACTS_GWS_COMMAND",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake-gws"),
            )
            .env("FAKE_GWS_STATE", gws_state(self.dir.path()))
            .env("CLOUD_CONTACTS_BROWSER", "true")
            .env("CLOUD_CONTACTS_SYSTEMCTL", "true")
            .env("FAKE_CONTACTS_QUIET", "1")
            .args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[])
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {} {}", stdout(&out), stderr(&out));
        stdout(&out)
    }

    fn envelope(&self, args: &[&str]) -> Value {
        envelope(self.ok(args))
    }

    fn json(&self, args: &[&str]) -> Value {
        self.envelope(args)["data"].clone()
    }

    fn fails(&self, args: &[&str]) -> (i32, Value) {
        let out = self.run(args);
        let v = envelope(&out.stdout);
        assert_eq!(v["ok"], false, "{args:?}: {v}");
        (code(&out), v["error"].clone())
    }
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn both_accounts_side_by_side() {
    let env = start();
    let all = env.json(&["list"]);
    assert_eq!(
        ids(&all),
        [
            "icloud:ACME-CO",
            "icloud:BOB-BERG",
            "google:people/c1",
            "google:people/c2",
            "icloud:ANN-LEE",
            "icloud:ME-CARD"
        ]
    );
    let gus = &all[2];
    assert_eq!(
        (gus["account"].as_str(), gus["provider"].as_str()),
        (Some("google"), Some("google"))
    );
    assert_eq!(gus["phones"][0], json!({ "label": "mobile", "value": "+1 555 0101" }));
    assert_eq!(gus["emails"][0]["label"], "work");
    assert_eq!(ids(&env.json(&["list", "--account", "google", "--cached"])).len(), 2);
    // Two Ann Lees, one per account: the name is ambiguous, the ID is not.
    let (exit, e) = env.fails(&["show", "ann lee", "--cached"]);
    assert_eq!((exit, e["code"].as_str()), (2, Some("ambiguous")));
    assert!(e["hint"].as_str().unwrap().contains("google:people/c2"), "{e}");
    assert_eq!(
        env.json(&["show", "google:people/c2", "--cached"])["company"],
        "Google Ann Co"
    );
    let text = String::from_utf8_lossy(&env.run(&["list", "--cached", "--styled"]).stdout).into_owned();
    assert!(
        text.contains("Gus Grant  (+1 555 0101, gus@example.com)  [google]"),
        "{text}"
    );
    // gws ran in Cloud Contacts' own directory for the account.
    let calls = env.google()["calls"].clone();
    let dir = env.dir.path().join("config/cloud-contacts/gws/google");
    assert!(
        calls
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["config_dir"] == dir.to_str().unwrap()),
        "{calls}"
    );
}

#[test]
fn a_change_goes_to_its_own_account_only() {
    let env = start();
    // With two accounts, a new contact needs to know where it goes.
    let (exit, e) = env.fails(&["add", "--first", "Dee"]);
    assert_eq!((exit, e["code"].as_str()), (2, Some("usage")));
    let icloud_writes = || {
        env.icloud
            .requests()
            .iter()
            .filter(|r| r["path"] == "/co/contacts/card/")
            .count()
    };
    let added = env.json(&[
        "add",
        "--account",
        "google",
        "--first",
        "Dee",
        "--phone",
        "work=+1 555 0102",
    ]);
    let id = added["contact"]["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("google:people/c"), "{id}");
    let person = &env.google()["people"][id.strip_prefix("google:").unwrap()];
    assert_eq!(person["names"], json!([{"givenName": "Dee"}]));
    assert_eq!(
        person["phoneNumbers"],
        json!([{"value": "+1 555 0102", "type": "work"}])
    );
    assert_eq!(icloud_writes(), 0);

    // An edit keeps what this app does not show: metadata, group memberships.
    let out = env.json(&[
        "edit",
        "google:people/c1",
        "--last",
        "Grant-Lee",
        "--email",
        "home=gus@home.example",
    ]);
    assert_eq!(out["contact"]["last"], "Grant-Lee");
    let gus = &env.google()["people"]["people/c1"];
    assert_eq!(gus["names"][0]["familyName"], "Grant-Lee");
    assert_eq!(gus["phoneNumbers"][0]["metadata"]["source"]["id"], "x");
    assert_eq!(
        gus["memberships"][0]["contactGroupMembership"]["contactGroupResourceName"],
        "contactGroups/friends"
    );
    assert_eq!(gus["emailAddresses"].as_array().unwrap().len(), 2);
    assert_eq!(icloud_writes(), 0);

    let out = env.json(&["delete", "google:people/c2", "--yes"]);
    assert_eq!(out["contact"]["account"], "google");
    assert!(env.google()["people"].get("people/c2").is_none());
    assert!(env.icloud.card("ANN-LEE").is_some());
}

/// Changed on another device since it was read: Google refuses the stale
/// etag, and the edit is made again over the fresh contact, keeping both.
#[test]
fn an_edit_over_a_change_elsewhere_keeps_both() {
    let env = start();
    env.ok(&["refresh"]);
    env.edit_google(|s| {
        let p = &mut s["people"]["people/c1"];
        p["organizations"] = json!([{"name": "Elsewhere Inc"}]);
        p["etag"] = json!("g1-changed");
    });
    let out = env.json(&["edit", "google:people/c1", "--cached", "--nickname", "G"]);
    assert_eq!(out["contact"]["nickname"], "G");
    assert_eq!(out["contact"]["company"], "Elsewhere Inc");
    let updates = env.google()["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["args"][2] == "updateContact")
        .count();
    assert_eq!(updates, 2);
}

#[test]
fn a_failing_account_is_a_warning_beside_the_others() {
    let env = start();
    let out = env.run_with(&["list"], &[("FAKE_GWS_OFFLINE", "1")]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = envelope(&out.stdout);
    assert_eq!(v["data"].as_array().unwrap().len(), 4);
    assert_eq!(v["meta"]["warnings"][0]["account"], "google");
    assert_eq!(v["meta"]["warnings"][0]["code"], "account_unavailable");
    let out = env.run_with(&["list", "--styled"], &[("FAKE_GWS_OFFLINE", "1")]);
    assert!(
        stderr(&out).starts_with("warning: google: Google (google): couldn't reach Google"),
        "{}",
        stderr(&out)
    );
    // That account alone: its failure is the answer.
    let out = env.run_with(&["list", "--account", "google"], &[("FAKE_GWS_OFFLINE", "1")]);
    assert_eq!(code(&out), 5);
}

#[test]
fn signed_out_of_google_is_exit_3_with_its_login() {
    let env = start();
    std::fs::remove_file(env.dir.path().join("config/cloud-contacts/gws/google/credentials.enc")).unwrap();
    let (exit, e) = env.fails(&["list", "--account", "google"]);
    assert_eq!((exit, e["code"].as_str()), (3, Some("account_unauthorized")));
    assert_eq!(e["hint"], "run `cloud-contacts account login google`");
    let status = env.json(&["status"]);
    assert_eq!(status["accounts"][0]["name"], "google");
    assert_eq!(status["accounts"][0]["signed_in"], false);
    env.ok(&["account", "login", "google"]);
    assert_eq!(env.json(&["list", "--account", "google"]).as_array().unwrap().len(), 2);
}

#[test]
fn every_page_is_read() {
    let env = start();
    let out = env.run_with(&["list", "--account", "google"], &[("FAKE_GWS_PAGE", "1")]);
    assert_eq!(envelope(&out.stdout)["data"].as_array().unwrap().len(), 2);
}

#[test]
fn unlinking_google_forgets_its_sign_in_not_its_contacts() {
    let env = start();
    let gws = env.dir.path().join("config/cloud-contacts/gws/google");
    assert!(gws.join("credentials.enc").is_file());
    let (exit, _) = env.fails(&["account", "remove", "google"]);
    assert_eq!(exit, 2, "asks first");
    env.ok(&["account", "remove", "google", "--yes"]);
    assert!(!gws.exists());
    assert_eq!(env.google()["people"].as_object().unwrap().len(), 2);
    assert!(ids(&env.json(&["list"])).iter().all(|i| i.starts_with("icloud:")));
    // A second iCloud account cannot be linked: icloud-session holds one.
    let (exit, e) = env.fails(&["account", "add", "icloud", "--name", "other"]);
    assert_eq!((exit, e["code"].as_str()), (3, Some("not_configured")));
}

#[test]
fn keep_same_needs_both_accounts() {
    let env = start();
    env.ok(&["account", "remove", "google", "--yes"]);
    let (exit, e) = env.fails(&["keep-same"]);
    assert_eq!((exit, e["code"].as_str()), (3, Some("needs_both_accounts")));
    assert_eq!(
        e["message"],
        "keeping contacts the same needs an iCloud and a Google account"
    );
    assert_eq!(
        e["hint"],
        "link the Google one with `cloud-contacts account add google`"
    );
    assert!(env.json(&["status"]).get("keep_same").is_none());
    let help = String::from_utf8_lossy(&env.run(&["--help"]).stdout).into_owned();
    assert!(
        help.contains("keep-same") && help.contains("(needs an iCloud and a Google account)"),
        "{help}"
    );
}

#[test]
fn keep_same_on_copies_both_ways_and_holds_doubts() {
    let env = start();
    let st = env.json(&["keep-same"]);
    assert_eq!((st["on"].as_bool(), st["linked"].as_u64()), (Some(false), Some(0)));
    assert_eq!(env.json(&["status"])["keep_same"]["on"], false);
    // it asks first
    let (exit, e) = env.fails(&["keep-same", "on"]);
    assert_eq!((exit, e["code"].as_str()), (2, Some("usage")));
    assert!(env.google()["people"].as_object().unwrap().len() == 2, "nothing copied");

    let on = env.json(&["keep-same", "on", "--yes"]);
    assert_eq!(on["on"], true);
    // the two Ann Lees share only a name: they wait for the person
    assert_eq!(on["plan"]["doubtful"].as_array().unwrap().len(), 1);
    assert_eq!(on["waiting"][0]["why"], "name Ann Lee");
    assert_eq!(on["report"]["copied_to_google"], 3);
    assert_eq!(on["report"]["copied_to_icloud"], 1);
    let people = env.google()["people"].clone();
    let firsts: Vec<&str> = people
        .as_object()
        .unwrap()
        .values()
        .filter_map(|p| p["names"][0]["givenName"].as_str())
        .collect();
    assert!(firsts.contains(&"Bob") && firsts.contains(&"Mock"), "{firsts:?}");
    let gus: Vec<_> = env
        .icloud
        .requests()
        .into_iter()
        .filter(|r| r["path"] == "/co/contacts/card/" && r["body"]["contacts"][0]["firstName"] == "Gus")
        .collect();
    assert_eq!(gus.len(), 1, "Gus copied to iCloud once");

    // an edit in Google crosses on the next run
    env.ok(&["edit", "google:people/c1", "--job-title", "Pilot"]);
    let run = env.json(&["keep-same", "run"]);
    assert_eq!(run["report"]["changed"], 1);
    let gus_icloud = env.json(&["list", "Gus", "--account", "icloud", "--cached"]);
    assert_eq!(gus_icloud[0]["job_title"], "Pilot");

    // the doubtful pair called one person: linked, not copied
    let waiting = env.json(&["keep-same"])["waiting"][0].clone();
    let i = format!("icloud:{}", waiting["icloud"].as_str().unwrap());
    let g = format!("google:{}", waiting["google"].as_str().unwrap());
    let st = env.json(&["keep-same", "pair", &i, &g]);
    assert_eq!(st["waiting"], json!([]));
    env.ok(&["keep-same", "run"]);
    let anns = env.json(&["list", "ann lee", "--cached"]);
    assert_eq!(anns.as_array().unwrap().len(), 2, "one in each account");
    assert_eq!(anns[0]["company"], anns[1]["company"]);

    // unlinking Google turns it off and says so
    let out = env.json(&["account", "remove", "google", "--yes"]);
    assert!(
        out["note"]
            .as_str()
            .unwrap()
            .contains("keeping iCloud and Google contacts the same is off now"),
        "{out}"
    );
}

#[test]
fn duplicates_and_merge() {
    let env = start();
    let dups = env.json(&["duplicates"]);
    let ann = dups
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["names"][0] == "Ann Lee")
        .expect("the two Ann Lees");
    assert_eq!(ann["why"], json!(["name Ann Lee"]));
    // merge into the iCloud one: Google's company is not taken over iCloud's,
    // and the Google copy goes
    let (exit, _) = env.fails(&["merge", "icloud:ANN-LEE", "google:people/c2"]);
    assert_eq!(exit, 2, "asks first");
    let out = env.json(&[
        "merge",
        "google:people/c2",
        "icloud:ANN-LEE",
        "--keep",
        "icloud",
        "--yes",
    ]);
    assert_eq!(out["contact"]["id"], "icloud:ANN-LEE");
    assert_eq!(out["contact"]["company"], "Acme");
    assert_eq!(out["deleted"]["id"], "google:people/c2");
    assert!(env.google()["people"].get("people/c2").is_none());
    assert!(
        env.json(&["duplicates"])
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["names"][0] != "Ann Lee")
    );
}
