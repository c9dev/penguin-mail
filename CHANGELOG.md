# Changelog

Each release of Penguin Mail, newest first. GitHub shows the same text on
the [releases page](https://github.com/c9dev/penguin-mail/releases).

## Unreleased

### New

- The About window has a Check for Updates button that shows its progress
  and installs the new version when one is out.
- The main menu has an entry for updates: check, install, or restart into
  the new version.

### Fixed

- The Install, Restart and Show Log buttons on the update banner work.

## 0.1.1 (2026-09-21)

The first public release.

### New

- Penguin Mail keeps itself up to date. Once a day it checks for a new
  version, and one click installs it.
- Install it from a `.deb`, or from a tarball or zip without admin rights.
- `penguin-mail --version` shows which version you have.

### Fixed

- The tray icon no longer disappears after the app has sat in the
  background for a while.
- After you install a new version, the app switches to it on its own
  instead of running the old one until you quit it.

### Already in Penguin Mail

- Every Gmail account in one inbox, kept in sync from the tray.
- Encrypted and signed mail with OpenPGP and S/MIME.
- Undo Send, Send Later, and an outbox that tries again when you are back
  online.
- Flags in seven colors, VIPs, smart mailboxes, reminders and follow-ups.
- Gmail rules, automatic replies and Hide My Email addresses.
- An optional assistant that runs on a model on your own computer, or on
  Claude.
- English and European Portuguese.
