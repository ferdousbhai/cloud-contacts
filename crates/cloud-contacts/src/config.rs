//! `~/.config/cloud-contacts/config.toml`: the linked accounts.
//!
//! ```toml
//! [accounts.icloud]
//!
//! [accounts.google]
//!
//! [accounts.work]
//! provider = "google"
//! ```
//!
//! An account's name prefixes its contacts' IDs (`icloud:…`, `work:people/c12`)
//! and names its cache directory. Nothing secret is kept here: iCloud's
//! sign-in is icloud-session's, Google's is gws's, in
//! `~/.config/cloud-contacts/gws/<name>`.
//!
//! On a first run with no file, accounts come from what this computer
//! already had (see [`first_run`]): Cloud Contacts 0.3's `config.json`, or,
//! for someone coming from iCloud for Omarchy's Contacts app, the iCloud
//! account icloud-session is signed in to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct AccountConfig {
    /// Which provider serves it ("icloud" or "google"); defaults to the account's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Google: gws's directory for it, when not the default
    /// `~/.config/cloud-contacts/gws/<name>` (kept from Cloud Contacts 0.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gws_dir: Option<PathBuf>,
}

impl AccountConfig {
    pub fn provider<'a>(&'a self, name: &'a str) -> &'a str {
        self.provider.as_deref().filter(|p| !p.is_empty()).unwrap_or(name)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Config {
    /// Set once the first run looked for accounts to bring in, so an account
    /// removed later is not linked again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first_run_done: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, AccountConfig>,
}

/// Providers Cloud Contacts knows how to link.
pub const PROVIDERS: &[(&str, &str)] = &[
    (
        "icloud",
        "iCloud Contacts, through the iCloud sign-in icloud-session shares with your other apps",
    ),
    ("google", "Google Contacts, through Google's Workspace CLI `gws`"),
];

/// Account names become ID prefixes and directory names: short lowercase words.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"));
    base.join("cloud-contacts")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn path() -> PathBuf {
    config_dir().join("config.toml")
}

/// The config, or an empty one when there is no file yet.
pub fn load() -> Result<Config> {
    read(&path())
}

pub fn read(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|e| Error::Config(format!("could not parse {}: {e}", path.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(Error::Config(format!("could not read {}: {e}", path.display()))),
    }
}

fn save(config: &Config) -> Result<()> {
    let p = path();
    let text = toml::to_string(config).map_err(|e| Error::Config(e.to_string()))?;
    write_private(&p, text.as_bytes()).map_err(|e| Error::Config(format!("could not write {}: {e}", p.display())))
}

/// Loads, changes and saves the config under a lock on `config.lock` beside
/// it, so two changes at once (the app and the command line, or two
/// sign-ins) cannot drop each other's accounts.
pub fn update<T>(change: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let p = path();
    let dir = p.parent().expect("a directory").to_path_buf();
    private_dir(&dir).map_err(|e| Error::Config(format!("could not create {}: {e}", dir.display())))?;
    let _lock = icloud_app::store::lock(&dir, "config.lock")
        .map_err(|e| Error::Config(format!("could not lock {}: {e}", dir.display())))?;
    let mut config = read(&p)?;
    let out = change(&mut config)?;
    save(&config)?;
    Ok(out)
}

/// Creates a directory (and its parents) readable only by you.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Writes a file only you can read, replacing it in one step.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(&tmp)?, contents)?;
    std::fs::rename(&tmp, path)
}

/// Writes a file the person asked for (an export) in one step: a temporary file beside it, synced to disk, then
/// renamed over it, so that a write that fails or is cut short leaves the file that was there as it was. A directory
/// that cannot hold the temporary file fails the write; the file is never truncated and rewritten in place instead.
///
/// The file keeps the permissions it had, and a new one gets what the umask gives any new file: it is the person's
/// own, not this app's private data. A link is followed, so the file it points to is the one replaced, and what is
/// no regular file (`/dev/stdout`, a pipe) is written straight to, as nothing of it can be lost.
pub fn write_replacing(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let existing = std::fs::metadata(&target).ok();
    if existing.as_ref().is_some_and(|m| !m.is_file()) {
        return std::fs::OpenOptions::new()
            .write(true)
            .open(&target)?
            .write_all(contents);
    }
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::other("not a file name"))?;
    // Hidden, and named for this process: two exports to one file at once each write their own.
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(existing) = &existing {
            file.set_permissions(existing.permissions())?;
        }
        file.write_all(contents)?;
        // On disk before the rename: a power cut then leaves the old file or the whole new one, not an empty one.
        file.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return written;
    }
    // The rename itself is in the directory; a file system that cannot sync one has still renamed.
    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}

/// Whether icloud-session is signed in (asked only on a first run).
pub type SignedIn = fn() -> bool;

pub fn icloud_signed_in() -> bool {
    icloud_session::status().is_ok_and(|s| s.signed_in)
}

/// The config, after a first run has brought in what this computer already
/// had, when there is no config yet: Cloud Contacts 0.3's accounts
/// (`config.json` beside this file), else an iCloud account when iCloud for
/// Omarchy's Contacts app was used here (its data directory) or
/// icloud-session is signed in. Answers the config and what was brought in,
/// said once.
pub fn first_run(signed_in: SignedIn) -> Result<(Config, Option<String>)> {
    let config = load()?;
    if config.first_run_done || !config.accounts.is_empty() {
        return Ok((config, None));
    }
    update(|config| {
        if config.first_run_done || !config.accounts.is_empty() {
            return Ok((config.clone(), None));
        }
        config.first_run_done = true;
        let old = config_dir().join("config.json");
        let note = if let Some(accounts) = legacy_accounts(&old) {
            let names: Vec<String> = accounts.keys().cloned().collect();
            config.accounts = accounts;
            (!names.is_empty()).then(|| format!("Linked the accounts Cloud Contacts had: {}", names.join(", ")))
        } else if icloud_app::store::data_dir("icloud-contacts").is_some_and(|d| d.is_dir()) || signed_in() {
            config.accounts.insert("icloud".into(), AccountConfig::default());
            Some(
                "Linked your iCloud contacts, through the iCloud sign-in your other apps share; \
                 `cloud-contacts account add google` adds a Google account beside it"
                    .into(),
            )
        } else {
            None
        };
        Ok((config.clone(), note))
    })
}

/// Cloud Contacts 0.3's `config.json`: `{"accounts": {name: {provider,
/// gws_config?}}}`. Names it allowed that are not valid now are left out.
fn legacy_accounts(path: &Path) -> Option<BTreeMap<String, AccountConfig>> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let accounts = v["accounts"].as_object()?;
    let mut out = BTreeMap::new();
    for (name, a) in accounts {
        let provider = a["provider"].as_str().unwrap_or_default();
        let name = name.to_lowercase().replace('_', "-");
        if !valid_name(&name) || !matches!(provider, "icloud" | "google") {
            continue;
        }
        let cfg = AccountConfig {
            provider: (name != provider).then(|| provider.to_string()),
            gws_dir: a["gws_config"].as_str().map(PathBuf::from),
        };
        out.insert(name, cfg);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("icloud") && valid_name("work-2"));
        assert!(!valid_name("") && !valid_name("Work") && !valid_name("2nd") && !valid_name("a:b"));
    }

    #[test]
    fn reads_cloud_contacts_0_3_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        std::fs::write(
            &p,
            r#"{"accounts": {"icloud": {"provider": "icloud", "identity": "x"},
                "Work_Mail": {"provider": "google", "gws_config": "/g/w"},
                "odd": {"provider": "carddav"}}}"#,
        )
        .unwrap();
        let got = legacy_accounts(&p).unwrap();
        assert_eq!(got.keys().collect::<Vec<_>>(), ["icloud", "work-mail"]);
        assert_eq!(got["icloud"], AccountConfig::default());
        assert_eq!(got["work-mail"].provider.as_deref(), Some("google"));
        assert_eq!(got["work-mail"].gws_dir.as_deref(), Some(Path::new("/g/w")));
    }
}
