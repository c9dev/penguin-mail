# Changelog

Each release of Penguin Mail, newest first. GitHub shows the same text on
the [releases page](https://github.com/c9dev/penguin-mail/releases).

## Unreleased

### New

- Address suggestions show which account each contact comes from, and
  after you send a message you can save its new recipients to that
  account's contacts with one press.
- Import your OpenPGP keys and S/MIME certificates in Preferences, under
  Writing: choose Import beside either row and pick an .asc, .p12 or .pem
  file.
- Add a POP3 account for a provider that offers only POP3, or to keep an
  account's mail on this computer alone: choose Other, then Enter Server
  Settings, and pick POP3. Mail stays on the server unless you choose to
  remove it after downloading or after some days. A message the server
  will not hand over after three tries appears in the account's menu with
  the server's own words, and About names the file that holds mail kept
  only on this computer.
- Add Outlook.com, Hotmail and Microsoft 365 accounts: choose Microsoft in
  Add Account, or on the first page, and sign in on Microsoft's own page.
- Outlook accounts get their calendars, with repeating events, invitations
  and the calendar list, and their contacts, rules and automatic reply.
- Outlook accounts get a Tags button and Focused and Other tabs over the
  inbox.
- Fastmail, iCloud, Yahoo and other accounts added with a password get their
  calendars and contacts: Penguin Mail finds their CalDAV and CardDAV
  servers, and Preferences shows what it found and lets you type an address
  it missed.
- Rules for accounts added with a password: on the mail server where it runs
  them, with the automatic reply beside them, or on this computer while
  Penguin Mail is open where it does not. A rule change made while the server
  is not answering goes out once it does.
- Markdown you paste into a rich text message arrives formatted, with Paste
  as Plain Text on the notice that follows if you wanted the marks. Markdown
  you type shows a bar with a Format button that turns it into rich text.
- Edit a rule: click it in Rules, or press Enter on it, to change what it
  matches or does. Parts the form can't show, such as a forward set up in
  Gmail, stay as they were.
- Each label in the sidebar has a ⋯ button on hover with Rename, New Label
  Inside, Color, Move Up, Move Down and Delete, the same menu a right click
  opens.
- Drag a label in the sidebar to put it in the order you like, or drop it
  on another label to nest it there. Folder accounts work the same way.
- Events show their attached files, such as Google Drive documents, in the
  event popover and the editor. Click one to open it in the browser.
- Attach a file from this computer to an event with Attach File… in the
  editor. It uploads to your Google Drive with a progress bar you can
  cancel, large files included, and the event's guests can open it unless
  you untick sharing. A file attached while offline uploads when you are
  back online. Removing a file takes it off the event and leaves it in
  Drive.
- A file that cannot upload, for want of Drive access or because it moved,
  no longer holds up your other calendar changes. It shows "Waiting for
  access" or "File not found" in the editor, with Grant Access or Remove.
- A calendar file that is not an invitation, such as a train ticket or a
  booking, now shows an event card in the message with Add to Calendar.
  Pick the calendar, and for a file with several events, which of them to
  add. Adding the same file twice does not repeat the events, and the card
  then says "Added to" the calendar and offers Show in Calendar.
- Penguin Mail opens .ics files from Files. The file shows the same card in
  a small window, and says so when no Google account can use its calendar.
- A reply or a forward opens with the quoted message folded behind a
  "•••" button under your words, as in Gmail, and still sends with it.
  Click "•••" to read it, and in a reply to trim it, or × to send
  without it.
- The calendar has an Agenda view beside Day, Week and Month, on the A key.
  It lists the coming days in a centered column, with today at the top,
  earlier days above and more days loading as you scroll down. A narrow
  window still shows this list in place of Week and Month.
- Fold an account's calendars away by clicking its address in the
  calendar sidebar. The list stays folded the next time you open the app.
- Right-click a calendar, or use its "⋮" button, to hide it from the list
  and the grid, or to give it a color of your own. Hidden Calendars at the
  foot of the list brings one back.
- Make a calendar of your own with Add Calendar at the foot of the
  calendar list, and rename or delete one you own from its "⋮" menu. A
  color or a hidden calendar now changes in Google Calendar too, so your
  phone shows the same. Each change shows at once and reaches Google when
  you are next online.
- Subscribe to a calendar someone publishes by pasting its https or webcal
  address, or add public holidays from Holiday Calendars, Portugal first.
  Both stay read-only here and show on every device, and Unsubscribe in
  the calendar's "⋮" menu takes one off again.
- The Mail and Calendar switch shows how much unread mail waits while
  you are in the calendar, and how many invitations wait for your answer
  while you are in mail.
- The mini month marks the day the calendar shows and, in Week or Month,
  the days in view. Click a day there to go to it.
- Out of office shows as a striped block, focus time with a target and
  a birthday with a cake, and where you work each day (Home, Office or the
  building) sits under the day's heading instead of in the all-day row.
- On a work or school account, make an out-of-office or focus-time entry
  from the event editor's new Type row, and choose whether it declines new
  or existing meetings and what the message to the organizer says.

- The earlier messages quoted under a reply or forward fold away behind
  a small "•••" button, as in Gmail; click it to read them in place.
- An invitation's card shows the hours around the meeting from your
  calendar and says whether anything else is on then.
- Answer one occurrence of a repeating invitation: after Yes, Maybe or
  No in the calendar, choose This event only or All events. An answer
  given offline goes out once you are back online, even after a restart.
- Add a note to your answer, in the event popover or on the invitation
  in your mail. The organizer reads it next to your Yes, Maybe or No.
- Propose a New Time from the calendar's event popover, the same way as
  from the invitation in your mail.
- Remove an invitation from your calendar with Remove in its popover or
  the Delete key. The organizer sees that you declined, and nobody else
  gets mail about it. Edit in the same popover changes your own
  reminders, color and busy or free.
- Move an event to another of your calendars: pick it in the editor's
  Calendar row and save. A new event from a quick drag can go on any of
  your calendars too.
- Penguin Mail comes as a package for Arch Linux: download the
  `.pkg.tar.zst` from the releases page and install it with
  `sudo pacman -U`.
- A calendar beside the mail: switch to it at the top of the sidebar or
  with Alt+2 to see your Google calendars by day, week or month, or as
  a scrolling list on a narrow window. Answer an invitation from its
  event and search your events.
- Add a mail account from Fastmail, iCloud, Yahoo or any other provider
  with IMAP: choose Add Account, then Another Provider, and type your
  address and password. Penguin Mail finds the servers, says when the
  provider wants an app password, and offers gmail.com when you type
  gmial.com. Archiving on a server with no Archive folder makes one,
  and a message says so; opening a folder lists its mail right away.
  Folders that only hold other folders show in the sidebar with their
  folders under them; you cannot open them or drop mail on them.
- Calendar events remind you with a notification before they start, with
  Join for video calls and Snooze for five minutes. Preferences can turn
  them off.
- Make, move and delete calendar events: press New Event or N to add
  one, click an empty slot or day, drag across a day to sketch one out,
  drag a block to move or stretch it, or open an event and press the
  pencil or the trash icon. A short event keeps a readable card however
  briefly it runs.
  Deleting or dragging offers Undo, and changing or deleting one date of
  a repeating event asks which occurrences it covers first. Moving a
  repeating event to another weekday moves its other dates with it, and
  setting it to not repeat leaves only the one you changed. When Google
  turns down part of a change to several dates of a repeating event, the
  event goes back to how it was and a message says why. Double-click an
  invitation to change its reminders, color and busy status.
- The assistant can tell you which calendars you have and put an event
  on the one you name, such as "the Family calendar".
- The calendar sidebar lists invitations you have not answered yet: open
  one to see it on the calendar, or open its mail when you still have
  it.
- Show in Calendar on an invitation opens the calendar on that day, with
  the event's details open. Its popover offers Open the invitation in
  Mail back to the message it arrived in.
- A Summarize button above a conversation asks the assistant for a
  summary, when the assistant is set up.
- The next event on your calendar shows at the foot of the mail sidebar
  from three hours before it starts; a click opens it in the calendar.
- An event's popover shows its notes under the place, as readable text
  with clickable links, and a repeating event says when, such as
  "Weekly on Wednesday". Its guest list shows every guest's own answer
  and marks the organizer, with Show all past a few names.
- Preferences has a Week Starts On choice, next to Working Hours:
  Automatic follows your locale, or pick Monday or Sunday yourself.
- Refresh syncs your calendars now, from the calendar's menu, F5, or
  Ctrl+R.

### Improved
- Schedule focus time or out of office from the New Event button's menu or the type switch in quick add, offered only on calendars that can keep them: Google Workspace has both, Outlook has out of office.
- The category chips over the inbox show each category's name and unread count, "Primary 2", wrapping onto a second line when one is too short, and fold to icons in a narrow window.
- Mail search shows results as you type: a moment after you stop, the mail on this computer shows first and the servers' results follow from three letters on. Enter still searches at once.
- The composer's header holds Attach Files, a More menu and Send. More has Templates, Preview, Sign and Encrypt, and a shield shows beside Attach Files while the message goes out signed or encrypted.
- Today's day, date and place over the calendar's days are dark enough to read on their tint in light.
- The mini month marks today with the only fill and the day you're viewing with a ring, and tints the week in view only in Week view.
- Section headings in the sidebar and the calendar's hour labels, week number, ALL-DAY, account names, days outside the month and agenda times are dark enough to read in light and dark.
- Conversations in the list light up under the pointer, as the sidebar's rows do.
- An event's bubble lists each guest once, calls you "You", marks guests who haven't answered with a clock, and shows a map pin beside the place.
- The Google tile shows Google's logo and the iCloud tile a cloud, in place of a colored letter.
- In the snap, when Penguin Mail cannot reach your keyring, the window and
  Add Account give the command that connects it, with a Copy button, in
  place of a keyring error. The snap's GnuPG now keeps its own keyring, so
  keys and certificates in `~/.gnupg` no longer appear there.

- The tray menu lists only the accounts with unread mail, and choosing one
  opens its inbox. With nothing unread it says "No unread mail" instead of
  listing every address.
- Add Account and the first window have a new look: pick your provider from
  a tile, see what it needs before you continue, and follow Google's
  sign-in, the server lookup and the first download on their own pages,
  with Cancel and Try Again where you need them.
- Penguin Mail now describes itself as mail and calendar for Linux, in
  the About window and in app stores.

- A new icon and logo.
- The page your browser shows after you sign in to Google says so with the
  Penguin Mail icon, in light or dark, and says how to try again if you
  didn't allow access.

- With an orange accent, buttons and pills that carry white text, such as
  Yes on an invitation and the chosen inbox category, are darker so the
  text reads clearly. Links and small accent text in mail follow the
  contrast the desktop sets for text.
- The calendar goes back as far as you look. Go to an older week, month
  or date, or scroll the Agenda up, and the events of that time load from
  Google once and stay on this computer. While they load, a small note says
  so, and offline it says older events can't load, instead of showing an
  empty week.
- Resize an event from its top edge as well as its bottom in Day and
  Week, or change its start with Ctrl+Shift+Up and Ctrl+Shift+Down.
- Drag an event to another day in Month. It keeps its time and length.
- Drag an all-day event to another day in Week, or stretch it from
  either end.
- Drop an event on the all-day row to make it all-day, or drag an
  all-day event into the hours to give it an hour at that time.
- Event reminders, the first day of the week and working hours now sit in
  the Calendar section of Preferences, on the Contacts & Calendar page.
- The Labels button in the reading pane has the same shape as the Flag
  button beside it, an icon and an arrow in one pill.
- An invitation's card now sits inside its message, under the sender and
  above the text, and scrolls with it.
- The mailboxes sit in their own rounded panel under Favorites,
  Mailboxes and Accounts, and the one you are in is tinted with your
  accent color.
- A plain-text message sits on the page, lined up with the sender's
  avatar, with no grey box around it. HTML mail keeps its white sheet.
- The inbox categories are chips: the one you are in shows its name on
  an accent pill, and the others show their unread count on the corner.
- The conversation you are reading is a tinted card in the list, and an
  invitation you have opened shows a calendar mark on its row.
- In Month, an event over several days draws as one bar across them, and
  a busy week gets more room than an empty one before it folds into "N
  more".
- Adding a Google account asks for every permission Penguin Mail uses
  in one visit to Google, and anything you leave unticked says why it
  is off, with Grant Access where you would use it. An account added
  earlier shows a bar above its mail that names what it lacks, until
  you go through Google's screen once more.
- The assistant and the invitation card read your Google calendars from
  this computer, so they answer without the network. A change the
  assistant makes waits there until Google takes it, even one to a
  single meeting of a repeating series, which leaves the others alone.
- An invitation no longer asks you to add the account to GNOME Online
  Accounts. When Penguin Mail lacks permission to read that calendar,
  the card offers Grant Access, so the event can show in Calendar.
- Moving an event, by dragging it, with Shift and an arrow key, or with
  a new time in the editor, asks first, and you can Cancel to leave it
  where it was. When the event has guests, the same question asks
  whether to send them an update; deleting one asks whether to send
  them a cancellation. A repeating event asks which dates it covers in
  that same question.
- Pressing Shift and an arrow key several times asks once, about the
  whole move. Changing the title, place, notes or guests of a meeting
  asks whether to tell the guests, and Keep Old Time in the editor's
  move question saves your other changes at the time the event had;
  Escape there throws the whole edit away.
- With Claude Opus 5 or newer, translating a message and reading an unsubscribe
  page take less time, because those quick jobs no longer think at length.
- The buttons above a conversation sit in rounded groups: replying,
  filing, moving and flagging.
- Undo Send sits at the foot of the sidebar with the seconds left. In a
  window too narrow for the sidebar it stays a toast.
- The message header is shorter: who sent it, who it went to, and
  Details for the rest.
- A sparkle button beside More opens the assistant from the reading
  pane, and the calendar's header carries the same button.
- The Day, Week and Month grids shade the hours and days outside your
  working hours, which you set in Preferences next to Event Reminders.
  The assistant's free-time tool now looks inside the same hours.
- The week starts on the day your locale expects, and every time the
  calendar shows follows your desktop's clock, 12-hour or 24-hour.
- The editor's Repeats menu offers "Monthly on the second Tuesday" or
  "the last Friday" worked out from the event's own date, and keeps
  showing such a rule from Google as that choice instead of a rule it
  cannot open. Moving the whole series a day still follows the right
  weekday.
- G opens a small date picker and jumps to the day you pick; Ctrl+Z
  undoes your last calendar change while its Undo toast is still up;
  and Shift with the Left or Right arrow moves the focused event a day
  earlier or later. The Keyboard Shortcuts window now lists every
  calendar key, including the ones that move or resize an event.
- The event popover names the event's own time zone beside the time
  when it differs from your desktop's, such as "09:00 New York".
- Offline, or when a calendar sync fails, the sidebar says so and when
  it last updated, instead of leaving you guessing.

### Fixed
- Inserting a GIF or WebP picture into a message, or showing a contact photo in such a format, no longer holds up the window while it loads. The composer shows a placeholder until the picture is ready.
- Muting a conversation on an Outlook, IMAP or POP3 account keeps its later replies out of the Inbox: each goes to the Archive, read and without a notification, while Penguin Mail runs.
- Format and Format Markdown in the composer keep the pictures in your message where they were, and Undo and Redo keep them too.
- An account that cannot start syncing no longer stops the accounts after it, as when Penguin Mail restarts itself in the tray; it shows as retrying and starts once it can.
- Penguin Mail finds the calendars and contacts of Fastmail, GMX, WEB.DE, mail.com and Zoho accounts, and when it finds none, Preferences says whether the password was refused or the server did not answer.
- Undo in the composer brings words back with the styles they had, after Format and after typing over or deleting them, and Redo brings back the lists Format made.
- Undo in the composer takes out a picture you just inserted and brings back one you deleted, and Redo does the reverse.
- Format Markdown in the composer keeps the bold, italics, links and lists you had already made.
- Pictures in a message keep the words that describe them, which go out with the message for people who cannot see the picture. A picture inserted from a file starts with the file's name, and Describe Picture on its right-click menu changes the words.
- An invitation's buttons move onto a second line in a narrow reading pane, so the card no longer runs past the edge.
- A message's details can be selected and copied. Only the Details button, now with a chevron, opens and closes them, and a collapsed message opens from a click anywhere on it.
- In Month view, the clock on an event waiting to be sent no longer covers its time, and an unanswered invitation's dashed outline no longer cuts through its time.
- In Month view, an event with a short name such as Gym shows its time again.
- A label's menu offers Move Up or Move Down only when the label has somewhere to move.
- Yes no longer looks already chosen on an invitation you haven't answered. The three answers look alike until you pick one.
- An invitation in a message and the same event in the calendar give the same date, time and count of guests who said yes.
- The week number in the calendar header is the same in Week and Day view for every day of the week, whichever day your weeks start on.
- Two short events at the same time in Week or Day show their whole titles on two lines when the column has room, rather than "Sprint p…" and "Call wit…".
- The calendar's Agenda opens with today's heading at the top, rather than with the day's first event and no day above it.
- The calendar's view switch always shows the view on screen. When the header runs short it becomes one button, such as "Week", that lists Day, Week, Month and Agenda, and the week keeps all seven days in view.
- On a phone-width window the calendar keeps New Event, Search and the window buttons in its header, with Today and the arrows beside List and Day at the bottom, and agenda rows fit their card.
- A window too narrow for the mail list beside the conversation shows one at a time, with a back button, instead of cutting off the conversation's right side.
- On a phone-width window the mail list and the open conversation fit the screen again, with times, New Message and the window buttons in view.
- Double-clicking an event opens the editor without leaving the event's bubble open beside it.
- Show more in an event's bubble opens the rest of the notes instead of closing the bubble. Notes too long for the screen scroll inside it, and Escape still closes it afterwards.
- A click on empty time in the calendar while an event's bubble is open closes the bubble and nothing more, so it no longer opens a new event that your next click has to close.
- Answering an invitation in the calendar keeps the hours where you had scrolled them, rather than jumping back to midnight.
- An event's tooltip no longer covers the bubble you just opened from it.
- The time on a calendar event is darker in light mode and brighter in dark mode, so it reads clearly on every event colour.
- The year beside the month in the calendar header is easier to read in both light and dark mode.
- A reply's quoted message folds behind "•••" again, including replies Penguin Mail sent earlier with each quoted paragraph set apart.
- The calendar sidebar shows a newly listed calendar as soon as it arrives, even before any of its events do.
- The assistant's button in the mail and calendar headers is a plain button
  like its neighbours; only its sparkle turns orange while the panel is open.
- Switching the calendar between Day, Week and Month no longer jumps months
  back to an empty range. It stays on the day you were looking at.
- The calendar's header no longer flickers, with the search button
  appearing and disappearing, at some window widths.
- A message opened in a wide window is no longer cut short when you narrow
  the window: the whole text stays readable.
- Newsletters keep their own layout: a narrow table column no longer
  breaks a word one letter per line, and a message designed at a set
  width keeps it.
- The page title some mail carries no longer shows as a stray line above
  the message.
- Mail that sets its font and colors for the whole message, as GitHub's
  notifications do, shows in that font, as in Gmail. Small icons in a
  message's tables no longer vanish, and a count beside one sits level
  with it.
- A message with a wide space character, such as some bank and shop
  receipts, opens again instead of leaving the reading pane stuck.
- Events side by side in a narrow week lane stay inside their blocks. A
  title that does not fit ends in "…", and the time shortens to the
  start, or leaves, when the lane has no room for it.
- In dark mode, a reply with quoted text shows on the dark page instead of
  in a white box.
- Picking a contact from the suggestions in an event's Guests field adds
  the guest at once and empties the field, with a gap above it; the start
  and end times now stand as tall as the dates beside them.
- Calendar search keeps the events that matter even when a common word
  matches hundreds of past ones: upcoming events first, then the most
  recent past ones, instead of whichever rows happened to load first.
- Calendar search matches an event's notes as the text they show, not
  Google's HTML tags: a `<br>` no longer matches a search for "br", and
  a word a tag splits still matches.
- The assistant's calendar tools read an event's notes as the text they
  show, not Google's HTML tags.
- Calendar search results and the "N more" popover show each event's
  end time and calendar, not just when it starts.
- At 700 pixels wide, the calendar's Day, Week and Month switch no
  longer squeezes its labels into each other; Week gives way to Month
  until the window is wide enough for all three.
- The month view's "N more" popover lets a long event title use the
  room it has before cutting it short.
- An event's notes from Google Calendar show as readable text in the
  editor instead of HTML tags, and their links still work after you
  save.
- A Google event made outside Google Calendar shows at its real time
  and zone in the editor, not in UTC, and saving it keeps that zone.
- Dragging a weekday event to another day with All events keeps it on Monday to Friday, even after the series was split with This and following.
- A guest you add to an event gets Google's invitation, even when you
  pick them from the suggestions and press Save without pressing Enter.
- Archive, Junk, Trash, All Mail, smart mailboxes and searches show mail
  you delete, junk, archive or move as soon as you open them, and no
  longer bring back mail that left them.
- Marking a Gmail message as junk from the Trash moves it to Junk, and
  deleting one from Junk moves it to the Trash, instead of leaving it
  in neither.
- Moving or changing a second calendar event while the first one's Undo is still showing no longer closes Penguin Mail.
- The Flatpak keeps its Google sign-in, IMAP passwords and AI keys in
  its own encrypted store instead of the desktop's shared keyring, which
  it no longer opens for every app to read; sign in again once to move
  an existing account over.
- `penguin-mail --demo` removes its sample store when it closes, and clears what earlier demo runs left in the temporary folder, which often lives in memory.
- The message after Undo names the label, such as Receipts, where it
  showed a code such as Label_5.
- Archiving or moving a whole conversation on an IMAP account no longer
  pulls your own reply out of Sent, or an old message out of Trash.
- Archiving, deleting or moving a conversation on an IMAP account moves
  only its messages in the folder you are looking at. A message filed
  in another folder stays there.
- An account set up manually under a listed provider, such as Fastmail,
  shows that provider's name and rules instead of its bare domain.
- `penguin-mail --demo`'s sample invitation keeps its series ending on
  the same time of day it started; the clocks going back used to move
  it an hour earlier.
- Send Later, Remind Me and Follow Up stay listed in the sidebar when
  empty, so the rows below them no longer jump.
- Dragging across empty time on the calendar stays in the day and column
  where it started, instead of scrolling the grid and offering to create
  an event that ran off the end of the wrong day.
- Double-clicking a calendar event opens its editor, instead of leaving
  only its popover open.

## 0.3.0 (2026-09-24)

### New

- A message forwarded as an attachment is listed as an .eml file named
  after its subject, which you can open or save, with its own files
  listed after it.

### Improved

- Pictures and files in a message under 2 MB open without a second
  download, since the message arrived with them inside.

## 0.2.2 (2026-09-24)

### New

- Penguin Mail speaks British English as well as American. Pick English
  (United Kingdom) under Language in Preferences, or leave it following a
  desktop set to British English.
- Penguin Mail remembers the lists you unsubscribed from. Mail that still
  arrives from one no longer offers Unsubscribe, and you can ask the
  assistant whether and when you left a list.

### Fixed

- The assistant's panel no longer runs off the right edge of a smaller
  window. The mailboxes fold away to make room for it, and in a window
  too narrow for both, the panel slides over the mail instead.
- The calendar's Day, Week and Month switch keeps its full width on a
  smaller screen. The week number, the year and the search button make
  way for it instead.
- The assistant stays closed when a narrowed window grows wide again,
  where it used to open by itself.
- Always Allow on a skill's command allows that one command. It used
  to let the skill run any command without asking, and Penguin Mail
  forgets any Always Allow saved that way.
- An S/MIME message whose certificate could not be checked for revocation
  is checked again when you open it next, so the card no longer says "could
  not check" until you restart.
- An account whose syncing crashes starts again after half a minute. If it
  crashes a second time, the sidebar marks the account instead of leaving
  it silent with no new mail.
- Invitations sent from Google Calendar show their card with Yes, No and
  Maybe again. Penguin Mail missed the invitation inside them and showed
  Google's own buttons, which open the browser.
- With the network gone, your accounts show Offline and wait for it,
  where they used to keep trying Gmail and failing. They check for mail as
  soon as the network returns.
- A screen reader reads out the items of every menu, such as Reply or
  Archive in a conversation's right-click menu. It used to announce each
  one as a menu item with no name.
- Unsubscribe finishes pages that ask why you are leaving. Penguin Mail
  ticks the box saying you no longer want the mail, and never one that
  reports the sender for spam or fraud.
- Unsubscribe no longer ticks a box such as "Send me all emails" on a
  preferences page. It ticks an "all" box only when the box says to
  unsubscribe.
- A page that answers Unsubscribe with the one word "Unsubscribed" now
  counts as done, where Penguin Mail said it could not tell.
- Open Page, after "Sent, but the page did not say it worked", opens the
  page the unsubscribe form led to, not the form you already sent.
- When a mailing list turns down an unsubscribe request, the message
  names the list's server, where it used to blame Gmail.
- The reason an unsubscribe page failed now appears in your language, not
  in English inside a Portuguese message.
- A list you leave by email hears from the address it writes to, not
  always the account's main one, and Penguin Mail says "Unsubscribed"
  only once the request has left. A request stuck in the Outbox says so.
- After the assistant unsubscribes you from a list, the open conversation
  stops offering Unsubscribe, as it does when you press the button
  yourself.
- Unsubscribe finds a button that shows only a picture, or a Submit button
  with no words on it, where it used to send you to the browser.
- An unsubscribe page that answers a moment after the press, without
  leaving the page, now counts as done instead of "did not say it worked".

## 0.2.0 (2026-09-23)

### Improved

- Adding a Gmail account opens Google's sign-in straight away. You no
  longer create a Google Cloud project first.
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
- The window keeps responding while Save to Downloads or Save All writes
  a large file from an encrypted message or a preview.
- A conversation you read before opens in about 0.2 seconds where a
  thread of forty newsletters took 1.4: the page loads once instead of
  twice, and the messages you have not opened are no longer laid out.
- A message still loading when a conversation opens, the pictures on
  attachment rows, a decrypted message and a translation now appear in
  place, so you keep your place in a long conversation instead of landing
  back at the top.
- A conversation shows its text before the pictures inside its messages,
  which fill in as they arrive. Eight large pictures used to make a 3 MB
  conversation a 12 MB page for WebKit to read.
- Opening a long conversation no longer holds up the window while its mail
  is cleaned for display: forty newsletters took the window 50 ms, and that
  work now happens on another thread.
- S/MIME signed mail opens quickly even offline. Penguin Mail waits at most
  ten seconds for the certificate authority, and when it could not check
  whether the certificate was revoked, the card says so instead of showing
  green.

### Fixed

- Accented letters in mail an older version stored show correctly again:
  "devolução", not "devoluÃ§Ã£o".
- An S/MIME signature from a certificate its authority revoked shows in
  red and says the certificate was taken back, where it used to read as a
  certificate nobody vouched for.
- An OpenPGP signature from a key only partly trusted people vouched for
  no longer shows in green. The card says the key is not fully checked.
- Unsubscribing works on pages that ask "Are you sure?" after you press
  their button, and a page that says you were
  "successfully removed" now counts as done instead of opening in your
  browser.
- Scrolling down a long mailbox while mail arrives or leaves shows each
  conversation once. The next page could repeat a conversation or skip
  one.
- Penguin Mail and `penguin-mail-cli sync` take turns on your mail. The
  one you start second stops and says why, where both used to sync the
  same store at once.
- Signing an account in again fetches the addresses it can send from. An
  alias added in Gmail used to take up to a day to reach the From menu.
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
- Pictures inside a message show when you open a conversation you read
  before. They used to appear only the first time.
- The Load Images bar no longer offers to load a picture the sender hid
  in a comment, which the message never shows.

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
