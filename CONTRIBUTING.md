# Contributing

Penguin Mail is one person's mail client, published so others can use it
and learn from it. It does not take pull requests: GitHub lets only
collaborators open them here. This file is for anyone who wants to report
a problem, build the app, or read how it works.

## Reporting a problem

Bug reports and ideas are welcome in
[Issues](https://github.com/c9dev/penguin-mail/issues). A useful bug
report says which version you run (`penguin-mail --version`), what you did,
what you expected, and what happened instead. Leave out message content
and addresses you would not post in public.

To report a security problem, see [SECURITY.md](SECURITY.md) instead of
opening an issue.

## Building from source

You need Rust 1.98 and the development packages:

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libglib2.0-dev-bin gettext
scripts/install.sh
```

This builds and installs into `~/.local`. `scripts/uninstall.sh` removes it
again and leaves your mail and settings alone. A copy built from source
signs in to Google only with a Google client compiled in;
[docs/setup.md](docs/setup.md#building-your-own-copy) says how to give it
one. `cargo run -p mailrs -- --demo` opens the app on sample accounts, with
no client needed.

### The Flatpak

Penguin Mail is not on Flathub. To build and install the Flatpak from
this repository:

```sh
flatpak-builder --user --install --force-clean build-dir \
  packaging/flatpak/io.github.c9dev.PenguinMail.yml
```

The manifest here carries no Google client, so this Flatpak cannot sign in
to Gmail; `flatpak run io.github.c9dev.PenguinMail --demo` shows the app on
sample data. It reads and writes `~/.gnupg` and reaches your gpg-agent, so
signing and encryption use your own keys, and it keeps its mail and
settings under `~/.var/app/io.github.c9dev.PenguinMail`.

## How it is built

```
domain/   shared types, Gmail's label names, categories
gmail/    Gmail REST client, OAuth, quota limiter
store/    SQLite schema and queries
sync/     one sync loop per account: bootstrap, history replay, backfill,
          mail actions, mailbox listing, and each account's Gmail settings
pgp/      OpenPGP mail through the person's own gpg
smime/    S/MIME mail through their gpgsm
ai/       model providers, tool calls, the Claude Code bridge
cli/      penguin-mail-cli
app/      the GTK 4 and libadwaita app
```

Windows and dialogs stay thin. Archiving, flagging, listing a mailbox and
changing an automatic reply each live in one module in `sync`, which the
window and the assistant both call, so the two cannot drift apart. Those
modules take an account lookup and the store, so their tests run against an
in-memory database and a fake Gmail with no window on screen. The terms the
code uses are defined in [CONTEXT.md](CONTEXT.md), and
[AGENTS.md](AGENTS.md) has the conventions and the testing traps.

Sync follows Gmail's history API, polling every 30 seconds per account, so a
change made on your phone shows up within half a minute. When history runs
out, the account re-lists its mail and removes anything deleted in the gap.

## Checks

```sh
cargo test --workspace                                # no network
cargo clippy --workspace --all-targets -- -D warnings
scripts/update-po.sh --check                          # translation template current
scripts/a11y-names.sh                                 # every control has a name
```

CI runs those four on every push, in an Ubuntu 26.04 container set up by
`scripts/ci-deps.sh`, validates the AppStream metainfo and the desktop
entry, and builds and starts the Flatpak. The OpenPGP and S/MIME tests
build a throwaway GnuPG keyring and skip when `gpg` or `gpgsm` is missing.
`PENGUIN_MAIL_REQUIRE_CRYPTO=1`, which CI sets, turns that skip into a
failure.

Release builds mask email addresses in the log, as `d…@example.com`.
Debug builds keep them whole, and `PENGUIN_MAIL_LOG_DETAILS=1` does the
same for an installed copy while you look into a problem.

## Screenshots and the demo video

`scripts/screenshots.sh` retakes every picture in `docs/screenshots` from
the demo, on a hidden display, in about four minutes.

`scripts/demo-video.sh` records the tour the README links to. It starts
GNOME Shell headless in a throwaway session, with its own home, D-Bus and
PipeWire, and records the shell's virtual monitor. A shell extension
loaded only in that session moves a pointer and presses keys, so the
video shows a cursor reaching each control, which the tour finds through
the accessibility tree. The result lands in `target/demo-video`: an MP4, a
WebM, and `poster.png`, which goes to `docs/screenshots/tour.png`.
`scripts/demo-video.sh probe` writes the accessibility tree at each stop
instead, for when a change to the UI breaks the tour.

Both run on the demo's sample accounts, and nothing talks to Google.

## Releasing

`scripts/release.sh` bumps the version, opens the changelog draft in your
editor, writes the store listings' release notes with
`scripts/metainfo.sh`, runs the checks, then commits, tags and pushes. If
the push fails, it takes the tag back off and keeps the release commit,
and running it again pushes that commit.

The tag starts the release workflow:

- It builds the `.deb`, tarball and zip on Ubuntu 26.04 and the rpm on
  Fedora 43, starts the rpm on a hidden display, and publishes them with
  the changelog once CI has passed on the tagged commit.
- The release's `SHA256SUMS` goes out signed as `SHA256SUMS.asc`, with the
  `APT_SIGNING_KEY` secret that also signs the repositories.
- It builds the snap and sends it to the Snap Store's edge channel once the
  `SNAPCRAFT_STORE_CREDENTIALS` secret exists.
- Every package gets the Google client from the
  `PENGUIN_MAIL_GOOGLE_CLIENT_ID` and `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`
  secrets. For the snap, the workflow writes them into
  `snap/snapcraft.yaml` before it builds.

When the release is out, the Package repositories workflow rebuilds the apt
and dnf repositories on GitHub Pages from the five newest releases with
`scripts/apt-repo.sh` and `scripts/rpm-repo.sh`. Run it from the Actions
tab to publish again without a release.

Flathub, once Penguin Mail is there, builds from its own repository,
flathub/io.github.c9dev.PenguinMail. `scripts/flatpak-sources.sh --flathub
vX.Y.Z <dir>` writes the manifest, `cargo-sources.json` and `flathub.json`
for a pull request there, with the Google client from
`packaging/secrets.env`.

## Licence

The code is GPL-3.0-or-later. You may fork it, change it, and publish
your changes under the same licence.
