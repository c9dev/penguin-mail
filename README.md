<div align="center">

<img src="app/data/icons/scalable/apps/dev.penguinmail.PenguinMail.svg" width="128" height="128" alt="">

# Penguin Mail

A Gmail client for the GNOME desktop, written in Rust.

[![CI](https://github.com/c9dev/penguin-mail/actions/workflows/ci.yml/badge.svg)](https://github.com/c9dev/penguin-mail/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/c9dev/penguin-mail?sort=semver&label=release)](https://github.com/c9dev/penguin-mail/releases/latest)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](LICENSE)
[![Rust 1.98](https://img.shields.io/badge/rust-1.98-orange?logo=rust)](https://www.rust-lang.org)
[![GTK 4 and libadwaita 1.8](https://img.shields.io/badge/GTK_4-libadwaita_1.8-4a86cf?logo=gnome)](https://gnome.pages.gitlab.gnome.org/libadwaita/)
[![Ubuntu 26.04](https://img.shields.io/badge/Ubuntu-26.04-e95420?logo=ubuntu&logoColor=white)](https://ubuntu.com)

[Install](#install) · [Features](#features) · [Screenshots](#screenshots) · [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md)

</div>

Penguin Mail keeps several Gmail accounts in sync from the system tray,
shows them in one inbox or one at a time, and keeps your mail on your own
computer. It talks to Google through an OAuth client you own, so no other
server sees your mail.

![The inbox, with a conversation open](docs/screenshots/inbox.png)

## Features

### Reading

- **One inbox for every account**, plus each account's Inbox, Flagged,
  Sent, Drafts and labels. A colored dot tells accounts apart.
- **Conversations in one view.** Older messages fold down to a line, and
  quoted text and signatures are dimmed. HTML mail renders in its own
  sandbox with scripts off and remote images blocked until you ask for them.
- **Categories.** A bar above the inbox splits it into Primary, Updates,
  Promotions and Social, using Gmail's own categories. Categorize Sender
  moves a sender to another category for good.
- **Gmail search** with its full query syntax, across one account or all,
  with suggestions for subjects, people and labels as you type.
- **Junk, Trash and All Mail**, for every account or one, read live from
  Gmail. Delete Forever asks Google for that permission the first time you
  use it, never at sign-in, and asks you to confirm each time.

### Writing

- **A composer that shows formatting as you write.** Recipients are chips,
  Cc and Bcc stay hidden until you want them, and attachments list their
  sizes. Replies and forwards thread correctly in Gmail, and drafts save to
  Gmail with their formatting, so they follow you to your phone.
- **Markdown when you want it.** Format Markdown turns Markdown in the body
  into formatted text, and Edit as Markdown goes back. Paste, drop or insert
  images into the text.
- **Recipient suggestions** from your contacts and the people you have
  written to or heard from.
- **Undo Send and Send Later.** Sent mail waits a few seconds with an Undo
  button. Send Later schedules a message, which goes out on time while
  Penguin Mail runs, even in the tray.
- **An Outbox.** A message that cannot go out waits on this computer through
  a quit and a restart, and Penguin Mail tries again on a widening interval
  and as soon as the network comes back. Problems another try would not fix,
  such as a refused recipient or a message over Gmail's size limit, come
  back to you instead.
- **Templates** you drop in at the cursor, and a spelling check while you
  write.

### Organizing

- **Select several at once** with Ctrl+click, Shift+click or Ctrl+A, then
  archive, trash, junk, flag, mark or label them together. Ctrl+Z undoes
  each one.
- **Flags in seven colors**, as in Apple Mail. The flag syncs through
  Gmail's star, and the color stays on this computer.
- **VIPs.** Their mail gathers in a VIPs mailbox, their rows get a star, and
  notifications can be limited to them.
- **Smart Mailboxes**: saved conditions such as sender, subject, label, age,
  size or attachments, for all accounts or one. They search Gmail, so they
  reach past the mail kept on this computer.
- **Remind Me** takes a conversation out of the inbox and brings it back,
  unread, when you choose. **Follow Up** lists mail you sent that has had no
  answer for three days. **Mute** keeps a noisy thread out of the inbox.
- **Labels** from the toolbar or with `l`, nested as a tree under their
  account. Drag mail onto any mailbox or label to move it there.
- **Export** a conversation or a selection as mbox, or one message as `.eml`.

### Gmail settings

- **Rules**: Gmail's filters, listed in plain words, with a form to add one.
- **Automatic replies** with a subject, message and optional dates.
- **Unsubscribe and Block Sender.** List mail shows an Unsubscribe banner
  that uses the list's one-click link when it has one.
- **Hide My Email.** Make a plus address, such as
  `you+kelp.ember795@gmail.com`, for each site you sign up to, and turn it
  off to send its mail to the Trash. Your real address stays visible inside
  it, so this stops lazy spam, not a determined sender.

Gmail runs all four, so they work with your computer off.

### Security and privacy

- **OpenPGP and S/MIME through your own GnuPG.** A signed message names its
  signer and says how far your trust database or the certificate chain
  vouches for it. An encrypted message opens and says it arrived that way.
  The composer has one Sign and one Encrypt for both standards and offers
  Encrypt once every recipient has a key. Penguin Mail holds no key and asks
  for no passphrase: gpg, gpgsm and their pinentry do.
- **Remote content blocked twice**, by a WebKit content filter and by the
  page's own Content-Security-Policy, with JavaScript off. Loading images is
  a choice per conversation, or per sender.
- **Invitations** show as a card above the message, and you can accept,
  decline or propose another time.

### The assistant

An assistant pane (Ctrl+J) summarizes, sorts, cleans up, drafts replies and
changes settings such as an automatic reply, using the app's own tools. It
runs on a local model (LM Studio, Ollama, or any OpenAI-compatible server),
an Anthropic API key, or your Claude subscription through Claude Code. It
asks before it sends mail or changes Gmail settings, and it is off until you
pick a model. [docs/assistant.md](docs/assistant.md) covers setup.

### On the desktop

- **Tray and notifications.** An unread count in the tray, and new-mail
  notifications with Archive, Mark Read, Delete and Reply buttons.
- **Light on memory.** In the tray Penguin Mail uses about 55 MB. A minute
  after you close the window, it restarts itself in the background to give
  back the memory the window used.
- **Apple Mail's shortcuts** with Ctrl in place of Command, plus Gmail's
  single keys.
- **English and European Portuguese**, chosen in Preferences, with the
  window's controls named for screen readers.

## Screenshots

| Dark | Writing | Narrow |
|---|---|---|
| ![Dark mode with an HTML email](docs/screenshots/dark.png) | ![Replying in the composer](docs/screenshots/composer.png) | ![The phone-width layout](docs/screenshots/phone.png) |

| Several selected | Automatic reply |
|---|---|
| ![Three conversations selected, with bulk actions](docs/screenshots/selection.png) | ![The automatic reply dialog](docs/screenshots/automatic-reply.png) |

| Flags | VIPs |
|---|---|
| ![A conversation flagged blue](docs/screenshots/flags.png) | ![A VIP in the sidebar and the list](docs/screenshots/vips.png) |

| Assistant | Categories |
|---|---|
| ![The assistant listing mail that waits on a reply](docs/screenshots/assistant.png) | ![The inbox narrowed to Promotions](docs/screenshots/categories.png) |

| Send Later | Rules |
|---|---|
| ![A message scheduled for Monday morning](docs/screenshots/send-later.png) | ![A Gmail filter in the Rules dialog](docs/screenshots/rules.png) |

| Hide My Email | Preferences |
|---|---|
| ![Hide My Email, with one address and its switch](docs/screenshots/hide-my-email.png) | ![Preferences](docs/screenshots/preferences.png) |

`scripts/screenshots.sh` retakes all of them from the demo.

## Install

Penguin Mail runs on Ubuntu 26.04, or any distribution with GTK 4.20,
libadwaita 1.8 and WebKitGTK 6.0. The tray icon needs a StatusNotifier host,
which Ubuntu's AppIndicator extension provides.

### From a release (recommended)

Download the `.deb` from the
[latest release](https://github.com/c9dev/penguin-mail/releases/latest) and
install it, replacing `X.Y.Z` with the version:

```sh
sudo apt install ./penguin-mail_X.Y.Z_amd64.deb
```

apt pulls in the libraries it needs.

### Without root

The same release has a tarball and a zip that install under `~/.local`:

```sh
tar xzf penguin-mail-X.Y.Z-x86_64.tar.gz
cd penguin-mail-X.Y.Z-x86_64
./install-files.sh .
```

It starts in the tray at login unless you run it as
`NO_AUTOSTART=1 ./install-files.sh .`. Check a download against the
release's `SHA256SUMS` with `sha256sum -c --ignore-missing SHA256SUMS`.

### From source

You need Rust 1.98 and the development packages:

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libglib2.0-dev-bin gettext
scripts/install.sh
```

This builds and installs into `~/.local`. `scripts/uninstall.sh` removes it
again and leaves your mail and settings alone.

### First run

The first screen asks for a Google OAuth client ID and secret, which you
create once in your own Google Cloud project.
[docs/setup.md](docs/setup.md) walks through it in about ten minutes. After
that, **Sign In with Google** adds each account.

### Updates

An installed Penguin Mail checks GitHub for a new release once a day. When
one is out, it says so in a notification, a banner across the window, and the
tray menu, and **Install** does the rest:

- **From the .deb**, it downloads the new `.deb` and installs it with apt.
  GNOME asks for your password, because apt changes files under `/usr`.
- **From the tarball or from source**, it downloads the new tarball and
  installs it into the same folder as before, with no password.

Every download is checked against the release's `SHA256SUMS` first. Once
the new version is in, Penguin Mail restarts into it. With a message open in
the composer, it waits and shows **Restart** instead, since a draft saves
only when you save it.

**Check for Updates** in the tray menu checks at once. To stop the daily
check, turn off **Check for Updates** under Preferences, Startup. A copy run
with `cargo run` or as the demo never checks.

To update by hand, download the new release and install it the same way as
the first time. For a copy built from source, pull and run
`scripts/install.sh` again.

### Try it without an account

```sh
penguin-mail --demo
```

The demo opens three sample accounts in a throwaway store. Search, triage,
the composer and attachments all work against sample data, and nothing
talks to Google.

## Usage

### Keyboard

Apple Mail's shortcuts work with Ctrl in place of Command. Gmail's single
keys work whenever you are not typing. `Ctrl+?` lists every shortcut.

| Key | Action | Key | Action |
|---|---|---|---|
| `j` / `k` | Next / previous conversation | `Ctrl+R` or `r` | Reply |
| `Ctrl+Alt+A` or `e` | Archive | `Ctrl+Shift+R` or `a` | Reply all |
| `Delete` or `#` | Move to trash | `Ctrl+Shift+F` or `f` | Forward |
| `Ctrl+Shift+J` | Junk | `Ctrl+N` or `c` | New message |
| `Ctrl+Shift+L` or `s` | Flag or unflag | `Ctrl+Shift+D` | Send |
| `Ctrl+Alt+1` to `Ctrl+Alt+7` | Flag color | `Ctrl+Shift+A` | Attach files |
| `Ctrl+Shift+U` or `u` | Mark read or unread | `Ctrl+B` / `Ctrl+I` / `Ctrl+K` | Bold, italic, link |
| `Ctrl+Alt+M` or `l` | Labels | `Ctrl+F` or `/` | Search |
| `Ctrl+Z` | Undo | `Ctrl+1` to `Ctrl+9` | Open a mailbox |
| `Ctrl+A` | Select all | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Text size |
| `Ctrl+Shift+N` or `F5` | Check for mail | `Ctrl+P` | Print |
| `Ctrl+O` or double-click | Open in a new window | `Ctrl+Alt+U` | View source |

### Command line

```sh
penguin-mail --background             # start in the tray, no window
penguin-mail --compose                # new message
penguin-mail mailto:ann@example.com   # new message to Ann
penguin-mail --version
```

To make Penguin Mail open `mailto:` links:

```sh
xdg-mime default dev.penguinmail.PenguinMail.desktop x-scheme-handler/mailto
```

The running app answers D-Bus actions, for custom shortcuts:

```sh
gdbus call --session --dest dev.penguinmail.PenguinMail --object-path /dev/penguinmail/PenguinMail \
    --method org.gtk.Actions.Activate show-window [] {}
```

The actions are `show-window`, `hide-window`, `compose`, `check` and `quit`.

`penguin-mail-cli` drives the same sync core without a window: `account add`,
`sync`, `threads`, `show`, `triage` and `export`.

## Privacy

- Penguin Mail talks only to Google's Gmail API, through an OAuth client you
  own.
- Refresh tokens live in the GNOME keyring. The config file holds only the
  client ID and secret, readable by you alone.
- Mail is cached in `~/.local/share/penguin-mail`: the last 30 days plus
  everything in your inbox. Opening an older thread fetches it on demand.
- The assistant is off until you pick a model. A local model keeps mail on
  your computer; the Anthropic API and Claude Code send what the assistant
  reads to Anthropic. API keys live in the GNOME keyring.

The full policy is in [docs/privacy-policy.md](docs/privacy-policy.md).

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
code uses are defined in [CONTEXT.md](CONTEXT.md).

Sync follows Gmail's history API, polling every 30 seconds per account, so a
change made on your phone shows up within half a minute. When history runs
out, the account re-lists its mail and removes anything deleted in the gap.

## Development

```sh
cargo run -p mailrs -- --demo                         # the UI with sample data
cargo test --workspace                                # no network
cargo clippy --workspace --all-targets -- -D warnings
scripts/update-po.sh --check                          # translation template current
scripts/a11y-names.sh                                 # every control has a name
```

CI runs those four checks on every push, in an Ubuntu 26.04 container set up
by `scripts/ci-deps.sh`. The OpenPGP and S/MIME tests build a throwaway
GnuPG keyring and skip when `gpg` or `gpgsm` is missing.
`PENGUIN_MAIL_REQUIRE_CRYPTO=1`, which CI sets, turns that skip into a
failure. [AGENTS.md](AGENTS.md) has the conventions and the testing traps.

To publish a version, `scripts/release.sh` bumps the version, opens the
changelog draft in your editor, runs the checks, then commits, tags and
pushes. The tag starts the release workflow, which builds the `.deb`,
tarball and zip and publishes them with the changelog.

## Contributing

Bug reports and ideas are welcome in
[Issues](https://github.com/c9dev/penguin-mail/issues). Pull requests are
open to collaborators only. [CONTRIBUTING.md](CONTRIBUTING.md) says what a
useful report holds, and [SECURITY.md](SECURITY.md) says where to send a
vulnerability.

## License

Penguin Mail is free software under the
[GNU General Public License, version 3 or later](LICENSE).

The icon is a penguin holding a letter, drawn on the rounded square and
bevel of the Gruvbox Plus icon pack so it sits among that pack's apps.
