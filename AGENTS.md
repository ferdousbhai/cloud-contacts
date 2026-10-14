# cloud-contacts

Native GTK4/libadwaita app and Bash-friendly CLI. Python's standard library owns
the application logic; PyGObject is only imported for the desktop UI. The iCloud Rust backend under `icloud/` is product-owned and ships privately;
only icloud-session is shared externally. The private helper and `gws` are
invoked as argument arrays, never shell code. No second authentication store or sync engine.

Run `bin/check` (provider rustfmt/clippy/tests, Python tests, native smoke test when GTK is
available); `.githooks/pre-push` runs it before a push (`git config core.hooksPath .githooks` once per
clone, `NO_CHECK=1` skips it).
Tests use temporary HOME/config and fake CLIs; never read live accounts, launch
the UI on the owner's desktop, or modify their contacts/tasks. Provider writes
are explicit. Preserve unknown fields, account IDs, and partial-result warnings.
