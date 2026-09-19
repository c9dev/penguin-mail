# Setting up mailrs

mailrs reads Gmail through your own Google Cloud project. You create the
project once, then add each Gmail account from the command line.

## 1. Create the Cloud project

1. Open <https://console.cloud.google.com/> and create a project called `mailrs`.
2. Enable the Gmail API: APIs & Services, Library, Gmail API, Enable.

## 2. Configure consent

In Google Auth Platform:

1. Branding: name the app `mailrs` and give your address as the support and developer contact.
2. Audience: choose External.
3. Data Access: add the scope `https://www.googleapis.com/auth/gmail.modify`.
4. Audience: click Publish app and confirm. Do not submit it for verification.

Step 4 is what keeps you signed in. Google expires refresh tokens after 7 days
for apps left in Testing, which would sign every account out once a week. A
published, unverified app keeps its tokens; the price is a warning screen
during consent.

## 3. Create the OAuth client

Clients, Create client, application type Desktop app, name `mailrs`. Copy the
client ID and client secret.

## 4. Write the config

Save this as `~/.config/mailrs/config.toml`:

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
to mail, and mailrs keeps those in the GNOME keyring, not in this file.

## 5. Add accounts

```sh
cargo run --release -p mailrs-cli -- account add
```

Your browser opens Google's consent screen. Pick the account. Google says
"Google hasn't verified this app": click Advanced, then Go to mailrs
(unsafe), then Continue. The terminal prints the address it added. Repeat for
each account.

## 6. Sync and look around

```sh
cargo run --release -p mailrs-cli -- sync                 # Ctrl-C to stop
cargo run --release -p mailrs-cli -- threads              # unified inbox
cargo run --release -p mailrs-cli -- threads --account you@gmail.com --label SENT
cargo run --release -p mailrs-cli -- show you@gmail.com <thread-id>
cargo run --release -p mailrs-cli -- triage you@gmail.com <thread-id> archive
```

## Milestone 0: confirm tokens last

Automated tests cannot check token lifetime. Write down the date you add your
first account. Eight or more days later, run `mailrs-cli sync`. Every account
should reach `ok`. An account that reports `needs_reauth` without you revoking
it means the project is probably still in Testing: check the publishing status
under Audience, publish, and add the account again.

## Where things live

| What | Where | Override |
|---|---|---|
| Config | `~/.config/mailrs/config.toml` | `MAILRS_CONFIG` |
| Mail cache | `~/.local/share/mailrs/mailrs.db` | `MAILRS_DATA_DIR` |
| Refresh tokens | GNOME keyring, service `mailrs`, one entry per address | |

`mailrs-cli account remove you@gmail.com` deletes an account's local mail and
its keyring entry. To revoke access on Google's side as well, use
<https://myaccount.google.com/permissions>.
