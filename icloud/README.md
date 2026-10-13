# Private iCloud provider

Moved from `contacts/` in icloud-for-omarchy at `bb699d4e713dcb3ea785b3d950908afaf2198f69`. Its
protocol implementation, fixtures, cache format, locks, and tests now belong to
cloud-contacts. Only the shared `icloud-session` library/service remains external.
The compiled `cloud-contacts-icloud` is installed privately beside the Python
application, never on PATH. No legacy app binary/package is needed.

Legacy cache directories remain in use so existing users keep cached data,
sync tokens, and notification history without a destructive migration.
