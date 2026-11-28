//! Which Google sign-ins of the other Cloud apps a new account could take
//! over (`accounts::takeovers`, what the window's "Add Google Account…" asks
//! about) and linking with none taken over (`SignIn::Browser`), on the fake
//! `gws` (tests/fake-gws) in a temporary home. The library reads its
//! directories and the gws command from the environment, so this is one test
//! in a binary of its own: nothing else runs beside it.

use std::path::Path;

use cloud_contacts::accounts::{self, SignIn};
use serde_json::{Value, json};

/// A Google sign-in as fake-gws keeps one, in another Cloud app's directory.
fn sibling_sign_in(config: &Path, app_account: &str, status: Value) {
    let dir = config.join(app_account);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".encryption_key"), "key").unwrap();
    std::fs::write(dir.join("credentials.enc"), status.to_string()).unwrap();
}

#[test]
fn the_addresses_to_take_over_and_a_browser_sign_in_that_takes_none() {
    let dir = tempfile::tempdir().unwrap();
    let (config, root) = (dir.path().join("config"), dir.path().join("data"));
    let state = dir.path().join("gws-state.json");
    // SAFETY: the only test in this binary, set before anything reads the environment.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var(
            "CLOUD_CONTACTS_GWS_COMMAND",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake-gws"),
        );
        std::env::set_var("FAKE_GWS_STATE", &state);
        std::env::set_var("CLOUD_CONTACTS_BROWSER", "true");
        std::env::remove_var("CLOUD_CONTACTS_GOOGLE_CLIENT_ID");
        std::env::remove_var("CLOUD_CONTACTS_GOOGLE_CLIENT_SECRET");
        std::env::remove_var("FAKE_GWS_USER");
    }
    let logins = || {
        std::fs::read_to_string(format!("{}.logins", state.display()))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let contacts = "https://www.googleapis.com/auth/contacts";
    let works = |user: &str| json!({"token_valid": true, "user": user, "scopes": ["openid", contacts]});

    assert!(
        accounts::takeovers(&root).unwrap().is_empty(),
        "no other app is signed in"
    );
    sibling_sign_in(&config, "cloudmail/gws/gmail", works("Sam@Gmail.com"));
    sibling_sign_in(&config, "cloudmail/gws/work", works("work@example.com"));
    // The same address in a second app is one choice; a sign-in Google no longer takes, and one without the
    // contacts permission, are none.
    sibling_sign_in(&config, "cloud-calendar/gws/google", works("sam@gmail.com"));
    sibling_sign_in(&config, "cloud-calendar/gws/old", json!({"token_valid": false}));
    sibling_sign_in(
        &config,
        "cloud-calendar/gws/narrow",
        json!({"token_valid": true, "user": "narrow@example.com", "scopes": ["https://www.googleapis.com/auth/calendar.events"]}),
    );
    let both = vec![
        ("sam@gmail.com".to_string(), "Cloudmail"),
        ("work@example.com".to_string(), "Cloudmail"),
    ];
    assert_eq!(accounts::takeovers(&root).unwrap(), both);

    // "Another account…": the browser, with both left where they are.
    let (name, note) = accounts::add(None, "google", &SignIn::Browser, &root).unwrap();
    assert_eq!(
        (name.as_str(), note.as_str()),
        ("google", "signed in to Google as me@gmail.com")
    );
    assert_eq!(logins(), 1);
    assert_eq!(accounts::takeovers(&root).unwrap(), both);

    // One that was chosen is taken over, and is no longer a choice.
    let name = accounts::next_name("google").unwrap();
    let (name, note) = accounts::add(
        Some(&name),
        "google",
        &SignIn::Address("Work@Example.com".into()),
        &root,
    )
    .unwrap();
    assert_eq!(
        (name.as_str(), note.as_str()),
        (
            "google-2",
            "signed in to Google as work@example.com, with the sign-in from Cloudmail"
        )
    );
    assert_eq!(logins(), 1, "no browser sign-in");
    assert_eq!(accounts::takeovers(&root).unwrap(), both[..1]);

    // Only iCloud's own sign-in for iCloud.
    assert!(accounts::add(Some("x"), "icloud", &SignIn::Browser, &root).is_err());

    // A client of your own takes over nothing made with the built-in one.
    // SAFETY: as above.
    unsafe {
        std::env::set_var("CLOUD_CONTACTS_GOOGLE_CLIENT_ID", "own.apps.googleusercontent.com");
        std::env::set_var("CLOUD_CONTACTS_GOOGLE_CLIENT_SECRET", "own");
    }
    assert!(accounts::takeovers(&root).unwrap().is_empty());
}
