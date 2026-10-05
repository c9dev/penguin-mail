# Penguin Mail privacy policy

Last updated: 29 September 2026

Penguin Mail is a mail and calendar app for Linux, published by Pivotd
(https://pivotd.com), at https://penguin-mail.com. It works with Google
accounts, through Google's APIs, with Microsoft accounts, through Microsoft
Graph, and with other mail providers over IMAP and SMTP. It runs on your own computer. This policy explains what it accesses,
where that data goes, and how it is protected.

## What Penguin Mail accesses in a Google account

You grant access through Google's own sign-in page. Adding an account asks
for every permission Penguin Mail uses, in one visit:

- `https://mail.google.com/`, to read your messages, labels and drafts so
  it can show them; to change labels, so you can archive, star, and mark
  mail read or unread; to move messages to the Trash; to save drafts and
  send the messages you write; and to erase mail permanently when you
  choose Delete Forever in the Trash.
- `gmail.settings.basic`, to read and change your automatic reply, your
  signature, and your Gmail filters, when you change them in Penguin Mail.
- `contacts`, to show your contacts' names and photos and suggest
  recipients, once you turn contacts on in Preferences, and to write a
  contact to Google Contacts when you ask the assistant to add or change
  one.
- `calendar.events`, to read your calendars and show your events; to make,
  change and delete events when you do so in Penguin Mail; to mark your
  answer to a meeting invitation; to add the events in a calendar file you
  open; to link a file you attach to an event; and to let the assistant
  read and change events when you ask it.
- `calendar.calendarlist`, to read the list of your calendars, so Penguin
  Mail can show more than the primary one; to change a calendar's color or
  hide it, when you do so in Penguin Mail; and to subscribe to a calendar
  by its address or to a public holiday calendar, when you ask.
- `calendar.calendars`, to make, rename and delete a calendar you own,
  when you ask.
- `drive.file`, to upload a file you attach to a calendar event to your
  Google Drive, and to share that file with the event's guests when you
  leave sharing on. It reaches only the files Penguin Mail itself put in
  your Drive, never any other file there.

You may leave any of these unticked on Google's screen. Penguin Mail then
turns off the feature that needs it and says why where you would use that
feature, with a Grant Access button that asks Google again. An account
added before Penguin Mail asked for all of them at once gets a bar at the
top of its mail list that names what it lacks and offers the same button.

## What Penguin Mail accesses in a Microsoft account

This covers Outlook.com, Hotmail, Live and Microsoft 365 accounts. You
grant access through Microsoft's own sign-in page. Adding an account asks
for every permission Penguin Mail uses, in one visit:

- `openid` and `User.Read`, to learn your address and whether the account
  is a personal one or an organization's.
- `offline_access`, to keep you signed in without asking for your password
  again.
- `Mail.ReadWrite` and `Mail.Send`, to read your messages, folders and
  drafts so it can show them; to move, flag, categorize and delete mail
  when you do so in Penguin Mail; to save drafts; and to send the messages
  you write.
- `MailboxSettings.ReadWrite`, to read and change your inbox rules, your
  Focused Inbox choices and your automatic reply, when you change them in
  Penguin Mail.
- `Calendars.ReadWrite`, to read your calendars and show your events; to
  make, change and delete events and calendars when you do so in Penguin
  Mail; to answer a meeting invitation; and to let the assistant read and
  change events when you ask it.
- `Contacts.ReadWrite`, to show your contacts' names and suggest
  recipients, once you turn contacts on in Preferences, and to add or
  change a contact when you ask the assistant to.

You may leave any of these unticked on Microsoft's screen. Penguin Mail
then turns off the feature that needs it and says why where you would use
that feature, with a Grant Access button that asks Microsoft again.

Your mail and events are downloaded from Microsoft straight to your
computer and kept in the same local database as for a Google account.
For these accounts Penguin Mail talks only to `graph.microsoft.com` and
`login.microsoftonline.com`. Microsoft's refresh token stays in your
desktop's keyring, as described below.

## Where your data goes

- Your mail is downloaded from Google straight to your computer and kept in
  a local database in your user folder: the last 30 days of mail plus
  everything in your inbox.
- Penguin Mail has no server of its own. No data is sent to Pivotd or to any
  third party, and Penguin Mail contains no analytics, tracking or
  advertising.
- Penguin Mail connects to Google's Gmail, People, Calendar and Drive APIs,
  to Microsoft Graph and Microsoft's sign-in service for a Microsoft
  account, and to
  GitHub once a day to check for a new version of the app. The update check
  sends nothing about you or your mail.
- When you choose to load remote images in a message, your computer fetches
  those images from wherever the sender hosted them. When you click
  Unsubscribe, Penguin Mail contacts the address the mailing list gave for
  that purpose.
- When you add an account from a provider other than Google or Microsoft, Penguin Mail
  looks for its mail servers using the part of your address after the @,
  never the whole address. For a provider it knows, such as Fastmail or
  iCloud, it asks nobody. Otherwise it asks your DNS servers for the
  domain's mail records, and stops there when they point at a provider it
  knows. If they do not, it asks the domain's own web server and Mozilla's
  Thunderbird database (autoconfig.thunderbird.net) for its mail settings,
  asks the same of the company that receives the domain's mail, and tries
  to connect to the usual mail server names at the domain. It sends your
  password only to the servers you then sign in to.
- Penguin Mail has three AI features: the assistant, translation, and help
  with unsubscribe pages. Each is off until you choose a model for it, and
  each sends what it reads to the model chosen for it, which may run on
  your own computer or at Anthropic. The assistant sends the messages it
  reads to answer your request, and only when you ask it something.
  Translation sends the message you asked to translate, and only when you
  press Translate. Unsubscribe help sends a newsletter's unsubscribe page,
  its address, title, visible text and form fields, together with the
  address the newsletter was sent to, and only when you press Unsubscribe
  and Penguin Mail's own rules cannot read that page. It never sends the
  message itself. With a model running on
  your own computer, your mail stays on your computer. With the Anthropic
  API or Claude Code, it goes to Anthropic, under Anthropic's terms.
  Penguin Mail does not use your data to train or improve any AI model.
- The assistant can search the web and read web pages. This is on for
  Claude and can be turned off under Web Search on the AI page. Each search
  sends its words to the search engine in use: Anthropic, when the
  assistant runs on Claude; Brave, when you chose Brave Search for a local
  model; or the SearXNG server you named. The words of a search come from
  your request and can include what the assistant read in your mail. When
  it reads a page, the site that serves it sees a request from your
  computer, or from Anthropic's servers when the assistant runs on Claude.
- On the AI page you can add MCP servers: programs on your computer or
  services on the web that give the assistant more tools. Penguin Mail adds
  none by itself. A server you add receives what the assistant sends it in
  each tool call, which can include text from your mail, and the assistant
  asks you before each call unless you chose Always Allow for that tool.
  What the server does with it is up to whoever runs the server.
- The assistant can follow skills, which are instruction folders you add
  and turn on yourself. A skill's scripts run on your computer in a
  sandbox that has no access to your mail, your keys or your home folder,
  and no network unless you allow it for that skill. The assistant asks
  before each script runs.

## How your data is protected

- **In transit.** Every connection to Google or Microsoft uses HTTPS with TLS. Sign-in
  uses OAuth 2.0 with PKCE through your web browser, so Penguin Mail never
  sees your Google or Microsoft password.
- **Sign-in tokens.** Google's and Microsoft's refresh tokens, an IMAP account's password,
  any assistant API keys, and the tokens of MCP servers you add are stored
  in your desktop's keyring (GNOME Keyring or another Secret Service),
  which encrypts them with your login password. The Flatpak keeps them in
  its own encrypted store instead, through the Secret portal, so no other
  app on your desktop can read them. Short-lived access tokens are kept in
  memory only and never written to disk.
- **Files on disk.** The folders Penguin Mail keeps its data in are readable
  by your user account alone, and its configuration file is written the
  same way. Penguin Mail does not add its own encryption to the local mail
  database, so we recommend turning on full-disk encryption, which Ubuntu
  offers during installation.
- **Logs.** Penguin Mail writes problems to your system log so they can be
  diagnosed. It masks email addresses there, keeping only their first
  letter and domain, and it never logs message text or sign-in tokens.
- **Reading mail safely.** Messages are displayed with JavaScript turned off
  and in a sandbox. Remote content is blocked twice, by a content filter and
  by the page's own security policy, so senders cannot track when you open
  a message unless you allow images for it.
- **Encryption and signatures.** OpenPGP and S/MIME are handled by your own
  GnuPG installation. Penguin Mail never holds your private keys or asks
  for their passphrases.
- **Updates.** New versions are downloaded from GitHub over HTTPS and checked
  against a published SHA-256 checksum before they are installed.
- **Security reports.** Vulnerabilities can be reported privately, as
  described at https://github.com/c9dev/penguin-mail/security.

## Keeping and removing data

Your data stays on your computer until you remove it. Removing an account in
Penguin Mail deletes its downloaded mail and its sign-in token from your
computer. Uninstalling the app and deleting `~/.local/share/penguin-mail`,
`~/.config/penguin-mail` and `~/.cache/penguin-mail` removes everything else.
To revoke Penguin Mail's access on Google's side, visit
https://myaccount.google.com/permissions. On Microsoft's side, visit
https://account.live.com/consent/Manage for a personal account, or your
organization's My Apps page for a work or school account.

## Google API Services User Data Policy

Penguin Mail's use and transfer of information received from Google APIs
adheres to the Google API Services User Data Policy, including the Limited
Use requirements. Data from Google APIs is used only to provide the features
you see in the app, is never sold, is never used for advertising, and is
never used to develop, improve or train generalized AI or machine learning
models.

## Contact

Questions about this policy: support@penguin-mail.com.
