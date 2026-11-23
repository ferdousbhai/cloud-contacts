//! `cloud-contacts-app` opens the Contacts window (GTK 4 with omui's look and
//! widgets and Omarchy's current theme). The package installs it under
//! `/usr/lib/cloud-contacts/`, off `PATH`: `cloud-contacts open` (and
//! `cloud-contacts` with no command), which the desktop entry runs, runs it in
//! its place. Given a command it runs the command line, like `cloud-contacts`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, so no GTK code may reach it (not even through
//! generic code a debug build shares between modules of one crate).

mod window;

use gtk::prelude::*;

/// The application's D-Bus name, which keeps one window for every launch. GLib takes only a name with a dot in it,
/// so this cannot be [`NAME`]; it is the name the app has always had, so a window from before an upgrade is the one
/// a later launch presents.
pub const APP_ID: &str = "com.ferdousbhai.cloudcontacts";

/// The app's one name: the command, the package, the desktop entry, the icon, and the window's Wayland app id
/// (Hyprland's `class`), which [`omui::application`] gives every window of the app.
pub const NAME: &str = "cloud-contacts";

/// The window's strings in each language, English listing them all.
const CATALOGS: &[(&str, &str)] = &[("en", include_str!("../../i18n/en.json"))];

fn main() {
    omui::i18n::register_all(CATALOGS);
    // `cloud-contacts open` runs this binary with no arguments; `open` given here too is the window, not a command.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() && args != ["open"] {
        std::process::exit(cloud_contacts::cli::run().into());
    }
    let app = omui::application(APP_ID, NAME, "Contacts", "", |app| window::Window::new(app).present());
    app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
    app.set_accels_for_action("win.new", &["<Ctrl>n"]);
    app.set_accels_for_action("win.find", &["<Ctrl>f"]);
    app.set_accels_for_action("win.edit", &["<Ctrl>e"]);
    // The program's name alone: `open`, if given, is not for GApplication's own option parsing.
    std::process::exit(app.run_with_args(&[NAME]).get().into());
}

#[cfg(test)]
mod tests {
    /// `en.json` lists exactly the strings the window translates.
    #[test]
    fn the_catalogs_match_the_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        omui::i18n::CatalogCheck::new(root.join("src"), root.join("src/i18n")).assert_ok();
    }
}
