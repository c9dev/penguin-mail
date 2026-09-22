# Setting up Penguin Mail

Penguin Mail reads Gmail through your own Google Cloud project. You create the
project once, then add each Gmail account from the command line.

## 1. Create the Cloud project

1. Open <https://console.cloud.google.com/> and create a project called `penguin-mail`.
2. Enable three APIs. For each one, go to APIs & Services, Library, search
   for it, and click Enable:
   - **Gmail API**, for your mail. Nothing works without it.
   - **People API**, for Google contacts. Without it, turning contacts on
     for an account does nothing.
   - **Google Calendar API**, so answering an invitation also updates your
     calendar. Without it, answers still reach the organizer by email.

   Penguin Mail tells you which one to turn on if you skip one, with a
   button that opens the right page.

## 2. Configure consent

In Google Auth Platform:

1. Branding: name the app `Penguin Mail` and give your address as the support and developer contact. Leave the logo empty: Google shows it only after verification. [google-cloud.md](google-cloud.md) lists every field.
2. Audience: choose External.
3. Data Access: click **Add or remove scopes**, paste these two into
   **Manually add scopes**, click **Add to table**, then **Update** and **Save**:
   ```
   https://www.googleapis.com/auth/gmail.modify
   https://www.googleapis.com/auth/gmail.settings.basic
   ```
   The first reads, sends, and organizes mail. The second lets Penguin Mail change
   Gmail settings: the automatic reply, Rules, and Block Sender.
4. Audience: click Publish app and confirm. Do not submit it for verification.

**Set up before automatic replies existed?** Add the second scope under Data
Access as above. Then in Penguin Mail, open an account's ⋮ menu, choose
**Automatic Reply…** or **Rules…**, and click **Grant Access**. Google asks
you to confirm once per account; mail keeps syncing throughout.

Step 4 is what keeps you signed in. Google expires refresh tokens after 7 days
for apps left in Testing, which would sign every account out once a week. A
published, unverified app keeps its tokens; the price is a warning screen
during consent.

## 3. Create the OAuth client

The client is what identifies Penguin Mail to Google. You create it once; every
account you add later uses it.

1. Open <https://console.cloud.google.com/auth/clients>. Check that the
   project picker at the top of the page shows `penguin-mail`. If it shows
   another project, click it and choose `penguin-mail`.
2. Click **Create client**. (If you arrive at **Credentials** instead, click
   **Create credentials**, then **OAuth client ID**.)
3. Under **Application type**, choose **Desktop app**.
4. Under **Name**, type `Penguin Mail`. The name is only for you; Google does not
   show it to anyone.
5. Click **Create**.
6. A window titled **OAuth client created** shows the **Client ID** and the
   **Client secret**. Copy both now, or click **Download JSON** to save
   them. **Google never shows the secret again** once this window closes.
7. Click **OK**.

The client ID ends in `.apps.googleusercontent.com`. The secret usually
starts with `GOCSPX-`. Keep both at hand for the next step.

**Lost the secret?** Open the client from the Clients list, click **Add
secret**, copy the new one, and put it in Penguin Mail. You can then disable and
delete the old secret on the same page. Deleting the client and creating a
new one works too.

**No Create client button?** Google asks for the consent screen first.
Finish step 2, then come back.

A desktop client needs no redirect URI. Penguin Mail receives Google's answer on
`127.0.0.1` at a random port, which Google allows for every desktop client.

## 4. Give Penguin Mail the client

Open Penguin Mail. The welcome screen asks for the client ID and secret; paste them
and click Continue. Penguin Mail saves them to
`~/.config/penguin-mail/config.toml`,
readable only by you.

If you prefer the command line, write that file yourself:

```toml
[oauth]
client_id = "1234567890-abc.apps.googleusercontent.com"
client_secret = "GOCSPX-..."

# Optional. These are the defaults.
[sync]
poll_seconds = 30
window_days = 30
body_cache_mb = 1024
```

Google issues a desktop client secret to identify the app, and anyone who
downloads a desktop app can read it. Your refresh tokens are what grant access
to mail, and Penguin Mail keeps those in the GNOME keyring, not in this file.

## 5. Add accounts

Click **Sign In with Google** in the app, or **Add Account** at the bottom of
the sidebar for later accounts. From a terminal, `penguin-mail-cli account add`
does the same.

Your browser opens Google's consent screen. Pick the account. Google says
"Google hasn't verified this app": click Advanced, then Go to Penguin Mail
(unsafe), then Continue. The account appears in the sidebar and starts
downloading. Repeat for each account.

## 6. The command line

```sh
cargo run --release -p mailrs-cli -- sync                 # Ctrl-C to stop
cargo run --release -p mailrs-cli -- threads              # unified inbox
cargo run --release -p mailrs-cli -- threads --account you@gmail.com --label SENT
cargo run --release -p mailrs-cli -- show you@gmail.com <thread-id>
cargo run --release -p mailrs-cli -- triage you@gmail.com <thread-id> archive
```

## Milestone 0: confirm tokens last

Automated tests cannot check token lifetime. Write down the date you add your
first account. Eight or more days later, check the sidebar, or run
`penguin-mail-cli account list`: every account should show no warning icon and the
state `ok`. An account that reports `needs_reauth` without you revoking
it means the project is probably still in Testing: check the publishing status
under Audience, publish, and add the account again.

## Which package

Every package is the same app, built with a cargo feature that says what
kind it is (`packaging-rpm`, `packaging-flatpak`, `packaging-snap`, or
none for the .deb and the tarball). The feature decides where
updates come from and whether skills run.

| | .deb / rpm | Flatpak | Snap |
|---|---|---|---|
| Updates | apt / dnf repository | Flathub | Snap Store |
| GnuPG | system | runtime's `gpg`, on `~/.gnupg` | snap's `gpg`, on `~/.gnupg` |
| Assistant skills | yes | no | no |
| Claude Code, MCP servers run as a command | yes | no | no |
| Tray icon | yes | yes | yes |
| Start at login | autostart file | Background portal | snapd autostart |

- **Updates.** A .deb and a tarball update themselves from GitHub
  releases. The rpm leaves it to dnf. The Flatpak and the snap
  leave it to their store, and Preferences says which.
- **GnuPG.** The Flatpak has two holes in its sandbox for signing and
  encryption: `~/.gnupg`, and the gpg-agent socket under
  `$XDG_RUNTIME_DIR/gnupg`, so your own agent and pinentry handle
  passphrases. The snap reaches `~/.gnupg` through a `personal-files` plug.
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
to skip the welcome screen, then add each account again, since the list
of accounts lives in the store.

`penguin-mail-cli account remove you@gmail.com` deletes an account's local mail and
its keyring entry. To revoke access on Google's side as well, use
<https://myaccount.google.com/permissions>.
