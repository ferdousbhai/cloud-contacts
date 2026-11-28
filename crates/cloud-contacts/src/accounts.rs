//! Linking, signing in to and unlinking accounts: the same steps for the
//! command line and the window.
//!
//! - iCloud: the one sign-in icloud-session holds for every app. Linking asks
//!   it to sign in (its own window) when it isn't; unlinking leaves it signed
//!   in, since other apps share it.
//! - Google: `gws auth login` in the browser, with the built-in OAuth client
//!   (see `google`).

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;

use crate::config::{self, AccountConfig, Config};
use crate::error::{Error, Result};
use crate::google::Google;
use crate::store::Store;

/// How long a browser or window sign-in may take.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// What `status`, `account list` and the window show about an account.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AccountStatus {
    pub name: String,
    pub provider: String,
    pub label: String,
    /// Signed in, as far as this computer knows (no network asked).
    pub signed_in: bool,
    /// What's wrong, or who is signed in.
    pub detail: String,
}

fn session_error(e: icloud_session::Error) -> Error {
    Error::Session(e)
}

/// Asks icloud-session to sign in, unless it is, and waits until it is (or
/// its window closes).
pub fn icloud_sign_in() -> Result<()> {
    if icloud_session::status().map_err(session_error)?.signed_in {
        return Ok(());
    }
    // Watching before asking, so the answer can't come in between.
    let watch = icloud_session::watch().map_err(session_error)?;
    icloud_session::sign_in().map_err(session_error)?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut opened = false;
        for st in watch {
            if st.signed_in {
                let _ = tx.send(Ok(()));
                return;
            }
            if st.signing_in {
                opened = true;
            } else if opened {
                let _ = tx.send(Err(Error::Session(icloud_session::Error::SignInRequired)));
                return;
            }
        }
        let _ = tx.send(Err(Error::Other(
            "iCloud: icloud-session went away during the sign-in".into(),
        )));
    });
    rx.recv_timeout(SIGN_IN_TIMEOUT).unwrap_or_else(|_| {
        Err(Error::Other(
            "iCloud: the sign-in wasn't finished within 10 minutes".into(),
        ))
    })
}

fn google(name: &str, cfg: &AccountConfig, root: &Path) -> Google {
    Google::new(name, cfg, Store::new(root.join(name)))
}

fn sign_in(name: &str, cfg: &AccountConfig, root: &Path) -> Result<String> {
    match cfg.provider(name) {
        "icloud" => {
            icloud_sign_in()?;
            let who = icloud_session::status()
                .map_err(session_error)?
                .apple_id
                .unwrap_or_default();
            Ok(format!("signed in to iCloud as {who}"))
        }
        // The sign-in Cloudmail or Cloud Calendar has on this computer when there is one, else the
        // browser. An account whose own sign-in works always gets the browser: that is how to change
        // to another Google account.
        "google" => {
            let google = google(name, cfg, root);
            if !google.works()
                && let Some((address, app)) = google.adopt()
            {
                return Ok(format!("signed in to Google as {address}, with {app}'s sign-in"));
            }
            google.login()?;
            Ok("signed in to Google".into())
        }
        other => Err(Error::Config(format!("unknown provider {other}"))),
    }
}

/// Links an account of `provider` ("icloud" or "google") and signs in to
/// it; answers its name and what happened.
pub fn add(name: Option<&str>, provider: &str, root: &Path) -> Result<(String, String)> {
    if !matches!(provider, "icloud" | "google") {
        return Err(Error::Config(format!(
            "unknown provider \"{provider}\" (known: icloud, google)"
        )));
    }
    let name = name.map(str::to_string).unwrap_or_else(|| provider.to_string());
    if !config::valid_name(&name) {
        return Err(Error::Config(format!(
            "\"{name}\" can't be an account name: use lowercase letters, digits and dashes, starting with a letter"
        )));
    }
    // Checked before the sign-in, and again after it under the config lock:
    // the sign-in can take minutes, and accounts may be linked meanwhile.
    let free = |config: &Config| {
        if config.accounts.contains_key(&name) {
            return Err(Error::Config(format!("an account named {name} is already linked")));
        }
        if provider == "icloud" && config.accounts.iter().any(|(n, c)| c.provider(n) == "icloud") {
            return Err(Error::Config(
                "iCloud is already linked: icloud-session holds one iCloud sign-in for every app".into(),
            ));
        }
        Ok(())
    };
    free(&config::load()?)?;
    let cfg = AccountConfig {
        provider: (name != provider).then(|| provider.to_string()),
        ..Default::default()
    };
    let note = sign_in(&name, &cfg, root)?;
    config::update(|config| {
        free(config)?;
        config.first_run_done = true;
        config.accounts.insert(name.clone(), cfg);
        Ok(())
    })?;
    Ok((name, note))
}

/// Signs in to a linked account again.
pub fn login(name: &str, root: &Path) -> Result<String> {
    let config = config::load()?;
    let cfg = config
        .accounts
        .get(name)
        .ok_or_else(|| Error::Config(format!("no account named {name}")))?;
    sign_in(name, cfg, root)
}

/// Unlinks an account: forgets its cache and (Google) its sign-in on this
/// computer. Its contacts stay in the account itself.
pub fn remove(name: &str, root: &Path) -> Result<String> {
    let cfg = config::update(|config| {
        config
            .accounts
            .remove(name)
            .ok_or_else(|| Error::Config(format!("no account named {name}")))
    })?;
    let mut note = format!("unlinked {name}; its contacts stay in the account");
    if cfg.provider(name) == "google" {
        match google(name, &cfg, root).forget() {
            Ok(true) => note.push_str(", and this computer's Google sign-in for it is gone"),
            Ok(false) => {}
            Err(e) => note.push_str(&format!("; its Google sign-in could not be removed: {e}")),
        }
    }
    let _ = std::fs::remove_dir_all(root.join(name));
    if matches!(cfg.provider(name), "icloud" | "google")
        && let Some(off) = crate::mirror::account_removed(root)
    {
        note.push_str(&format!("; {off}"));
    }
    Ok(note)
}

/// Each linked account's status, from what this computer knows.
pub fn statuses(config: &Config, root: &Path) -> Vec<AccountStatus> {
    config
        .accounts
        .iter()
        .map(|(name, cfg)| {
            let provider = cfg.provider(name).to_string();
            let (signed_in, detail) = match provider.as_str() {
                "icloud" => match icloud_session::status() {
                    Ok(s) if s.signed_in => (true, s.apple_id.unwrap_or_default()),
                    Ok(_) => (false, "signed out: run `icloud-session sign-in`".to_string()),
                    Err(e) => (false, e.to_string()),
                },
                "google" => {
                    let g = google(name, cfg, root);
                    if g.signed_in() {
                        (true, "signed in".to_string())
                    } else {
                        (false, format!("signed out: run `cloud-contacts account login {name}`"))
                    }
                }
                other => (false, format!("unknown provider {other}")),
            };
            AccountStatus {
                name: name.clone(),
                label: crate::book::label(&provider).to_string(),
                provider,
                signed_in,
                detail,
            }
        })
        .collect()
}
