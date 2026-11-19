//! Cloud Contacts: your iCloud and Google contacts side by side, never synced
//! with each other unless you ask. This library is the core ([`book`]: every
//! account's contacts together; [`icloud`] and [`google`]: one kind of account
//! each; [`model`], [`store`], [`vcard`], the edit dialog's [`draft`]; the
//! accounts in [`config`] and [`accounts`]) and the command line ([`cli`]), with
//! no GTK anywhere; the window is the `cloud-contacts-app` binary
//! (`src/bin/app/`), behind the `ui` feature.

pub mod account;
pub mod accounts;
mod agent_skill;
pub mod book;
pub mod cli;
pub mod config;
pub mod draft;
pub mod error;
pub mod google;
pub mod icloud;
pub mod mirror;
pub mod model;
pub mod store;
pub mod vcard;
