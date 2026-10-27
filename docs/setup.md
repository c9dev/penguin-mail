# Setting up Penguin Mail

Penguin Mail works with Google accounts and with any mail provider that
offers IMAP and SMTP, such as Fastmail, iCloud or Yahoo. Add each account in
the app, or from the command line.

## Adding an account

The first window shows the providers you can add. For each account after the
first, open the main menu and choose **Add Account**. From a terminal,
`penguin-mail-cli account add` does the same for a Google account.

### Google

Choose **Google**, then pick how Penguin Mail signs in to Gmail. If you pick a
way that opens Google's sign-in page, sign in and allow the permissions Penguin
Mail asks for, then come back to the app. Google may show a warning page first;
choose **Advanced**, then continue to Penguin Mail.

If Google blocks the sign-in, or your organization allows only apps its
administrator approved, sign in with a Google client of your own instead; see
[Your own Google client](#your-own-google-client).

### Other providers

Choose your provider, or **Other**, and type your address. Penguin Mail finds
the server settings for you and says what the provider needs, such as an app
password for iCloud, Fastmail or Yahoo, with a link to the page where you make
one. If it can't find them, **Enter Server Settings** lets you type the
incoming (IMAP) and outgoing (SMTP) servers yourself. The password goes to
your desktop's keyring, never into a file.

The account appears in the sidebar and starts downloading.

### Account order and defaults

In **Preferences > Accounts**, drag an account's handle above or below another
account. **Move Up** and **Move Down** do the same from the keyboard. This order
applies to the sidebar and account menus throughout Penguin Mail.

Choose **Default Account** above the account list. Turn on **Use Default Account
for All Tasks** to use it for new messages, replies, forwards, calendar events,
assistant actions and startup. Otherwise, expand **Defaults by Task** to choose
each separately. Turning the switch off restores your individual choices.

The automatic choices keep the account on screen for new messages, the original
account for replies and forwards, the last calendar account used for events, and
All Inboxes at startup. Assistant actions follow the new-message choice unless
you choose another account. An account you explicitly choose in a composer or
assistant request takes precedence. Saved drafts and existing events keep their
accounts. If a chosen account is unavailable, the task falls back to its
automatic choice; calendar events also require a writable calendar.

### Sender addresses and suffixes

Under **Preferences > Accounts**, expand your IMAP or POP3 account and
open **Addresses You Send From**. Add the addresses you send from through
that account there. Each uses that account's existing
SMTP server and credentials. Choose a default address for new messages and
an optional sender name. Google accounts keep using the addresses verified
in Gmail.

Enable **Allow +suffix** or **Allow .suffix** for each address whose server
accepts them. In the composer's **From** row, choose an address and enter a
suffix including its separator, such as `+shop` or `.orders`. The full
address appears below it. A suffix inherits its base address's name and
signature and stays with the saved draft.

Replies select the matching address in the original message's To or Cc,
including permitted suffixes on the same domain. Reply All leaves those
addresses out of the recipients. If no address matches, the account's
default is used. Delivery headers such as Delivered-To are not used, so
mail received through a blind copy or forwarding may need a manual From
choice. A reply to your own sent message can keep its original From.

### Proton Mail Bridge

Proton Mail needs Bridge for IMAP and SMTP. Set up its certificate before
signing in to Penguin Mail:

1. Start Bridge. Under **Settings > Advanced settings > Export TLS
   certificates**, export the certificate to a folder only you can access.
   Bridge also exports a private key. Keep that key out of Penguin Mail's
   certificate file and remove the exported copy when you no longer need it.
2. Create Penguin Mail's config directory and copy **only the public
   certificate** to the path for your installation:
   - Linux `.deb`, `.rpm`, or source build:
     `~/.config/penguin-mail/proton-bridge-cert.pem`
   - Snap: `penguin-mail/proton-bridge-cert.pem` under the snap's config
     directory (`$XDG_CONFIG_HOME`, or `.config` under its home directory).
   - Flatpak:
     `~/.var/app/io.github.c9dev.penguin-mail/config/penguin-mail/proton-bridge-cert.pem`
   - macOS: `~/Library/Application Support/penguin-mail/proton-bridge-cert.pem`

   The file must contain one PEM `BEGIN CERTIFICATE` block and no private key.
   Create the parent directory first if it does not exist. You can copy the
   exported certificate even if Bridge gave it a different file extension.
3. In Bridge, copy the account's IMAP and SMTP settings and its Bridge
   password. Choose **Enter Server Settings** in Penguin Mail. Use
   `127.0.0.1` for both hosts and enter the ports and security types shown
   by Bridge. Use the Bridge password, not your Proton account password.

Penguin Mail accepts that exact certificate for connections to `127.0.0.1`.
If Bridge replaces its certificate, export and copy the new public certificate
before signing in again. Penguin Mail reads the file on each connection, so
you do not need to restart it. A different certificate on `127.0.0.1`, and
all certificates on other hosts, still go through the system's certificate
checks. [Proton's Bridge settings guide](https://proton.me/support/comprehensive-guide-to-bridge-settings)
describes the export option.

### Calendars, contacts and rules on other providers

Penguin Mail looks for an IMAP account's calendar (CalDAV), contacts
(CardDAV) and rules server (ManageSieve) when you add the account, with
the same password. Fastmail, iCloud, Yahoo, Zoho, GMX, WEB.DE, mail.com,
Yandex, mailbox.org and Posteo are known; for another server it asks the
domain's DNS and its well-known addresses. **Preferences > Accounts**
lists what it found inside each account under **Calendar, Contacts and Rules
Servers**, with **Find Again**, and **Edit** to type a CalDAV or CardDAV
address yourself. A server outside your address's domain waits there for
you to press **Use It** before your password goes to it. Where the server
runs no rules, Penguin Mail runs them on this computer while it is open.

### A calendar without mail

A calendar that belongs to none of your mail accounts, such as one on a
Nextcloud or Radicale server, is added on its own. Choose **Add Account**,
then **Add a calendar without mail** under the tiles, and type the
server's address, your user name and your password. The address must be
HTTPS. Where the server uses two-step sign-in, make an app password for
Penguin Mail there and type that one.

The server's calendars appear in the calendar under your user name and
the server's host, beside your other accounts' calendars. The account has
no mail, so it is listed nowhere in the mail window. The button beside its
heading in the calendar's sidebar signs it in again or removes it. An
invitation you answer there reaches its organizer when the server sends
replies itself, as Nextcloud does; on a server that does not, the answer is
saved on your calendar and Penguin Mail says nobody was told. Until the
server has said which it does, as it does when the calendar first reads,
Penguin Mail says nobody will be told. Propose New Time goes out as mail,
so it is not offered on these invitations.

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
working, and signing one in again keeps its client.

## The config file

Penguin Mail needs no config file. To change how often it checks for mail,
how many days of mail it keeps, or how much message text it caches, write
`~/.config/penguin-mail/config.toml` on Linux or
`~/Library/Application Support/penguin-mail/config.toml` on macOS:

```toml
# These are the defaults.
[sync]
poll_seconds = 30
window_days = 30
body_cache_mb = 1024
body_prefetch_count = 0
```

**Preferences → Sync → Storage → Download Recent Inbox Messages** can keep
text for the newest 100, 300, or 1,000 Inbox messages per account before you
open them. Off is the default. A custom `body_prefetch_count` in the config
file uses the same setting. The existing message cache limit still applies,
so this is a cache rather than a permanent offline archive. Downloads run
after sync and backfill, yield to foreground actions, and preserve unread
state. Attached files and remote images stay on the server. POP3 ignores
this setting because it already downloads whole messages.

Temporary failures retry with the sync engine's backoff. Attempt tracking
stays within the selected newest messages, so it does not grow with the
account's lifetime or repeatedly download bodies the cache cannot hold.
Based on [zwnk's proposal for issue #50](https://github.com/c9dev/penguin-mail/issues/50).

Opening a custom or nested IMAP folder, Sent, or Drafts lists its server mail
beyond this background window. Each page reads at most 25 messages' metadata
per IMAP account, without
message text or attachments, and leaves unread mail unread. Scroll down or
choose **More** below the list for another page. More also works when a page
contains only replies to one conversation and has no scrollbar. A failed
page leaves the rows already loaded and can be tried again with More.
Refresh starts a fresh server listing after mail moves or is deleted.

Pages walk the server's UIDs from newest to oldest. The loaded rows are
sorted by date and grouped with the same local threading as synced mail;
imported mail can therefore change the order as more pages arrive. Listing
keeps metadata in the mail cache for threading and offline use, not message
bodies. This metadata still follows normal sync-window pruning and is not
protected by opening the folder. Offline, only cached mail is available;
a failed server listing says so rather than claiming the folder is empty.
The body cache limit still applies to message text, not listing metadata.
Gmail labels, JMAP, Outlook and local POP3 folders keep their existing
listing behavior. Unified Sent and Drafts combine these accounts' cached
mail with the IMAP server pages.

A failed first folder load can be retried by reopening the folder or choosing
**Check for Mail**. Pending loads coalesce until the headers reach the store;
a later visit can retry a load that failed or was canceled. The first visit
after a restart also repairs headers an older client may have skipped.

Penguin Mail keeps your refresh tokens in Keychain on macOS or the desktop
keyring on Linux, not in this file. The Flatpak keeps them in its own store
instead, through the Secret portal; see "Which package" below.

## Your own Google client

Penguin Mail signs in to Google with the project's own client. When Google
blocks that client, or your organization allows only apps its administrator
approved, you can sign in with a client you make in your own Google Cloud
project. Any package works this way, the released ones included.

### Making the client

In the [Google Cloud console](https://console.cloud.google.com/), create a
project, then:

1. Under APIs & Services, enable the Gmail, People, Google Calendar and Google
   Drive APIs.
2. Under Google Auth Platform, Branding, set the app name (`Penguin Mail` is
   fine) and your own address as the support and developer contact. Leave the
   logo out.
3. Under Audience, choose **External** and leave the publishing status at
   **Testing**. Under Test users, add the Google address you will sign in
   with. Only test users can sign in to an app in testing.
4. Under Data Access, add the seven scopes Penguin Mail asks for at sign-in,
   so Google's list matches the app's:
   - `https://mail.google.com/`
   - `https://www.googleapis.com/auth/gmail.settings.basic`
   - `https://www.googleapis.com/auth/contacts`
   - `https://www.googleapis.com/auth/calendar.events`
   - `https://www.googleapis.com/auth/calendar.calendarlist`
   - `https://www.googleapis.com/auth/calendar.calendars`
   - `https://www.googleapis.com/auth/drive.file`

   The [privacy policy](privacy-policy.md) says what each one is for.
5. Under Clients, create an OAuth client of type **Desktop app**. Copy its
   client ID and client secret.

Google ends a sign-in made through an app in testing after seven days, and
the account then asks you to sign in again. To avoid that, choose
**Publish app** under Audience instead of adding test users. You click
through Google's "unverified app" notice once per account.

### Signing in with it

Set `PENGUIN_MAIL_GOOGLE_CLIENT_ID` and `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`
in Penguin Mail's environment, then choose **Add Account** and **Google**.
The sign-in page says "Signing in with your own Google client". If only one
of the two is set, Penguin Mail says which one is missing and opens no page.

From a terminal, with the .deb, rpm, Arch package, tarball or a copy you
built:

```sh
PENGUIN_MAIL_GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com \
PENGUIN_MAIL_GOOGLE_CLIENT_SECRET=GOCSPX-... \
penguin-mail
```

Quit Penguin Mail first: a second start hands over to the copy already
running, which does not see the new variables. `penguin-mail-cli account add`
reads the same two variables.

The Flatpak reads its environment from Flatpak's overrides:

```sh
flatpak override --user \
  --env=PENGUIN_MAIL_GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com \
  --env=PENGUIN_MAIL_GOOGLE_CLIENT_SECRET=GOCSPX-... \
  io.github.c9dev.penguin-mail
```

Quit Penguin Mail and start it again for it to see them. Once the account
is added, remove them again. This also removes any other
override you made for Penguin Mail:

```sh
flatpak override --user --reset io.github.c9dev.penguin-mail
```

For the snap, quit Penguin Mail and start it from a terminal with the
variables set:

```sh
PENGUIN_MAIL_GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com \
PENGUIN_MAIL_GOOGLE_CLIENT_SECRET=GOCSPX-... \
snap run penguin-mail
```

snapd passes variables like these through to a strictly confined snap
(checked with snapd 2.77). A start from the app grid does not have them.

On macOS, quit the app and start it from Terminal:

```sh
PENGUIN_MAIL_GOOGLE_CLIENT_ID=... PENGUIN_MAIL_GOOGLE_CLIENT_SECRET=... \
"/Applications/Penguin Mail.app/Contents/MacOS/penguin-mail"
```

A copy installed with `scripts/install-macos.sh` also needs
`XDG_DATA_DIRS="$(brew --prefix)/share:/usr/local/share:/usr/share"` before
the command when started from Terminal. Finder sets this through the
app's `Info.plist`.

### What Penguin Mail keeps

An account added this way keeps its client in the keyring beside its
refresh token, under the service `penguin-mail-google-client` (in the
Flatpak's own store through the Secret portal), and refreshes through that
client at every start, with or without the variables. You need them again
only to add another account. Signing an account in again keeps its client.
Removing the account deletes its client from the keyring.

Your shell's history, a script that sets the variables, and Flatpak's
override file (`~/.local/share/flatpak/overrides/io.github.c9dev.penguin-mail`)
keep the client secret in plain text. Google issues a desktop client secret
to identify the app rather than to protect it, so this exposes no mail, but
remove the override once the account is added.

## Building your own copy

A build signs in to Google or Microsoft only when it was compiled with the
project's client, from these variables:

- `PENGUIN_MAIL_GOOGLE_CLIENT_ID`
- `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`
- `PENGUIN_MAIL_MICROSOFT_CLIENT_ID`

The release workflow takes them from the secrets
`PENGUIN_MAIL_GOOGLE_CLIENT_ID`, `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET` and
`MICROSOFT_CLIENT_ID`. `scripts/install.sh` and `scripts/install-macos.sh`
read them from `packaging/secrets.env` when that file exists. A copy built
without them works in every other way and says so when you try to add a
Google account.
A copy built without the Microsoft one hides Microsoft in Add Account.

To build with a client of your own instead, make one as
[Your own Google client](#your-own-google-client) says, and put its ID and
secret in `packaging/secrets.env`:

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

Every package is the same app. The Linux packages use a cargo feature that
says what kind they are (`packaging-rpm`, `packaging-arch`,
`packaging-flatpak`, `packaging-snap`, or none for the .deb and tarball).
The macOS app uses the native build. Its WebKit view and app menu use macOS
instead of Linux's WebKitGTK and tray.

| | .deb | rpm | Arch | Flatpak | Snap | macOS app |
|---|---|---|---|---|---|---|
| Updates | Install in the app, or the apt repository | the dnf repository | pacman, by hand | the Flatpak remote | Snap Store | replace the app from a release zip |
| GnuPG | system | system | system | runtime's `gpg`, on `~/.gnupg` | snap's `gpg`, on a keyring inside the snap | install separately |
| Assistant skills | yes | yes | yes | no | no | no |
| Claude Code, MCP servers run as a command | yes | yes | yes | no | no | if available on the app's PATH |
| Tray icon | yes | yes | yes | yes | yes | no |
| Start at login | autostart file | autostart file | autostart file | Background portal | snapd autostart | no |

- **Updates.** The .deb checks GitHub once a day and offers Install,
  which downloads the new .deb and installs it through apt; `apt upgrade`
  brings the same version from the apt repository. The tarball updates
  itself the same way, into its own folder. The rpm leaves updates to
  dnf, and the Arch package to pacman, though there is no Arch
  repository yet, so that means downloading and installing the new
  `.pkg.tar.zst` by hand; see `packaging/aur/PKGBUILD` for what an AUR
  package would add. The Flatpak gets updates from its remote, and the
  snap from the Snap Store. On macOS, download the next zip from the
  [releases page](https://github.com/c9dev/penguin-mail/releases/latest)
  and replace `Penguin Mail.app`. Preferences and the About window say
  which update method applies to the Linux packages.
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
  desktop's keyring, service `mailrs` or `penguin-mail-imap`. On macOS,
  that is Keychain. The
  Flatpak keeps them in its own encrypted file instead, through the
  Secret portal, so no other app on the desktop can read them; the snap
  still uses the desktop's keyring, the same as the .deb. The snap reaches
  it only once you run `snap connect penguin-mail:password-manager-service`.
  Until then it cannot save a sign-in, and a bar across the top of the
  window says so and gives the command. The app asks snapd again each
  time the window opens, and the bar goes once the plug is connected.
- **Skills.** A skill's scripts run under bubblewrap, which cannot start
  inside Flatpak's or a strict snap's sandbox and cannot start on macOS.
  Running scripts without it would hand a skill your mail and keys, so
  Flatpak and snap turn skills off. On macOS, the sandbox check refuses to
  run a script.
- **Programs on your system.** Claude Code, and an MCP server you add as a
  command, run as programs on your computer. The Flatpak and the snap
  cannot see those programs. A local model, the Anthropic API and MCP
  servers you reach by address work in every package. On macOS, install
  command-line programs separately and make them available on the app's PATH.
- **AppImage.** The x86_64 AppImage carries the current app and its runtime
  for Ubuntu 22.04 and newer, and Debian 13. It keeps WebKit and image-loader
  sandboxes enabled and uses your desktop's services and GnuPG. On Ubuntu
  24.04 it shows a command to run once, which lets it start those sandboxes.
  Download a new AppImage to update. See [AppImage](appimage.md) for
  installation, extraction mode, build instructions and testing limits.

## Where things live

| What on Linux | Where | Override |
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

On macOS, the config file and mail database live under
`~/Library/Application Support/penguin-mail/`. The cache lives under
`~/Library/Caches/penguin-mail/`, and Keychain holds sign-in tokens.

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


### Space on the server

Expand an account in Preferences to see **Space on the Server**. Reported usage
shows a bar and the used and total amounts. The row updates if the report arrives
while Preferences is open. When the server provides no report, the row says so.
Gmail reports its shared Google storage. IMAP and JMAP accounts report quotas when
their servers offer them. Fastmail accounts signed in through the browser also
read their IMAP quota when JMAP does not offer one, using the same OAuth session.
Outlook connected through Microsoft Graph does not
currently provide a mailbox quota report in Penguin Mail.

## Attachments

Use the save button beside an attachment to choose its filename and destination.
The desktop file chooser grants Flatpak access to the selected file. Opening
an attachment uses the system's default application for its type. A picture's
**Open in Another Program** button opens the desktop's application chooser.
