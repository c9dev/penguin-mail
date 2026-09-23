# Setting up Penguin Mail

Penguin Mail signs in to Gmail with your Google account. Add each account in
the app, or from the command line.

## Signing in

Choose **Sign In with Google** on the first screen. Your browser opens
Google's sign-in page; sign in and allow the permissions Penguin Mail asks
for. Until Google finishes verifying the app, the page warns that Google has
not verified it. Choose **Advanced**, then continue.

The account appears in the sidebar and starts downloading. For each account
after the first, click **Add Account** at the bottom of the sidebar. From a
terminal, `penguin-mail-cli account add` does the same.

Some features ask Google for more access the first time you use them:
automatic replies and Rules, contacts, the calendar, and Delete Forever. Penguin
Mail asks with a **Grant Access** button, and Google confirms once per
account. Mail keeps syncing throughout.

Accounts you added through the old setup page signed in with a Google Cloud
client of your own, kept in `~/.config/penguin-mail/config.toml`. They keep
working. The next time one of them signs in again, it moves to the app's own
client, and the `[oauth]` section can go once none of them uses it.

## The config file

Penguin Mail needs no config file. To change how often it checks for mail,
how many days of mail it keeps, or how much message text it caches, write
`~/.config/penguin-mail/config.toml`:

```toml
# These are the defaults.
[sync]
poll_seconds = 30
window_days = 30
body_cache_mb = 1024
```

Penguin Mail keeps your refresh tokens in the GNOME keyring, not in this file.

## Building your own copy

A build signs in to Google only when it was compiled with the project's
client, from these variables:

- `PENGUIN_MAIL_GOOGLE_CLIENT_ID`
- `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`
- `PENGUIN_MAIL_MICROSOFT_CLIENT_ID`

`scripts/install.sh` reads them from `packaging/secrets.env` when that file
exists. A copy built without them works in every other way and says so when
you try to add a Google account.

To build with a client of your own instead, make one in a Google Cloud project:
enable the Gmail, People and Calendar APIs, fill in the consent screen with
the values in [google-cloud.md](google-cloud.md), and create an OAuth client of
type **Desktop app**. Put its ID and secret in `packaging/secrets.env`:

```sh
PENGUIN_MAIL_GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com
PENGUIN_MAIL_GOOGLE_CLIENT_SECRET=GOCSPX-...
```

Google issues a desktop client secret to identify the app, and anyone who
downloads a desktop app can read it. Your refresh tokens are what grant access
to mail.

## The command line

```sh
cargo run --release -p mailrs-cli -- sync                 # Ctrl-C to stop
cargo run --release -p mailrs-cli -- threads              # unified inbox
cargo run --release -p mailrs-cli -- threads --account you@gmail.com --label SENT
cargo run --release -p mailrs-cli -- show you@gmail.com <thread-id>
cargo run --release -p mailrs-cli -- triage you@gmail.com <thread-id> archive
```

## Which package

Every package is the same app, built with a cargo feature that says what
kind it is (`packaging-rpm`, `packaging-flatpak`, `packaging-snap`, or
none for the .deb and the tarball). The feature decides where
updates come from and whether skills run.

| | .deb | rpm | Flatpak | Snap |
|---|---|---|---|---|
| Updates | Install in the app, or the apt repository | the dnf repository | Flathub | Snap Store |
| GnuPG | system | system | runtime's `gpg`, on `~/.gnupg` | snap's `gpg`, on `~/.gnupg` |
| Assistant skills | yes | yes | no | no |
| Claude Code, MCP servers run as a command | yes | yes | no | no |
| Tray icon | yes | yes | yes | yes |
| Start at login | autostart file | autostart file | Background portal | snapd autostart |

- **Updates.** The .deb checks GitHub once a day and offers Install,
  which downloads the new .deb and installs it through apt; `apt upgrade`
  brings the same version from the apt repository. The tarball updates
  itself the same way, into its own folder. The rpm leaves updates to
  dnf, and the Flatpak and the snap to their store; Preferences and the
  About window say which.
- **GnuPG.** The Flatpak reaches two places for signing and encryption:
  `~/.gnupg`, read and written, and the gpg-agent socket folder under
  `$XDG_RUNTIME_DIR/gnupg`, read-only, so your own agent and pinentry
  handle passphrases. It also talks to the Secret Service, the tray and
  the notification daemon, and writes to Downloads; the manifest says why
  for each. The snap reaches `~/.gnupg` through a `personal-files` plug.
- **Skills.** A skill's scripts run under bubblewrap, which cannot start
  inside Flatpak's or a strict snap's sandbox. Running them without one
  would hand a skill your mail and keys, so both packages turn skills off
  and say so under Preferences, AI, Skills.
- **Programs on your system.** Claude Code, and an MCP server you add as a
  command, run as programs on your computer. The Flatpak and the snap
  cannot see those programs. A local model, the Anthropic API and MCP
  servers you reach by address work in every package.
- **No AppImage.** WebKit draws HTML mail in helper processes it
  sandboxes with bubblewrap, and that sandbox mounts your system's `/usr`,
  where helpers bundled in an AppImage find none of their libraries.

## Where things live

| What | Where | Override |
|---|---|---|
| Config | `~/.config/penguin-mail/config.toml` | `MAILRS_CONFIG` |
| Mail cache | `~/.local/share/penguin-mail/mailrs.db` | `MAILRS_DATA_DIR` |
| Refresh tokens | GNOME keyring, service `mailrs`, one entry per address | |

The keyring service keeps the app's old name, mailrs, so accounts added
before the rename stay signed in.

The Flatpak keeps its config and mail under
`~/.var/app/io.github.c9dev.PenguinMail/`, in `config/penguin-mail` and
`data/penguin-mail`, and the snap under `~/snap/penguin-mail/current/`, in
`.config/penguin-mail` and `.local/share/penguin-mail`. Moving from the
.deb to one of them starts with an empty store; copy `config.toml` across
to keep your sync settings, then add each account again, since the list
of accounts lives in the store.

`penguin-mail-cli account remove you@gmail.com` deletes an account's local mail and
its keyring entry. To revoke access on Google's side as well, use
<https://myaccount.google.com/permissions>.
