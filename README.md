# mailrs

A Gmail client for the GNOME desktop, written in Rust. It keeps several Gmail
accounts in sync from the system tray, shows them in one inbox or one at a
time, and keeps your mail on your own computer.

![The inbox, with a conversation open](docs/screenshots/inbox.png)

## What it does

- **One inbox for every account**, plus each account's Inbox, Starred, Sent,
  Drafts, and labels. A coloured dot tells accounts apart.
- **Conversations in one view.** Older messages fold down to a line; quoted
  text and signatures are dimmed. HTML mail renders in its own sandbox with
  scripts off and remote images blocked until you ask for them.
- **A Markdown composer.** Replies, reply-all, and forwards thread correctly
  in Gmail. Drafts save to Gmail, so they follow you to your phone.
- **Gmail search**, with its full query syntax, across one account or all.
- **Junk, Trash, and All Mail**, for every account together or one at a
  time, read live from Gmail. Not Junk and Move to Inbox put mail back.
- **Recipient suggestions** in To and Cc, from people you have written to
  and heard from.
- **Undo Send and Send Later.** Sent mail waits a few seconds with an Undo
  button (Preferences sets how long). Send Later, on the arrow next to
  Send, schedules a message; it waits in Send Later and goes out on time
  while mailrs runs, even in the tray.
- **Formatting without Markdown**: a bar for bold, italic, strikethrough,
  links, lists, and quotes. Paste, drop, or insert images into the text.
- **Tray and notifications.** An unread count in the tray and a notification
  for new mail, which opens the thread when clicked.
- **Select several at once** with Ctrl+click, Shift+click, or Ctrl+A, then
  archive, trash, junk, star, mark, or label them together. Every one of
  these can be undone with Ctrl+Z or the Undo button on the toast.
- **Labels.** Add or remove Gmail labels from the toolbar or with `l`.
  Nested labels show as a tree under their account. Create labels from the
  label menu or the account menu; right-click one to rename or delete it.
- **Drag and drop.** Drag mail onto any mailbox or label to move it there.
- **Unsubscribe and Block Sender.** List mail shows an Unsubscribe banner
  that uses the list's one-click link when it has one. Block Sender sends
  the address's future mail to the Trash with a Gmail filter.
- **Rules**: Gmail's filters, listed in plain words, with a form to add
  one. Gmail runs them, so they work with the computer off.
- **Print, View Source, and Open in New Window** from the ⋮ menu, Ctrl+P,
  Ctrl+Alt+U, and a double-click or Ctrl+O.
- **Automatic replies.** Turn Gmail's out-of-office reply on from an
  account's ⋮ menu, with a subject, message, and optional dates. Gmail sends
  it, so it works with the computer off.
- **Apple Mail's shortcuts**, with Ctrl in place of Command, plus Gmail's
  single keys.
- **Preferences** (`Ctrl+,`): group messages into conversations or list each
  one, when to mark as read, remote images, text size, light or dark, a
  default sending account, a Markdown signature per account (or the one you
  already set in Gmail, one click to import), notification
  previews, how often to check and how much mail to keep, and starting at
  login.
- **Light.** In the tray, mailrs uses about 55 MB. A minute after you close
  the window, it restarts itself in the background to give back the memory
  the window used.

| Dark | Writing | Narrow |
|---|---|---|
| ![Dark mode with an HTML email](docs/screenshots/dark.png) | ![Replying in the composer](docs/screenshots/composer.png) | ![The phone-width layout](docs/screenshots/phone.png) |

| Several selected | Automatic reply |
|---|---|
| ![Three conversations selected, with bulk actions](docs/screenshots/selection.png) | ![The automatic reply dialog](docs/screenshots/automatic-reply.png) |

| Send Later | Rules |
|---|---|
| ![A message scheduled for Monday morning](docs/screenshots/send-later.png) | ![A Gmail filter in the Rules dialog](docs/screenshots/rules.png) |

![Preferences](docs/screenshots/preferences.png)

## Try it without an account

```sh
cargo run --release -p mailrs -- --demo
```

Demo mode opens three sample accounts in a throwaway store. Everything works
except talking to Google: search, triage, the composer, and attachments all
run against local sample data.

## Install

mailrs targets Ubuntu 26.04 (GTK 4.20 or newer, libadwaita 1.8, WebKitGTK
6.0) and Rust 1.98.

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libglib2.0-dev-bin
scripts/install.sh
```

The script installs `mailrs` and `mailrs-cli` into `~/.local/bin`, adds the
app to your launcher, and starts it in the tray at login (`NO_AUTOSTART=1`
skips that). `scripts/uninstall.sh` removes it again.

Then open mailrs. The first screen asks for a Google OAuth client ID and
secret, which you create once in your own Google Cloud project;
[docs/setup.md](docs/setup.md) walks through it in about ten minutes. After
that, **Sign In with Google** adds each account.

## Keyboard

Apple Mail's shortcuts work with Ctrl in place of Command. Gmail's single
keys work too, whenever you are not typing.

| Key | Action | Key | Action |
|---|---|---|---|
| `j` / `k` | Next / previous conversation | `Ctrl+R` or `r` | Reply |
| `Ctrl+Alt+A` or `e` | Archive | `Ctrl+Shift+R` or `a` | Reply all |
| `Delete` or `#` | Move to trash | `Ctrl+Shift+F` or `f` | Forward |
| `Ctrl+Shift+J` | Junk | `Ctrl+N` or `c` | New message |
| `Ctrl+Shift+L` or `s` | Star or unstar | `Ctrl+Shift+D` | Send |
| `Ctrl+Shift+U` or `u` | Mark read or unread | `Ctrl+Shift+A` | Attach files |
| `Ctrl+Alt+M` or `l` | Labels | `Ctrl+B` / `Ctrl+I` / `Ctrl+K` | Bold, italic, link |
| `Ctrl+Z` | Undo | `Ctrl+F` or `/` | Search |
| `Ctrl+A` | Select all | `Ctrl+1` to `Ctrl+9` | Open a mailbox |
| `Ctrl+Shift+N` or `F5` | Check for mail | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Text size |
| `Ctrl+O` or double-click | Open in a new window | `Ctrl+P` | Print |
| `Ctrl+Alt+U` | View source | `Ctrl+?` | Every shortcut |

`Ctrl+?` shows them all.

## From the command line

```sh
mailrs --background             # start in the tray, no window
mailrs --compose                # new message
mailrs mailto:ann@example.com   # new message to Ann
```

To make mailrs open `mailto:` links:
`xdg-mime default dev.mailrs.Mailrs.desktop x-scheme-handler/mailto`

The running app also answers D-Bus actions, handy for custom shortcuts:

```sh
gdbus call --session --dest dev.mailrs.Mailrs --object-path /dev/mailrs/Mailrs \
    --method org.gtk.Actions.Activate show-window [] {}
```

The actions are `show-window`, `hide-window`, `compose`, `check`, and `quit`.

`mailrs-cli` drives the same sync core without a window: `account add`,
`sync`, `threads`, `show`, and `triage`. It is handy for debugging.

## Privacy

- mailrs talks only to Google's Gmail API, through an OAuth client that you
  own. Nobody else's server sees your mail.
- Refresh tokens live in the GNOME keyring. The config file holds only the
  client ID and secret, and mailrs writes it readable by you alone.
- Mail is cached in `~/.local/share/mailrs`: the last 30 days, plus
  everything in your inbox. Opening an older thread fetches it on demand.
- Email is shown with JavaScript off, and with remote content blocked twice:
  by a WebKit content filter and by the page's own Content-Security-Policy.
  Loading images is a per-conversation choice.

## How it is built

```
domain/   shared types
gmail/    Gmail REST client, OAuth, quota limiter
store/    SQLite schema and queries
sync/     one sync loop per account: bootstrap, history replay, backfill
cli/      mailrs-cli
app/      the GTK4 and libadwaita app
```

Sync follows Gmail's history API, polling every 30 seconds per account, so a
change made on your phone shows up here within half a minute. When history
runs out, the account re-lists its mail and removes anything deleted in the
gap. The design and its trade-offs are written up in
[docs/superpowers/specs/](docs/superpowers/specs/2026-09-17-gmail-client-design.md).

## Development

```sh
cargo test --workspace                          # about 150 tests, no network
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p mailrs -- --demo                   # the UI with sample data
scripts/smoke.sh                                # by hand, against a real account
```

## The icon

Three envelopes fanned out in the account colours: several inboxes, one app.
The other concepts considered are in
[docs/icon-concepts.png](docs/icon-concepts.png).
