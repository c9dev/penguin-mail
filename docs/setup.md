# Setting up Penguin Mail

Penguin Mail works with Google accounts and with any mail provider that
offers IMAP and SMTP, such as Fastmail, iCloud or Yahoo. Add each account in
the app, or from the command line.

## Adding an account

The first window shows the providers you can add. For each account after the
first, open the main menu and choose **Add Account**. From a terminal,
`penguin-mail-cli account add` does the same for a Google account.

### Google

Choose **Google**. Your browser opens Google's sign-in page; sign in and allow
the permissions Penguin Mail asks for, then come back to the app. Until Google
finishes verifying the app, the page warns that Google has not verified it.
Choose **Advanced**, then continue.

### Other providers

Choose your provider, or **Other**, and type your address. Penguin Mail finds
the server settings for you and says what the provider needs, such as an app
password for iCloud, Fastmail or Yahoo, with a link to the page where you make
one. If it can't find them, **Enter Server Settings** lets you type the
incoming (IMAP) and outgoing (SMTP) servers yourself. The password goes to
your desktop's keyring, never into a file.

The account appears in the sidebar and starts downloading.

### Calendars, contacts and rules on other providers

Penguin Mail looks for an IMAP account's calendar (CalDAV), contacts
(CardDAV) and rules server (ManageSieve) when you add the account, with
the same password. Fastmail, iCloud, Yahoo, Zoho, GMX, WEB.DE, mail.com,
Yandex, mailbox.org and Posteo are known; for another server it asks the
domain's DNS and its well-known addresses. **Preferences > Contacts &
Calendar** lists what it found under **Calendar, Contacts and Rules
Servers**, with **Find Again**, and **Edit** to type a CalDAV or CardDAV
address yourself. A server outside your address's domain waits there for
you to press **Use It** before your password goes to it. Where the server
runs no rules, Penguin Mail runs them on this computer while it is open.

Adding a Google account asks for every permission Penguin Mail uses in that one
visit, so you never see a second consent screen for automatic replies and
Rules, contacts, the calendar and its list of calendars, Google Drive
files, or Delete Forever. Leave a box unticked and
the feature it serves turns off with a reason rather than an error, and
offers **Grant Access** where you would use it: in the calendar sidebar,
on an invitation, in Preferences, or when you first try the feature. An account added
before this way of signing in gets a bar at the top of the mail list that
names what it lacks, until you go through Google's screen once more. Mail
keeps syncing throughout.

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

Penguin Mail keeps your refresh tokens in the GNOME keyring, not in this
file. The Flatpak keeps them in its own store instead, through the Secret
portal; see "Which package" below.

## Building your own copy

A build signs in to Google or Microsoft only when it was compiled with the
project's client, from these variables:

- `PENGUIN_MAIL_GOOGLE_CLIENT_ID`
- `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`
- `PENGUIN_MAIL_MICROSOFT_CLIENT_ID`

The release workflow takes them from the secrets
`PENGUIN_MAIL_GOOGLE_CLIENT_ID`, `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET` and
`MICROSOFT_CLIENT_ID`. `scripts/install.sh` reads them from
`packaging/secrets.env` when that file exists. A copy built without them
works in every other way and says so when you try to add a Google account.
A copy built without the Microsoft one hides Microsoft in Add Account.

To build with a client of your own instead, make one in a Google Cloud
project:

1. Enable the Gmail, People, Calendar and Drive APIs.
2. Under Branding, set the app name to `Penguin Mail` and your own address as
   the support and developer contact. Leave the logo out for personal use:
   uploading one sends the app toward Google's verification.
3. Under Audience, choose **External** and publish the app. Don't submit it
   for verification for personal use; you click through Google's
   "unverified app" notice once per account instead.
4. Under Data Access, add the seven scopes the app asks for at sign-in, so
   Google's list matches the app's:
   - `https://mail.google.com/`
   - `https://www.googleapis.com/auth/gmail.settings.basic`
   - `https://www.googleapis.com/auth/contacts`
   - `https://www.googleapis.com/auth/calendar.events`
   - `https://www.googleapis.com/auth/calendar.calendarlist`
   - `https://www.googleapis.com/auth/calendar.calendars`
   - `https://www.googleapis.com/auth/drive.file`

   The [privacy policy](privacy-policy.md) says what each one is for.
5. Under Clients, create an OAuth client of type **Desktop app**.

Put its ID and secret in `packaging/secrets.env`:

```sh
PENGUIN_MAIL_GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com
PENGUIN_MAIL_GOOGLE_CLIENT_SECRET=GOCSPX-...
```

Google issues a desktop client secret to identify the app, and anyone who
downloads a desktop app can read it. Your refresh tokens are what grant access
to mail.

To build with a Microsoft client of your own, register a public client in
Microsoft Entra for "Accounts in any organizational directory and personal
Microsoft accounts", with the platform "Mobile and desktop applications" and
the redirect `http://localhost`. Put its id in `packaging/secrets.env`:

```sh
PENGUIN_MAIL_MICROSOFT_CLIENT_ID=...
```

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
kind it is (`packaging-rpm`, `packaging-arch`, `packaging-flatpak`,
`packaging-snap`, or none for the .deb and the tarball). The feature
decides where updates come from and whether skills run.

| | .deb | rpm | Arch | Flatpak | Snap |
|---|---|---|---|---|---|
| Updates | Install in the app, or the apt repository | the dnf repository | pacman, by hand | the Flatpak remote | Snap Store |
| GnuPG | system | system | system | runtime's `gpg`, on `~/.gnupg` | snap's `gpg`, on a keyring inside the snap |
| Assistant skills | yes | yes | yes | no | no |
| Claude Code, MCP servers run as a command | yes | yes | yes | no | no |
| Tray icon | yes | yes | yes | yes | yes |
| Start at login | autostart file | autostart file | autostart file | Background portal | snapd autostart |

- **Updates.** The .deb checks GitHub once a day and offers Install,
  which downloads the new .deb and installs it through apt; `apt upgrade`
  brings the same version from the apt repository. The tarball updates
  itself the same way, into its own folder. The rpm leaves updates to
  dnf, and the Arch package to pacman, though there is no Arch
  repository yet, so that means downloading and installing the new
  `.pkg.tar.zst` by hand; see `packaging/aur/PKGBUILD` for what an AUR
  package would add. The Flatpak gets updates from its remote, and the
  snap from the Snap Store. Preferences and the About window say which
  applies.
- **GnuPG.** The Flatpak reaches two places for signing and encryption:
  `~/.gnupg`, read and written, and the gpg-agent socket folder under
  `$XDG_RUNTIME_DIR/gnupg`, read-only, so your own agent and pinentry
  handle passphrases. It also talks to the tray and the notification
  daemon, and writes to Downloads; the manifest says why for each. The
  snap's `gpg` and `gpgsm` use a keyring of their own inside the snap, in
  `~/snap/penguin-mail/current/.gnupg`, so the OpenPGP keys and S/MIME
  certificates in `~/.gnupg` do not appear there. The app has no way yet
  to import them, so signing and decrypting with your existing keys and
  certificates does not work in the snap yet.
- **Secrets.** Outside a sandbox, Google's refresh tokens, an IMAP
  password, and the assistant's API keys and MCP tokens sit in the
  desktop's keyring, service `mailrs` or `penguin-mail-imap`. The
  Flatpak keeps them in its own encrypted file instead, through the
  Secret portal, so no other app on the desktop can read them; the snap
  still uses the desktop's keyring, the same as the .deb. The snap reaches
  it only once you run `snap connect penguin-mail:password-manager-service`.
  Until then it cannot save a sign-in, and a bar across the top of the
  window says so and gives the command. The app asks snapd again each
  time the window opens, and the bar goes once the plug is connected.
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
before the rename stay signed in. The Flatpak keeps refresh tokens, IMAP
passwords and AI keys in its own file through the Secret portal instead;
see "Which package" above. A Flatpak installed before this file moved
those secrets there cannot read what it left in the desktop's keyring, so
its accounts ask you to sign in again once, and any IMAP password or AI
key needs typing in again too.

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
