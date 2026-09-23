# Changelog

Each release of Penguin Mail, newest first. GitHub shows the same text on
the [releases page](https://github.com/c9dev/penguin-mail/releases).

## Unreleased

### Improved

- A conversation you read before opens with one page load instead of two,
  so it is ready in about half the time.
- Before installing an update, Penguin Mail checks that the release is
  signed with its own key, and refuses one that is unsigned or signed by
  anyone else.
- The Social and Forums category lists in 1 ms instead of 10 on an inbox of
  15,000 conversations, and unread counts for the inbox, its categories and
  VIPs take under a millisecond where they took 6 to 29.
- The sidebar works out every mailbox's count in 6 ms instead of 55.
- A conversation with many pictures opens faster and never stalls the
  window: pictures arrive several at a time, and the small pictures on
  attachment rows are made away from the window, where ten large photos
  used to hold it up for about a second and a half.
- A list with thousands of conversations selected keeps up: new mail,
  Escape and moving on after an action no longer look at every row
  against every selected one, which with 14,800 selected took 167 ms.
- A change to an account or a label counts unread mail once rather than
  twice, and a burst of changes rereads only the open conversations it
  touched.
- Penguin Mail keeps at most 48 MB of pictures from mail it has shown,
  where it could hold 430 MB before.
- The assistant on the Anthropic API costs less per question. Each request
  used to send its tools and instructions, about 12,000 tokens, at full
  price; now the API reads them and the chat so far from its cache.
- Delete Forever on a Trash full of conversations goes out as one request
  per thousand messages. Erasing 200 conversations used to make 400
  requests and use up Gmail's allowance for about a minute.
- Catching up after the computer was off for a week or more fetches only
  the mail that changed meanwhile, instead of every message again, so the
  account is ready sooner and leaves Gmail's allowance to you.
- Typing in a long quoted reply keeps up: each key used to restyle the
  whole message, several milliseconds on a reply of 5,000 lines, and now
  touches only the line you are on. The spell check that runs when you
  pause looks at the lines you changed rather than all of them, and
  saving or sending that reply reads it in about 13 ms instead of 280.
- A message with big attachments leaves the composer sooner: it is built
  once rather than twice, and Undo Send no longer copies every file it
  carries.
- Opening a signed conversation again shows its card at once, without
  fetching the message from Gmail or starting gpg, until your keyring
  changes.

### Fixed

- Installed from the tarball into a folder with a space in its path,
  Penguin Mail starts from the app grid and at login, where the launcher
  used to run nothing.
- A Flag mailbox's count includes a starred conversation with one of its
  messages in the Trash, as the mailbox's list already did.
- In Send Later, the Outbox and Remind Me, the Delete button and its menu
  item say what they do there, Cancel Send, Delete from Outbox and Cancel
  Reminder, where they said Move to Trash.
- Dragging mail from Sent or Flagged onto a label keeps it open, since it
  stays in that list. Moving mail out of the label on screen, by dragging
  it to another label or to All Mail, opens the next conversation.
- Starting a search drops the rows selected before it, so the buttons act
  on the results, and the Outbox's own buttons switch off.
- A new account colour reaches every row in the list. The list could
  redraw before the colour had been read.
- Opening an attachment no longer leaves a copy behind for good. The copy
  only you can read goes when its preview closes, or the next time Penguin
  Mail starts, and a copy of a file from encrypted mail goes when the
  window closes.
- Opening a large photo no longer holds up the window while it loads.
- Reply on a new-mail notification answers that mail, and no longer the
  conversation you opened while it was still loading.
- The toast after moving mail to a label with an ampersand in its name,
  such as R&D, names the label again instead of showing nothing.
- Your preferences survive a damaged settings file. Penguin Mail used to
  start from the defaults and save over the file; it now keeps the old
  file beside the new one and tells you its name. A save also replaces
  the file in one step, so a crash in the middle cannot empty it.
- MCP servers the assistant started stop when Penguin Mail restarts
  itself to free memory or to install an update, where they used to keep
  running with nothing talking to them.
- The assistant reports sorting a sender into a category once their mail
  has moved, where it used to say so before anything happened, and it
  says when the rule for their future mail still needs a permission.
- A chat with the assistant keeps working after you press Stop, and after
  it reads one very long conversation. Either could leave every later
  question failing until you started a new chat.
- Undo puts back only what the action changed. Undoing Trash on an archived
  conversation leaves it archived instead of moving it to the inbox, and
  undoing Mark as Read leaves the messages you had already read as read.
- A message in the Outbox goes out once, even when you press Send Now while
  the outbox is already sending it, or the connection drops just after
  Gmail took it.
- Labels made, renamed, recoloured or deleted in Gmail on the web reach the
  sidebar within the hour, and at once when mail arrives carrying a new
  one. Before, the sidebar only learned of them when the account was
  added.
- Your contacts stay in the address book when Google stops part way
  through sending them again, instead of leaving it empty or half full.
- Moving a few conversations out of the Trash puts them back in the inbox
  at Gmail too, as moving ten or more already did.
- Quoting, forwarding or reopening HTML mail with a ">" inside a tag's
  attribute, such as a link's title, no longer spills the rest of that
  tag into the text, and paragraphs quote with a blank line between them.
- Choosing another From address swaps the signature and nothing else: the
  cursor stays where you were typing, and Undo still reaches what you
  wrote before.
- Clear Formatting takes the bullets and numbers off a list along with its
  styles, where it used to leave "•" and "1." behind as typed text.
- A signed message shows only what its signature covers. A part somebody
  added beside the signed one, or text written around a signed block, no
  longer appears under "Signed by".
- An S/MIME message encrypts only to certificates you trust. Opening a
  stranger's signed message used to store their certificate, and the next
  message to the address it named could go to them.
- "Signed by" turns green only when you trust the key and it belongs to
  the address the message is from. A key nobody vouched for, or one that
  names somebody else, now says so on the card.
- Replying to or forwarding a message that arrived encrypted starts with
  Encrypt on, and asks before the quoted words go out readable.
- Opening a signed OpenPGP message never fetches the signer's key from the
  network, even when gpg.conf asks for that, so the sender cannot learn
  when you read it.
- Every signed or encrypted message in a conversation opens, not only the
  newest, so an older encrypted reply no longer stays unreadable.
- What gpg and gpgsm say when they fail now shows in the language the
  window is in, on the card and in the composer.

## 0.1.7 (2026-09-23)

### New

- Penguin Mail comes as an rpm for Fedora, with a dnf repository of its
  own, so `sudo dnf upgrade` brings each new version.

### Improved

- A big inbox lists faster. Opening the inbox or one of its categories
  reads about a page of mail instead of sorting all of it first, which on
  an inbox of 15,000 conversations took the wait from 25 to 45 ms down to
  1 to 13 ms.
- Switching between All, Primary, Updates, Promotions and Social answers
  at once: the switcher takes its new shape on the next frame and only
  the category's name fades in, where it used to slide for a fifth of a
  second while every icon drifted sideways.
- Right-clicking a conversation in the list offers everything the
  conversation's More Actions menu does: reply, archive, trash, flag,
  labels, mute, remind, print and the sender's options.

### Fixed

- LinkedIn's mail, and any other mail that labels its text the same way,
  shows its message again instead of an empty box and two attachments
  called text-text-body.txt and text-html-body.html.
- Naming a label Important, Starred, Inbox or another name Gmail keeps for
  itself now says why it cannot be used, instead of failing with Gmail's
  "Invalid label name".
- When Gmail refuses something, the message shows Gmail's own words, not
  the whole reply it sent.
- Unsubscribing no longer sits on "Reading the page…" for ever when
  the page needs the AI model and the model is Claude Code.
- Right-clicking a conversation that was not selected yet opens its
  menu. Before, the menu closed as the conversation opened.
- The icons in the category switcher above the inbox sit in the middle
  of their highlight instead of leaving a gap on the right.
- Shift+F10 and the Menu key open a message's own menu. Before, they
  opened an empty Cut and Paste menu in the window's top corner.

## 0.1.6 (2026-09-22)

### Fixed

- A message the assistant writes puts your signature under the words,
  not above them, with a blank line before it.
- A message that sets no colours of its own is drawn in the window's
  colours instead of on a white sheet, which in a dark window meant a
  bright slab. Mail that picks its own colours, such as a newsletter,
  keeps its white page.

### Improved

- The composer's header has room for its title again: Sign and Encrypt
  sit behind one button that says what the message goes out as.
- A message in a conversation slides open and shut instead of appearing
  all at once, and mailbox rows, conversation rows and attachments fade
  under the pointer. Turning off animations in the desktop settings turns
  all of it off.

### New

- Unsubscribe fills in and sends a newsletter's own unsubscribe page,
  after you confirm what it will press. A page it cannot read still opens
  in your browser, and a newsletter that hides its way out in a link at
  the foot of the message now has an Unsubscribe button too.
- Right-click a message inside an open conversation for a menu that acts
  on that message alone: reply to it, archive it, trash it, mark it,
  flag it in a colour, label it, export it, or copy its sender's address.
  Archiving one message of five leaves the other four in the inbox, and
  Ctrl+Z takes it back. The Menu key and Shift+F10 open the same menu for
  the message you are on.
- Ask the assistant to unsubscribe you from several newsletters at once.
  It lists the ones you get first, and asks about all of them together.
- The assistant can send a message waiting in Send Later or the Outbox
  now, cancel it, give it a new time, or delete it from the Outbox.
- The assistant lists your reminders and can move or cancel one.
- The assistant can unmute mail, read your muted mail and smart
  mailboxes, and undo your last change, as Ctrl+Z does.
- The assistant can rename, recolour and delete labels, change and delete
  smart mailboxes, and save and delete templates. Before deleting a label
  it tells you how many conversations carry it.
- The assistant can add people to your Google contacts and change the ones
  there, after asking.
- The assistant can let a sender's remote images load, or stop them, and
  tell you whose images load now.
- The assistant can export conversations as an mbox file, or one message
  as an .eml file, to your Downloads folder or a place you name. It never
  replaces a file without saying so first.
- The assistant can forward a message, add a Bcc, attach files from your
  mail or from this computer, and sign or encrypt what it sends. It asks
  before it reads a file from this computer, and names the path.
- The assistant lists your drafts, and changes or deletes one when you
  ask, encrypted drafts included.

### Fixed

- Cancel Send keeps a message you scheduled while offline. It goes to
  Drafts, or, while Gmail is still out of reach, opens in a composer for
  you to save.

## 0.1.5 (2026-09-22)

### New

- Penguin Mail has an apt repository. Add it once, or install the .deb,
  and `sudo apt upgrade` brings each new version.
- An Archive mailbox lists the mail you took out of the inbox, across all
  accounts and under each one. Drag mail onto it to archive it.
- You can label mail from several accounts at once by picking a label name.
- An invitation to one meeting of a repeating series now says how the
  series runs, such as "Every Wednesday, 6 left", read from your Google
  Calendar.
- An encrypted message can carry a Bcc when it goes out with OpenPGP. The
  people in To and Cc cannot tell who got the blind copy. S/MIME cannot
  hide one, so the Encrypt button says why it stays off there.
- A message waiting in the Outbox or in Send Later opens in the reading
  pane, with why it has not gone or when it goes, and buttons to edit,
  send or delete it.

### Improved

- With several accounts open in the sidebar, each account's heading has
  space above it, so you can tell where one account ends.
- Menu or Shift+F10 opens the menu of the conversation in focus, so Export
  and the Outbox's Edit, Send Now and Delete work from the keyboard.
- The Outbox stays in the sidebar, so you can check it before a message
  gets stuck there.
- A new account finishes its first sync sooner: Penguin Mail fetches each
  conversation in one request instead of one request per message.
- A conversation with several results in a search, the Trash or Junk opens
  without waiting on Gmail again.
- Labelling mail by name asks before it creates the label in an account
  that lacks it. Say no, and only mail in accounts with the label gets it.
- A short message in another language, such as "Ok, obrigado!", now gets
  the offer to translate it, and so does a short reply from someone who
  wrote to you in that language earlier in the conversation.
- The composer's formatting bar is one Tab stop: the arrow keys move
  between its buttons, and a screen reader hears it as a toolbar.
- Ctrl+Shift+P inserts an image in the composer, and the Keyboard
  Shortcuts window lists it.
- You can reach any recipient in an address field from the keyboard: Left
  from the start of the field steps onto the addresses, and Delete removes
  the one you are on.

### Fixed

- Dates name the weekday and month in the language the app speaks, so a
  Portuguese window says "sex 11 set" instead of "Fri 11 Sep".
- The offer to translate a message no longer names the wrong language
  when it could be Portuguese or Spanish, or German or Dutch. When the
  words leave it close, it says the message is in another language.
- A conversation opened right after a sync shows its older replies too,
  not only the recent messages kept on this computer.
- Declining a rule the assistant proposed no longer leaves its new label
  behind in Gmail.
- Mail archived in Gmail no longer stays in Penguin Mail's inbox when the
  app missed the change. It now checks its inbox against Gmail's when it
  starts and every hour after.
- An action Gmail refuses part way, such as archiving a long conversation
  when the connection drops, leaves the messages Gmail did change as Gmail
  has them, and no longer undoes changes made in the browser meanwhile.
- In demo mode, a message you send now shows up in Sent and in its
  conversation.
- The inbox category switcher fits a narrow list without scrolling
  sideways: when the chosen category's name has no room, it shows icons
  alone.
- The assistant no longer loses its right edge in a window about 1000
  pixels wide, or in a narrow window with the app in Portuguese. In a
  window narrower than 1100 pixels it opens over the mail.
- An encrypted message you send without signing it stays readable in your
  Sent folder.
- A draft of an encrypted message no longer waits in Gmail as readable
  text. Penguin Mail saves it encrypted to your own key, and Edit opens it
  with its Bcc, its files and Encrypt on again.
- An encrypted message you undo the send of, or that comes back from the
  Outbox, reopens with Encrypt on. If a recipient's key has gone missing,
  Send asks before the message goes out readable.
- Edit on a draft brings back its Bcc, its files and the message it
  replies to, and so does a draft the assistant schedules with Send Later.
- Cancel Send no longer tells you to look in Drafts for a message you
  scheduled while offline. Gmail never had that one, and the toast now
  says it is gone.
- A message waiting in Send Later shows the day it goes, such as
  "Tomorrow at 08:00", instead of "Today" for any day to come.
- A waiting message opened in a window of its own has working Edit, Send
  Now, Delete and Cancel Send buttons, and they act on that message
  rather than on the row selected in the main window.
- A template's `{{date}}` comes out in the language the app speaks, so a
  Portuguese window writes "22 de setembro de 2026".
- A Send Later message due tomorrow or later in the week says so in the
  list, not only the hour it goes out.

## 0.1.4 (2026-09-22)

### Improved

- Penguin Mail masks email addresses in its log, so the system journal no
  longer holds the addresses of your accounts or the people you write to.
- Cancel Reminder and dragging the open conversation onto a mailbox open
  the next conversation, as Archive does.

### Fixed

- A conversation whose first messages you deleted shows in the inbox again
  when a reply arrives, as it does in Gmail.
- A conversation you clicked just before switching mailboxes or inbox
  categories no longer opens in the new one and gets marked read there.
- Marking a conversation read or unread shows in its own window, whether
  you did it there or in the main window.
- Delete Forever no longer closes a conversation you opened while Gmail was
  still deleting.
- The Keyboard Shortcuts window lists Ctrl+J, which shows or hides the
  assistant.
- A conversation in its own window decides whether Archive, Delete or Mute
  close it by the mailbox you opened it from, not the one the main window
  has moved on to.
- Ctrl+Z after dismissing a follow-up brings it back, rather than undoing
  what you did before, and it also takes back a dismissal the assistant made.
- "Always load images from" a sender works again, and so does removing a
  sender from that list in Preferences.
- New Message from the dock or the app menu starts from your default
  account and adds your signature.
- Conversations open in their own windows follow changes to the text size,
  VIPs and contact photos.
- An encrypted message shows its invitation and offers translation once
  it is decrypted.
- Allowing images, an invitation card and the unsubscribed note stay on the
  conversation they belong to when you open another one while they load.
  Clicking two conversations quickly no longer shows the first one.
- Opening another conversation while one is still loading no longer adds
  the first one's newest messages to it.
- Trash in a conversation open in its own window moves it to the Trash or
  deletes it forever depending on where that conversation was opened from,
  not on the mailbox the main window shows now.

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
