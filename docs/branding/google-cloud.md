# What to enter in Google Cloud

Values for each field of the Google Auth Platform pages, in the order
[docs/setup.md](../setup.md) visits them. Anything marked optional can stay
empty for personal use.

## Branding

| Field | Value |
|---|---|
| App name | `mailrs` |
| User support email | your Gmail address |
| App logo | Optional. Leave it empty for personal use (see below). If you want it: `mailrs-google-120.png`, or `mailrs-google-120-white.png` on a white background |
| Application home page | Optional |
| Application privacy policy link | Optional. Needed only for verification; host [privacy-policy.md](privacy-policy.md) somewhere public and link it |
| Application terms of service link | Optional |
| Authorized domains | Leave empty. A desktop app redirects to `127.0.0.1`, which needs no domain |
| Developer contact email | your Gmail address |

**About the logo.** Google shows a logo on the consent screen only after
the app passes verification, and uploading one pushes the app toward that
review. mailrs asks for a restricted Gmail scope, so full verification also
means a paid yearly security assessment. For an app that only you use,
leave the logo out. The consent screen then shows the app name, and you
click through Google's "unverified app" notice once per account.

## Audience

| Field | Value |
|---|---|
| User type | External |
| Publishing status | In production (click **Publish app**). Do not submit for verification |

## Data Access

| Field | Value |
|---|---|
| Scope | `https://www.googleapis.com/auth/gmail.modify` |
| Justification | Only asked during verification. If you need one: "mailrs is a desktop mail client. It reads messages to display them, changes labels to archive, star, and mark mail read, moves messages to trash, saves drafts, and sends mail the user writes. Mail is stored only on the user's computer." |

## Clients

| Field | Value |
|---|---|
| Application type | Desktop app |
| Name | `mailrs` |

Copy the client ID and secret into mailrs's welcome screen.

## Other icon sizes

`png/` holds the icon at 16 to 1024 pixels, for anything else that asks.
The source is `app/data/icons/scalable/apps/dev.mailrs.Mailrs.svg`.
