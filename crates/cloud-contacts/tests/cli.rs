//! The built binary's command line against the fake iCloud Contacts server
//! (examples/fake_contacts.rs) through `icloud-session`'s mock mode: no
//! D-Bus, no Apple account, a temporary data directory.

#[path = "../examples/fake_contacts.rs"]
mod fake;

use std::net::TcpListener;
use std::process::Output;
use std::sync::Arc;

use icloud_app::testing::{
    assert_cli_links_no_gtk, assert_no_command_runs_the_app, code, command, envelope, stderr, stdout,
};
use serde_json::{Value, json};

struct Env {
    base: String,
    state: Arc<fake::State>,
    dir: tempfile::TempDir,
}

/// A seeded fake server of its own per test.
fn start() -> Env {
    let env = start_fresh();
    // The first run links the iCloud account (and says so): done here, so
    // the tests read what every later run prints.
    env.ok(&["status"]);
    env
}

/// A server and an empty home: nothing has run yet.
fn start_fresh() -> Env {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(fake::State::seeded());
    let s = state.clone();
    std::thread::spawn(move || fake::serve_with(listener, s));
    Env {
        base,
        state,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Env {
    /// Stdin not a terminal, `--data-dir <tmp>/data`, a home and XDG
    /// directories in the temp dir, and no session bus.
    fn run(&self, args: &[&str]) -> Output {
        command(env!("CARGO_BIN_EXE_cloud-contacts"), &self.base, self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .args(args)
            .env("FAKE_CONTACTS_QUIET", "1")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        stdout(&out)
    }

    /// The `data` of the JSON envelope on stdout (the default when piped).
    fn json(&self, args: &[&str]) -> Value {
        let v = envelope(self.ok(args));
        assert_eq!(v["ok"], true, "{v}");
        v["data"].clone()
    }

    /// Human-readable text: `--styled` forces it even when piped.
    fn styled(&self, args: &[&str]) -> String {
        let mut all = args.to_vec();
        all.push("--styled");
        self.ok(&all)
    }

    /// A failing run: its exit code and the error object of the envelope.
    fn fails(&self, args: &[&str]) -> (i32, Value) {
        let out = self.run(args);
        // A failure leaves stdout to successful output alone: the error object is on stderr.
        assert_eq!(stdout(&out), "", "{args:?}");
        let v = envelope(&out.stderr);
        assert_eq!(v["ok"], false, "{args:?}: {v}");
        (code(&out), v["error"].clone())
    }

    /// A failing run: its exit code and error code.
    fn error(&self, args: &[&str]) -> (i32, String) {
        let (exit, e) = self.fails(args);
        (exit, e["code"].as_str().unwrap().to_owned())
    }

    /// The card writes sent: `(method, body)`, method "" for a create.
    fn writes(&self) -> Vec<(String, Value)> {
        self.state
            .requests()
            .into_iter()
            .filter(|r| r["path"] == "/co/contacts/card/")
            .map(|r| {
                (
                    r["query"]["method"].as_str().unwrap_or("").to_owned(),
                    r["body"].clone(),
                )
            })
            .collect()
    }
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn lists_sorted_by_last_name_and_searches() {
    let env = start();
    assert_eq!(
        names(&env.json(&["list"])),
        ["Acme Hardware", "Bob Berg", "Ann Lee", "Mock User"]
    );
    assert_eq!(names(&env.json(&["list", "acme"])), ["Acme Hardware", "Ann Lee"]);
    // Digits match phone numbers whatever their punctuation.
    assert_eq!(names(&env.json(&["list", "010 0100"])), ["Ann Lee"]);
    let text = env.styled(&["list", "--cached"]);
    assert!(
        text.contains("Ann Lee  (Acme, +1 (555) 010-0100, ann@acme.example)"),
        "{text}"
    );
}

#[test]
fn shows_a_contact_by_name_phone_email_or_me() {
    let env = start();
    let ann = env.json(&["show", "ann lee"]);
    assert_eq!(ann["id"], "icloud:ANN-LEE");
    assert_eq!(
        ann["phones"][0],
        json!({ "label": "mobile", "value": "+1 (555) 010-0100" })
    );
    assert_eq!(ann["addresses"][0]["city"], "Springfield");
    assert_eq!(ann["birthday"], "1990-05-01");
    assert_eq!(ann["has_photo"], true);
    assert_eq!(env.json(&["show", "5550100100", "--cached"])["id"], "icloud:ANN-LEE");
    assert_eq!(
        env.json(&["show", "ANN@acme.example", "--cached"])["id"],
        "icloud:ANN-LEE"
    );
    let me = env.json(&["show", "me", "--cached"]);
    assert_eq!(
        (me["id"].as_str(), me["me"].as_bool()),
        (Some("icloud:ME-CARD"), Some(true))
    );
    let text = env.styled(&["show", "bob", "--cached"]);
    assert!(
        text.contains("phone:     +47 555 01 234  (iPhone)") && text.contains("Met at the conference"),
        "{text}"
    );
}

#[test]
fn not_found_and_ambiguous() {
    let env = start();
    let (exit, e) = env.fails(&["show", "zed"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("not_found")));
    assert!(e["hint"].as_str().unwrap().contains("cloud-contacts list"), "{e}");
    // "a" is in several names: the hint lists them with their IDs.
    let (exit, e) = env.fails(&["show", "a", "--cached"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("ambiguous")));
    let hint = e["hint"].as_str().unwrap();
    assert!(
        hint.contains("Ann Lee  (icloud:ANN-LEE)") && hint.contains("(icloud:ACME-CO)"),
        "{hint}"
    );
    // The same for a command that changes something, and nothing was sent.
    assert_eq!(env.error(&["edit", "zed", "--last", "Z", "--cached"]).0, 1);
    assert_eq!(env.error(&["delete", "a", "--yes", "--cached"]).0, 1);
    assert!(env.writes().is_empty());
}

/// A failure exits 1 and leaves stdout empty, whatever its kind: in text mode it is `error:`/`hint:` lines on
/// stderr, and in every machine mode the error object on stderr, its `code` the kind.
#[test]
fn errors_go_to_stderr_as_text_or_json_and_exit_1() {
    let env = start();
    let out = env.run(&["show", "zed", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (1, ""));
    let err = stderr(&out);
    assert!(err.starts_with("error: no contact matches \"zed\"\nhint: "), "{err}");
    for flag in ["--json", "--quiet", "--ids-only", "--count"] {
        let out = env.run(&["show", "zed", "--cached", flag]);
        let v = envelope(&out.stderr);
        assert_eq!((code(&out), v["ok"].as_bool()), (1, Some(false)), "{flag}");
        assert_eq!(v["error"]["code"], "not_found", "{flag}");
        assert_eq!(stdout(&out), "", "{flag}");
    }
    // --help and --version are no failures, and need no display.
    for flag in ["--help", "--version"] {
        let out = env.run(&[flag]);
        assert_eq!(code(&out), 0, "{flag}: {}", stderr(&out));
        assert!(stdout(&out).contains("cloud-contacts"), "{flag}");
    }
    // A usage error clap finds reads the same way.
    let (exit, e) = env.fails(&["list", "--no-such-flag"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("usage")));
    assert_eq!(e["hint"], "run `cloud-contacts --help`");
}

#[test]
fn the_envelope_ids_and_counts() {
    let env = start();
    let v = envelope(env.ok(&["list"]));
    assert_eq!(
        (v["ok"].as_bool(), v["summary"].as_str()),
        (Some(true), Some("4 contacts"))
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 4);
    // `--json` is the same when piped; `--quiet` is only the data.
    assert_eq!(envelope(env.ok(&["list", "--cached", "--json"])), v);
    assert_eq!(envelope(env.ok(&["list", "--cached", "--quiet"])), v["data"]);
    assert_eq!(
        env.ok(&["list", "--cached", "--ids-only"]),
        "icloud:ACME-CO\nicloud:BOB-BERG\nicloud:ANN-LEE\nicloud:ME-CARD\n"
    );
    assert_eq!(
        env.ok(&["list", "acme", "--cached", "--ids-only"]),
        "icloud:ACME-CO\nicloud:ANN-LEE\n"
    );
    assert_eq!(env.ok(&["list", "--cached", "--count"]), "4\n");
    assert_eq!(env.ok(&["list", "acme", "--cached", "--count"]), "2\n");
    assert_eq!(env.ok(&["list", "nobody", "--cached", "--count"]), "0\n");
    assert_eq!(env.ok(&["list", "nobody", "--cached", "--ids-only"]), "");
    assert_eq!(env.styled(&["list", "nobody", "--cached"]), "No contacts.\n");
    // ids-only wins over count, quiet and json.
    assert_eq!(
        env.ok(&["show", "ann", "--cached", "--json", "--count", "--ids-only"]),
        "icloud:ANN-LEE\n"
    );
    assert_eq!(env.ok(&["show", "ann", "--cached", "--count"]), "1\n");
    assert_eq!(env.ok(&["export", "ann", "bob", "--cached", "--count"]), "2\n");
    assert_eq!(
        env.ok(&["export", "ann", "bob", "--cached", "--ids-only"]),
        "icloud:ANN-LEE\nicloud:BOB-BERG\n"
    );
    // The ids of what a command changed.
    assert_eq!(
        env.ok(&["edit", "bob", "--cached", "--nickname", "B", "--ids-only"]),
        "icloud:BOB-BERG\n"
    );
    assert_eq!(
        env.ok(&["delete", "bob", "--cached", "--yes", "--ids-only"]),
        "icloud:BOB-BERG\n"
    );
    let id = env.ok(&["add", "--first", "Dee", "--ids-only"]);
    assert_eq!(env.json(&["show", id.trim()])["name"], "Dee");
}

#[test]
fn add_finds_its_card_by_id_in_a_changeset_of_others() {
    let env = start();
    env.ok(&["refresh"]);
    // Another device changes Bob after this computer's tokens were issued:
    // the add's answer then lists Bob first.
    env.state
        .put(json!({ "contactId": "BOB-BERG", "firstName": "Bob", "lastName": "Berg", "nickName": "B" }));
    let bob_before = env.state.card("BOB-BERG").unwrap();
    let added = env.json(&[
        "add",
        "--cached",
        "--first",
        "Cleo",
        "--last",
        "Park",
        "--phone",
        "work=+44 20 7946 0000",
        "--email",
        "cleo@example.org",
        "--birthday",
        "07-04",
    ]);
    let c = &added["contact"];
    assert_eq!(c["name"], "Cleo Park");
    assert_eq!(c["phones"][0], json!({ "label": "work", "value": "+44 20 7946 0000" }));
    assert_eq!(c["emails"][0]["label"], "home");
    assert_eq!(c["birthday"], "07-04");
    let id = c["id"].as_str().unwrap();
    let local = id.strip_prefix("icloud:").unwrap();
    assert_eq!(env.state.card(local).unwrap()["birthday"], "1604-07-04");
    // Only the create was written; Bob is as the other device left him.
    assert_eq!(env.writes().len(), 1);
    assert_eq!(env.state.card("BOB-BERG").unwrap(), bob_before);
    // The cache has the new card, and Bob was not overwritten with it.
    assert_eq!(env.json(&["show", "cleo", "--cached"])["id"], id);
    assert_eq!(env.json(&["show", "bob", "--cached"])["name"], "Bob Berg");
}

#[test]
fn add_needs_a_name_phone_or_email() {
    let env = start();
    assert_eq!(env.error(&["add", "--notes", "who?"]), (1, "usage".into()));
    assert_eq!(env.error(&["add", "--phone", "call me"]), (1, "usage".into()));
    assert!(env.writes().is_empty());
    let company = env.json(&["add", "--company", "Globex", "--is-company"]);
    assert_eq!(
        (
            company["contact"]["name"].as_str(),
            company["contact"]["is_company"].as_bool()
        ),
        (Some("Globex"), Some(true))
    );
}

#[test]
fn edit_changes_only_what_it_names() {
    let env = start();
    let out = env.json(&[
        "edit",
        "ann",
        "--job-title",
        "Manager",
        "--nickname",
        "",
        "--phone",
        "home=+1 555 010 0177",
        "--remove-phone",
        "555-010-0199",
        "--remove-email",
        "ANN@ACME.EXAMPLE",
    ]);
    let c = &out["contact"];
    assert_eq!(c["job_title"], "Manager");
    let phones: Vec<&str> = c["phones"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["value"].as_str().unwrap())
        .collect();
    assert_eq!(phones, ["+1 (555) 010-0100", "+1 555 010 0177"]);
    assert_eq!(c["emails"], json!([]));
    // What this tool does not show went back as it was.
    let card = env.state.card("ANN-LEE").unwrap();
    assert_eq!(card["photo"]["signature"], "sig");
    assert_eq!(card["profiles"][0]["user"], "ann");
    assert_eq!(card["streetAddresses"][0]["field"]["city"], "Springfield");
    assert!(card.get("emailAddresses").is_none());
    let (method, body) = &env.writes()[0];
    assert_eq!(method, "PUT");
    assert_eq!(body["contacts"][0]["contactId"], "ANN-LEE");

    let (exit, e) = env.fails(&["edit", "ann", "--remove-phone", "999 999", "--cached"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("error")));
    assert!(
        e["message"].as_str().unwrap().contains("has no phone number 999 999"),
        "{e}"
    );
    assert_eq!(env.error(&["edit", "ann", "--cached"]), (1, "usage".into()));
}

#[test]
fn an_edit_over_a_change_from_another_device_keeps_both() {
    let env = start();
    env.ok(&["refresh"]);
    let mut theirs = env.state.card("ANN-LEE").unwrap();
    theirs["jobTitle"] = json!("Director");
    env.state.put(theirs);
    // The cached etag is now stale: iCloud refuses with 412, and the edit is
    // made again over the fresh card.
    let out = env.json(&["edit", "ann", "--cached", "--last", "Lee-Smith"]);
    assert_eq!(out["contact"]["last"], "Lee-Smith");
    assert_eq!(out["contact"]["job_title"], "Director");
    let puts: Vec<_> = env.writes().into_iter().filter(|(m, _)| m == "PUT").collect();
    assert_eq!(puts.len(), 2);
}

/// A label is changed by removing the value and adding it again: the
/// removal is by digits, whatever the label, so it must not take the value
/// just added with it.
#[test]
fn removing_and_adding_a_number_in_one_edit_relabels_it() {
    let env = start();
    let out = env.json(&[
        "edit",
        "ann",
        "--phone",
        "home=+1 555 010 0199",
        "--remove-phone",
        "555-010-0199",
    ]);
    assert_eq!(
        out["contact"]["phones"],
        json!([
            { "label": "mobile", "value": "+1 (555) 010-0100" },
            { "label": "home", "value": "+1 555 010 0199" },
        ])
    );
}

/// The same edit over a card another device changed meanwhile: the number
/// written anew is not taken for one the fresh card "already has" (the one
/// the same edit removes).
#[test]
fn rewriting_a_number_over_a_change_from_another_device_keeps_it() {
    let env = start();
    env.ok(&["refresh"]);
    let mut theirs = env.state.card("ANN-LEE").unwrap();
    theirs["jobTitle"] = json!("Director");
    env.state.put(theirs);
    let out = env.json(&[
        "edit",
        "ann",
        "--cached",
        "--remove-phone",
        "555-010-0199",
        "--phone",
        "work=+1-555-010-0199",
    ]);
    assert_eq!(out["contact"]["job_title"], "Director");
    assert_eq!(
        out["contact"]["phones"],
        json!([
            { "label": "mobile", "value": "+1 (555) 010-0100" },
            { "label": "work", "value": "+1-555-010-0199" },
        ])
    );
}

#[test]
fn expired_tokens_are_renewed_once() {
    let env = start();
    env.ok(&["refresh"]);
    env.state.expire_tokens();
    let out = env.json(&["edit", "bob", "--cached", "--company", "Fjord AS"]);
    assert_eq!(out["contact"]["company"], "Fjord AS");
    let startups = env
        .state
        .requests()
        .iter()
        .filter(|r| r["path"] == "/co/startup")
        .count();
    assert_eq!(startups, 2, "the sync's, then one for fresh tokens");
}

#[test]
fn delete_asks_or_needs_yes() {
    let env = start();
    // Piped: nobody can be asked, in a machine mode or as text.
    let (exit, e) = env.fails(&["delete", "bob"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("usage")));
    assert_eq!(e["hint"], "pass --yes to delete without asking");
    let out = env.run(&["delete", "bob", "--styled"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).starts_with("error: delete asks before it acts"),
        "{}",
        stderr(&out)
    );
    assert!(env.state.card("BOB-BERG").is_some());
    assert!(env.writes().is_empty());
    let out = env.json(&["delete", "bob", "--yes"]);
    assert_eq!(
        out["contact"],
        json!({ "id": "icloud:BOB-BERG", "account": "icloud", "name": "Bob Berg" })
    );
    assert!(env.state.card("BOB-BERG").is_none());
    assert_eq!(env.state.count(), 3);
    assert_eq!(env.error(&["show", "bob", "--cached"]), (1, "not_found".into()));
}

#[test]
fn exports_vcards() {
    let env = start();
    // As text, the vCards themselves; as JSON, inside the data.
    let text = env.styled(&["export"]);
    assert!(
        text.starts_with("BEGIN:VCARD\r\n") && text.ends_with("END:VCARD\r\n"),
        "{text}"
    );
    assert_eq!(text.matches("BEGIN:VCARD\r\n").count(), 4);
    assert!(
        text.contains("FN:Ann Lee\r\n") && text.contains("TEL;TYPE=CELL:+47 555 01 234\r\n"),
        "{text}"
    );
    let out = env.json(&["export", "--cached"]);
    assert_eq!(
        (out["action"].as_str(), out["count"].as_i64()),
        (Some("export"), Some(4))
    );
    assert_eq!(out["vcard"], text);
    let file = env.dir.path().join("ann.vcf");
    let out = env.json(&["export", "ann", "--cached", "--output", file.to_str().unwrap()]);
    assert_eq!(out["count"], 1);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        written.contains("BDAY:1990-05-01\r\n") && written.contains("UID:ANN-LEE\r\n"),
        "{written}"
    );
}

/// `export -o` replaces a file in one step: the temporary file beside it is renamed over it, the file keeps its
/// permissions, a link still points to the file it did, and nothing is left behind.
#[test]
fn an_export_replaces_its_file_in_one_step() {
    use std::os::unix::fs::PermissionsExt;
    let env = start();
    let dir = env.dir.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("contacts.vcf");
    std::fs::write(&file, "an older export").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let link = dir.join("link.vcf");
    std::os::unix::fs::symlink("contacts.vcf", &link).unwrap();

    env.json(&["export", "bob", "-o", link.to_str().unwrap()]);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        written.starts_with("BEGIN:VCARD\r\n") && written.contains("FN:Bob Berg\r\n"),
        "{written}"
    );
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    let mut left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["contacts.vcf", "link.vcf"]);

    // A new file in the directory the command is run in, named without one.
    let out = command(env!("CARGO_BIN_EXE_cloud-contacts"), &env.base, env.dir.path())
        .env("XDG_CONFIG_HOME", env.dir.path().join("config"))
        .env("FAKE_CONTACTS_QUIET", "1")
        .current_dir(&dir)
        .args(["export", "ann", "--cached", "-o", "ann.vcf"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        std::fs::read_to_string(dir.join("ann.vcf"))
            .unwrap()
            .contains("FN:Ann Lee\r\n")
    );
}

/// A directory that cannot hold the temporary file fails the export, and the file that was there is whole: it is
/// not truncated and rewritten in place, which the file's own permissions would allow.
#[test]
fn an_export_that_cannot_be_written_leaves_the_file_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    let env = start();
    let dir = env.dir.path().join("read-only");
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("contacts.vcf");
    std::fs::write(&file, "an older export").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes where permissions forbid it, and the test would prove nothing: it fails rather than pass so.
    assert!(
        std::fs::write(dir.join("probe"), "").is_err(),
        "this test needs a user that permissions hold back: run the tests as a regular user, not as root"
    );

    let (status, error) = env.fails(&["export", "-o", file.to_str().unwrap()]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(status, 1, "{error}");
    assert!(error["message"].as_str().unwrap().contains("cannot write"), "{error}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "an older export");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn signed_out_fails_with_its_sign_in_hint() {
    let env = start();
    env.ok(&["refresh"]);
    *env.state.signed_out.lock().unwrap() = true;
    for args in [&["list"][..], &["show", "ann"], &["export"], &["refresh"]] {
        let (exit, e) = env.fails(args);
        assert_eq!((exit, e["code"].as_str()), (1, Some("sign_in_required")), "{args:?}");
        let hint = e["hint"].as_str().unwrap();
        assert!(hint.starts_with("run `icloud-session sign-in`"), "{hint}");
    }
    let out = env.run(&["list", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (1, ""));
    assert!(
        stderr(&out).starts_with("error: sign in to iCloud required"),
        "{}",
        stderr(&out)
    );
    // What the last sync left is still readable.
    assert_eq!(env.ok(&["list", "--cached", "--count"]), "4\n");
}

#[test]
fn refresh_counts_changes() {
    let env = start();
    let counts = |v: Value| v["accounts"][0].clone();
    assert_eq!(
        counts(env.json(&["refresh"])),
        json!({ "account": "icloud", "contacts": 4, "changed": 4 })
    );
    assert_eq!(
        counts(env.json(&["refresh"])),
        json!({ "account": "icloud", "contacts": 4, "changed": 0 })
    );
    env.state.put(json!({ "contactId": "NEW", "firstName": "Dee" }));
    assert_eq!(
        counts(env.json(&["refresh"])),
        json!({ "account": "icloud", "contacts": 5, "changed": 1 })
    );
    assert_eq!(env.styled(&["refresh"]), "icloud: 5 contacts (0 changed)\n");
    // `--count` is how many changed.
    env.state.put(json!({ "contactId": "NEWER", "firstName": "Eve" }));
    assert_eq!(env.ok(&["refresh", "--count"]), "1\n");
}

/// Someone coming from iCloud for Omarchy's Contacts (or signed in to
/// icloud-session) finds their iCloud account linked on the first run, and
/// is told so once.
#[test]
fn the_first_run_links_icloud_and_says_so_once() {
    let env = start_fresh();
    let v = envelope(env.ok(&["status"]));
    assert!(
        v["meta"]["notice"]
            .as_str()
            .unwrap()
            .contains("Linked your iCloud contacts"),
        "{v}"
    );
    assert_eq!(v["data"]["accounts"][0]["name"], "icloud");
    let v = envelope(env.ok(&["status"]));
    assert!(v.get("meta").is_none(), "{v}");
    // Unlinked, it stays unlinked.
    env.ok(&["account", "remove", "icloud", "--yes"]);
    let (exit, e) = env.fails(&["list"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("not_configured")));
}

/// `cloud-contacts` is what agents and scripts run: it must not load GTK.
/// The app binary does need GTK, and neither needs libadwaita: omui draws
/// the window.
#[test]
fn the_command_line_binary_does_not_link_gtk() {
    assert_cli_links_no_gtk(
        env!("CARGO_BIN_EXE_cloud-contacts"),
        option_env!("CARGO_BIN_EXE_cloud-contacts-app"),
        &["libgtk-4."],
        &["libadwaita-1."],
    );
}

/// README.md's Keyboard table has every accelerator the window sets, so a shortcut added there is one a reader
/// can find.
#[test]
fn the_readme_lists_every_shortcut() {
    let main = include_str!("../src/bin/app/main.rs");
    let readme = include_str!("../../../README.md");
    let table = readme
        .split_once("## Keyboard\n")
        .and_then(|(_, rest)| rest.split_once("\n## "))
        .expect("README.md has a Keyboard section")
        .0;
    let mut keys = Vec::new();
    for line in main.lines().filter(|l| l.contains("set_accels_for_action(")) {
        // The quoted strings after the action's name: `"<Ctrl>r", "F5"`.
        for accel in line.split('"').skip(3).step_by(2) {
            let key = match accel.strip_prefix("<Ctrl>") {
                Some(letter) => format!("Ctrl+{}", letter.to_uppercase()),
                None => accel.to_owned(),
            };
            assert!(
                table.lines().any(|row| row.starts_with('|') && row.contains(&key)),
                "{key} ({accel}) is missing from README.md's Keyboard table"
            );
            keys.push(key);
        }
    }
    assert_eq!(keys, ["Ctrl+R", "F5", "Ctrl+N", "Ctrl+F", "Ctrl+E"]);
    // The window's own keys (omui's quit keys and shortcut overlay), and what the dialog's fields take from GTK,
    // which dev/qa/undo.jsonl presses. The overlay lists the same keys as the table.
    let window = include_str!("../src/bin/app/window.rs");
    for row in [
        "Ctrl+W, Ctrl+Q",
        "F1, Ctrl+?",
        "Ctrl+Z, Ctrl+Shift+Z",
        "Ctrl+R, F5",
        "Escape, Alt+Left",
        "Up, Down",
    ] {
        assert!(
            table.contains(&format!("| {row} |")),
            "{row} is missing from README.md's Keyboard table"
        );
        // The overlay has the row's keys on one row too, given to `ShortcutSection::alternatives`.
        let keys: Vec<String> = row.split(", ").map(|key| format!("\"{key}\"")).collect();
        assert!(
            window.contains(&format!("&[{}]", keys.join(", "))),
            "{row} is missing from the shortcut overlay"
        );
    }
    for key in ["Ctrl+N", "Ctrl+E", "Ctrl+F", "Enter", "Alt+S", "Alt+C"] {
        assert!(
            window.contains(&format!("(\"{key}\", ")),
            "{key} is missing from the shortcut overlay"
        );
    }
    assert!(
        window.contains("omui::quit_keys(&self.window)")
            && window.contains("self.shortcuts.install(&self.window, true)")
    );
}

/// `cloud-contacts open`, and no command at all, run the window's binary in their place: in `../lib/cloud-contacts/`
/// from the command line where the package puts it, off `PATH`, and beside it in a build tree. The package's
/// `cloud-contacts-app` on `PATH`, the old command, runs `cloud-contacts open`. Without the window's binary it says so
/// and exits 1.
#[test]
fn open_runs_the_window_beside_it_or_in_lib() {
    // The shared check: with no command, the app beside the binary, then the message without it.
    assert_no_command_runs_the_app(env!("CARGO_BIN_EXE_cloud-contacts"), "cloud-contacts");

    let tmp = tempfile::tempdir().unwrap();
    let (bin, lib) = (tmp.path().join("usr/bin"), tmp.path().join("usr/lib/cloud-contacts"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    let cli = bin.join("cloud-contacts");
    std::fs::copy(env!("CARGO_BIN_EXE_cloud-contacts"), &cli).unwrap();
    let marker = tmp.path().join("ran");
    icloud_app::testing::write_script(
        &lib.join("cloud-contacts-app"),
        &format!("#!/bin/sh\necho \"$#\" > '{}'\nexit 0\n", marker.display()),
    );
    // The old command, beside the installed one as the package puts it, runs `cloud-contacts open`, which runs the
    // window in lib (not the launcher again).
    let launcher = bin.join("cloud-contacts-app");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/cloud-contacts-app"),
        &launcher,
    )
    .unwrap();
    for (program, args) in [(&cli, &["open"][..]), (&cli, &[]), (&launcher, &[])] {
        let _ = std::fs::remove_file(&marker);
        // A test thread forking while the copy was open for writing holds it busy for a moment.
        let out = (0..50)
            .find_map(|_| {
                match std::process::Command::new(program)
                    .args(args)
                    .env("PATH", tmp.path().join("nowhere"))
                    .stdin(std::process::Stdio::null())
                    .output()
                {
                    Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        None
                    }
                    r => Some(r.unwrap()),
                }
            })
            .unwrap();
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        // The window is given no arguments: with none, its binary opens the window rather than run a command.
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "0", "{args:?}");
    }
    // `open` with options is a usage error, not a window.
    let env = start();
    let (exit, e) = env.fails(&["open", "--cached"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("usage")));
}

/// One name, `cloud-contacts`, for the command, the package, the desktop entry, the icon and the window's class; the
/// window's binary is off `PATH`, under /usr/lib/cloud-contacts/, and the desktop entry opens it through the command.
/// The application's D-Bus name stays the one 0.4 had, `com.ferdousbhai.cloudcontacts` (GLib takes no name without
/// a dot), so an upgrade loses nothing keyed by it: a window from before the upgrade is the one a launch after it
/// presents.
#[test]
fn the_app_has_one_name() {
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    let mut files: Vec<String> = std::fs::read_dir(&data)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["cloud-contacts.desktop", "cloud-contacts.svg"]);
    let desktop = std::fs::read_to_string(data.join("cloud-contacts.desktop")).unwrap();
    for line in [
        "Exec=cloud-contacts open",
        "Icon=cloud-contacts",
        "StartupWMClass=cloud-contacts",
    ] {
        assert!(
            desktop.lines().any(|l| l == line),
            "{line} is missing from the desktop entry"
        );
    }
    let recipe = include_str!("../../../packaging/aur/cloud-contacts/PKGBUILD");
    for path in [
        "\"$pkgdir/usr/bin/cloud-contacts\"",
        "\"$pkgdir/usr/lib/cloud-contacts/cloud-contacts-app\"",
        "\"$pkgdir/usr/share/applications/cloud-contacts.desktop\"",
        "\"$pkgdir/usr/share/icons/hicolor/scalable/apps/cloud-contacts.svg\"",
    ] {
        assert!(recipe.contains(path), "the PKGBUILD does not install {path}");
    }
    // The window's binary is off PATH; the old command on it is the launcher, which runs `cloud-contacts open`.
    assert!(
        recipe.contains("install -Dm755 packaging/cloud-contacts-app \"$pkgdir/usr/bin/cloud-contacts-app\""),
        "the PKGBUILD does not install the old command's launcher"
    );
    let main = include_str!("../src/bin/app/main.rs");
    assert!(main.contains(r#"pub const NAME: &str = "cloud-contacts";"#));
    assert!(main.contains(r#"pub const APP_ID: &str = "com.ferdousbhai.cloudcontacts";"#));
    // omui makes NAME the app id of every window added to the app.
    assert!(main.contains("omui::application(APP_ID, NAME, "));
    let window = include_str!("../src/bin/app/window.rs");
    assert!(window.contains(".icon_name(super::NAME)"));
}
