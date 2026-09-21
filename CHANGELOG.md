# Changelog

Each release of Penguin Mail, newest first. GitHub shows the same text on
the [releases page](https://github.com/c9dev/penguin-mail/releases).

## Unreleased

## 0.1.3 (2026-09-21)

### New

- Preferences has a Contacts & Calendar page. Google contacts turn on and
  off for each account, and it shows which accounts GNOME Calendar can see,
  with a button to add the others.
- Translation can use a model of its own, such as a small local one while
  the assistant runs on Claude. Preferences calls the page AI now.
- The assistant shows what the model thought and each tool it ran, as
  rows under your question that open to show the details, with a line
  underneath saying what it is doing now: waiting, thinking, running a
  tool, waiting for your answer, or writing.
- The assistant can search the web and read pages. Claude uses Anthropic's
  own search; a local model can search with Brave Search or your own
  SearXNG server, chosen under Web Search on the AI page.
- The assistant can use your Google calendar: it lists what is on, finds
  free time, adds, moves and deletes events, and answers invitations,
  asking you before each change.
- The assistant can mute conversations, delete mail forever, send mail
  later, write from your templates, unsubscribe you from lists, read
  text, web page and PDF attachments, and look people up in your
  contacts.
- The assistant can use tools from MCP servers you add on the AI page, by
  a command or a URL. It asks before each call, and Always Allow stops the
  asking for a tool you trust.
- The assistant can follow skills: folders of instructions for one kind of
  task, from Penguin Mail's own skills folder or Claude Code's. Turn each
  one on under Skills on the AI page. A skill's scripts run in a sandbox
  with no access to your mail or home folder, after you allow each
  command, and reach the internet only if you allow it for that skill.

### Improved

- The thinking and tool rows under an answer use smaller, dimmer text and
  a smaller arrow, so they sit quietly beside the reply.
- Your mail, settings and cache folders can only be opened by your own
  user account, not by other people who use the same computer.

### Fixed

- Asking the assistant something scrolls the chat down to your question
  when the chat is already full. The chat glides down with the answer
  only while you are at the end, so reading an earlier answer keeps your
  place.
- Updates download even while GitHub's download links are failing, and a
  brief server error no longer stops an update.
- Rules open for an account that has none yet, instead of showing an
  error.
- Contacts load for every account, not only the first one.
- When the Google Cloud project has the People or Calendar API switched
  off, Penguin Mail says which one and opens the page that turns it on,
  instead of doing nothing. An invitation answer still reaches the
  organizer by email.
- The focus ring in a new message is a single thin line around the whole
  row, with room around the text.

## 0.1.2 (2026-09-21)

### New

- The About window has a Check for Updates button that shows its progress
  and installs the new version when one is out.
- The main menu has an entry for updates: check, install, or restart into
  the new version.

### Fixed

- The Install, Restart and Show Log buttons on the update banner work.

## 0.1.1 (2026-09-21)

The first public release.

### New

- Penguin Mail keeps itself up to date. Once a day it checks for a new
  version, and one click installs it.
- Install it from a `.deb`, or from a tarball or zip without admin rights.
- `penguin-mail --version` shows which version you have.

### Fixed

- The tray icon no longer disappears after the app has sat in the
  background for a while.
- After you install a new version, the app switches to it on its own
  instead of running the old one until you quit it.

### Already in Penguin Mail

- Every Gmail account in one inbox, kept in sync from the tray.
- Encrypted and signed mail with OpenPGP and S/MIME.
- Undo Send, Send Later, and an outbox that tries again when you are back
  online.
- Flags in seven colors, VIPs, smart mailboxes, reminders and follow-ups.
- Gmail rules, automatic replies and Hide My Email addresses.
- An optional assistant that runs on a model on your own computer, or on
  Claude.
- English and European Portuguese.
