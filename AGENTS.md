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

CLI output, errors and exit codes follow the shared product contract in `cloud_contacts/cli_output.py`
(text on a terminal, JSON envelope when piped; exit 0/1/2 usage/3 auth/4 not found/5 unavailable). Keep that
file identical across product repositories.

## Packaging and releases

```sh
bash packaging/repo/test.sh && python3 packaging/repo/test_arch.py   # installer/release tests (in bin/check)
bin/build-package "$(uname -m)"  # full package build as a regular Arch user
sudo bin/arch-run bin/build-package "$(uname -m)" /output  # disposable native Arch chroot
```

`packaging/aur/cloud-contacts/PKGBUILD` is the package recipe; `bin/build-package` builds it from the
committed source. Pushing a `v*` tag runs `.github/workflows/packages.yml` (Native packages), which
builds and uploads the x86_64 and aarch64 packages as artifacts.

Release: bump `version` in `pyproject.toml`, `cloud_contacts/__init__.py`, `icloud/Cargo.toml` (and
`icloud/Cargo.lock`) and `pkgver` in the PKGBUILD together, tag `vX.Y.Z`, push the tag, draft the
GitHub release (`gh release create vX.Y.Z --draft --verify-tag --generate-notes`), then
`bin/release X.Y.Z`: it waits for the Native packages run, signs both packages and repository
databases with the maintainer key (only in the local keyring, never CI), uploads them with
`install.sh`, `SHA256SUMS` and the signing key, and publishes. Publishing runs
`verify-release.yml`; `bin/verify-release X.Y.Z <arch>` verifies on a native host. Do not run
release scripts unless asked.
