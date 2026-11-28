//! `cloud-contacts <command>` runs the command line (see
//! `cloud_contacts::cli`). This binary never calls into GTK, so the linker
//! leaves GTK out of it, and a command starts in milliseconds.
//!
//! `cloud-contacts open`, or no command at all, opens the window by running
//! the window's binary in its place: `cloud-contacts-app` in
//! `../lib/cloud-contacts/` from this binary, where the package installs it
//! (`/usr/lib/cloud-contacts/`), off `PATH`, since `cloud-contacts` is the one
//! command a person runs; else beside it (a build tree). The package's
//! `/usr/bin/cloud-contacts-app` is only the old command kept, which runs
//! `cloud-contacts open`.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args == ["open"] {
        open();
    }
    std::process::exit(cloud_contacts::cli::run().into());
}

/// Where the window's binary is, of the places it can be; the first is named when none is there. Only places beside
/// this binary: a copy of it somewhere else (a build tree, a test's) never runs the window a package installed.
fn app_binary() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let candidates: Vec<PathBuf> = exe
        .iter()
        .flat_map(|exe| {
            let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
            // The package's place first: beside the installed command is the old command's launcher, which runs this
            // binary again.
            [
                dir.join("../lib/cloud-contacts/cloud-contacts-app"),
                dir.join("cloud-contacts-app"),
            ]
        })
        .collect();
    candidates
        .iter()
        .find(|p| p.is_file())
        .or(candidates.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cloud-contacts-app"))
}

/// Runs the window in this process's place, so the launcher and the compositor see the one process; returns only
/// to say it could not.
fn open() -> ! {
    let app = app_binary();
    let err = std::process::Command::new(&app).exec();
    eprintln!(
        "cloud-contacts: cannot open the app ({}: {err}); run `cloud-contacts --help` for the command line",
        app.display()
    );
    std::process::exit(cloud_contacts::cli::EXIT_FAILED.into());
}
