# Penguin Mail

Terms the code and its docs use for Gmail mail. The `domain` crate holds the code for each.

## Glossary

**System label**: a label Gmail defines, with the same id in every account, such as `INBOX`, `SPAM`, `UNREAD`, and the category labels. Code names them through `mailrs_domain::system_label`. _Avoid_: built-in label, Gmail folder.

**User label**: a label the account owner made, with an id like `Label_12` that only that account knows. _Avoid_: custom label, tag.

**Category label**: one of Gmail's `CATEGORY_*` system labels, which Gmail puts on inbox mail to sort it. _Avoid_: tab, category.

**Category**: a slice of the inbox that Penguin Mail shows: All, Primary, Updates, Promotions, or Social. Each one is defined by the category labels a thread has or lacks. Primary is mail with no category label but Personal, and Social includes Forums. _Avoid_: tab, inbox type.

**Folder**: Junk, Trash, or All Mail. The local store does not keep this mail, so the app lists a folder with a Gmail search and checks a message's labels to see if it still belongs there. _Avoid_: mailbox, spam folder.

**Target**: what a mail action applies to: a thread, or one message of it when the list shows messages instead of conversations. `mailrs_domain::Target`. _Avoid_: selection, item.

**Mail action**: a change a person or the assistant makes to targets, such as archive, flag in a colour, remind at a time, or label by name. `mailrs_sync::MailActions` runs each one for the window and the assistant alike, carries on past a failed target, and reports each failure in its `Outcome`. _Avoid_: command, operation.

**Muted**: a thread carrying Gmail's `MUTE` label. Gmail's own filters archive whatever arrives on such a thread, so a reply lands outside the inbox without the app doing anything; muting is therefore the label plus one archive, and unmuting drops the label and puts the thread back. `mailrs_sync::TriageAction::Mute`, behind `MailAction::Mute`. The Muted mailbox lists these threads and a row marks them. _Avoid_: ignore, silence, snooze.

**Undo**: the stack of recorded mail actions that Ctrl+Z or a toast's Undo button reverses, newest first. Each press takes one off and puts back that action's labels, earlier flag colours, and earlier reminders, and the toast names what it took back. The stack holds twenty actions and lasts as long as the run: mail actions are already applied at Gmail, so one that outlived the process would offer to reverse what the server has long since moved on from. A target that has left the folder the action put it in is left there, which is why undoing an archive cannot pull a conversation back out of the trash; erased mail and a signed-out account take their entries with them. The window and the assistant share it. _Avoid_: history, revert.

**Mailbox**: one choice in the sidebar, and what the thread list then shows: a label in one account or across all, a folder, a search, a smart mailbox, a flag colour, VIP mail, Follow Up, Remind Me, Send Later, or the Outbox. `mailrs_sync::Mailbox`. _Avoid_: view, source, box.

**Queued message**: a message waiting to go out, kept on this computer with the bytes it will be sent from, what the composer needs to reopen it, the tries already behind it, and the time of the next one. `mailrs_store::outbox::Queued`, sent by `mailrs_sync::Outbox`. One table holds both kinds, because both are a message waiting: a Send Later message, whose hour has not come and whose bytes Gmail holds as a draft, and one that hit a problem on the way out. The recorded problem is what tells the two apart and decides which mailbox lists the message. _Avoid_: pending message, spool entry, draft.

**Outbox**: the mailbox listing the queued messages with a problem against them, each row saying why the message has not gone and when the next try is. It appears in the sidebar only while it holds something. The wait between tries doubles from half a minute to half an hour, the network coming back cuts it short, and after about a day of tries the outbox stops and leaves the message to the person, as it does at once for a failure nothing would fix. A message inside its Undo Send window is not here yet, since nothing is stored until that delay runs out. _Avoid_: queue, spool, pending.

**Smart mailbox**: conditions saved in Preferences that become a Gmail search, such as "from Ann" and "newer than 7 days". `mailrs_domain::SmartMailbox`. _Avoid_: saved search, filter.

**Listing**: one page of a mailbox, with its rows, its unread count, the title and subtitle the header shows, and what an empty list should say. `mailrs_sync::Mailboxes` produces it for the window and the assistant alike, so only it knows which mailboxes the store answers and which Gmail does. _Avoid_: result, page, query.

**View**: the settings that change what a mailbox lists: conversation grouping, the inbox category on screen, whether Follow Up is on, the clock, and a row limit. `mailrs_sync::View`. _Avoid_: options, config, filter.

**Settings change**: one named change to the preferences, such as the text size, an account's signature, or a VIP. `mailrs::settings::Change`. Preferences, the keyboard shortcuts, and the assistant all make the same change the same way, so the same input lands the same way whichever one asks. _Avoid_: patch, setting update.

**Effect**: the part of the window a settings change leaves stale: the list's shape, the sidebar's accounts, row colours, a smart mailbox's conditions, the VIP marks, Follow Up, the inbox categories, the assistant, the text size, or light and dark. `mailrs::settings::Effect`. Applying a change reports its effects, the window redoes one part per effect, and a change with no effects saves the file and stops. _Avoid_: signal, notification.

**Account settings**: what Gmail keeps for one account and Penguin Mail changes: the automatic reply, the rules, blocked senders, and hidden addresses. `mailrs_sync::AccountSettings` makes each change for the dialogs and the assistant alike. Preferences, which live in the app's own settings file, are something else. _Avoid_: preferences, options, Gmail config.

**Automatic reply**: what Gmail sends back while the account owner is away, with the days it runs between. `mailrs_sync::AutomaticReply` names the first and last day; Gmail stores an end it stops before, and only `AccountSettings` converts between the two. _Avoid_: vacation, out of office, auto-responder.

**Rule**: one Gmail filter: which mail it matches, and what Gmail does to it as it arrives. `mailrs_domain::Filter` holds one. The Rules dialog, Block Sender, Categorize Sender, and Hide My Email all make rules. _Avoid_: filter (in wording the user reads).

**Hidden address**: one plus address from Hide My Email, such as `dana+kite.fern482@gmail.com`, with the rules behind it. One rule gives its mail the Hide My Email label; a second trashes that mail while the address is off. `mailrs_sync::HiddenFilters` holds the two rule ids. _Avoid_: masked address, burner.

**Settings permission**: the Gmail access an account grants once so Penguin Mail may read and change its account settings. Without it every `AccountSettings` call answers `Permitted::NeedsPermission`, and the caller offers Grant Access rather than showing an error. _Avoid_: scope, consent.

**Delete permission**: the Gmail access an account grants so Penguin Mail may erase mail. Sign-in never asks for it; the window asks the first time somebody chooses Delete Forever in the Trash, and until then `MailActions::erase` answers `Permitted::NeedsPermission` and changes nothing. The assistant has no tool that erases mail. _Avoid_: scope, full access.

**Tool call**: one thing the assistant asks the app to do, by name and with JSON: list a mailbox, organize mail, change a setting. `mailrs::assistant::tools` declares what the model may call and `mailrs::assistant::run` runs it against the modules. The tools change mail through the same `MailActions` and `AccountSettings` the window uses, so the assistant cannot do anything the user could not. _Avoid_: function call, command, action.

**Desk**: the port the tools read the window through: the preferences, the accounts and their labels, the view, and what is on screen, meaning the mailbox, the open conversation, and the selected rows. `mailrs::assistant::run::Desk`. Every method gives back plain data, so a test fills one in without a widget. _Avoid_: context, state, session.

**Effect port**: the port the tools change the window through: approve, open, compose, send, copy, ask for the settings permission, and the rest. `mailrs::assistant::run::Effects`. The window is one adapter behind it and the tests are another, so the whole tool loop runs with no GTK. Not to be confused with a settings `Effect`, which names a part of the window a preference left stale. _Avoid_: side effect, callback, handler.

**In-memory Gmail**: `mailrs_sync::fake::FakeGmail`, one account's mailbox held in memory behind the same `GmailApi` seam as the real client. It answers Gmail's search language, keeps a history log, and takes writes. Sync's tests and `penguin-mail --demo` both run on it, so there is one fake to keep honest rather than two; sync ships it under the `fake` feature so only the app and the tests carry it. _Avoid_: mock, stub, demo API.

**Rich body**: the composer's message while it is being written as rich text: lines with a kind, each carrying runs of styled words. `mailrs::richtext::RichBody`. It becomes the HTML part of the message, the plain text part beside it, and the Markdown a writer who prefers marks sees, and it reads Markdown back in, which is how a reply's quote and the assistant's drafts arrive. _Avoid_: document, rich text, formatted body.

**Foreground work**: a Gmail call the user is waiting on: a mail action, opening a thread, listing a folder, and whatever the assistant runs on their behalf. It takes the account's quota ahead of background work and waits out a rate limit rather than failing. `mailrs_gmail::Priority::Foreground`, which is what a call is unless some caller wrapped it in `limiter::background`. _Avoid_: user action (too narrow, the assistant counts too), interactive.

**Background work**: a Gmail call nobody is waiting on: backfill, history polling, pruning. The sync engine runs its whole tick as background work, which leaves 100 of the account's 250 unit burst for the user and stands aside while a foreground call waits. `mailrs_gmail::Priority::Background`. _Avoid_: sync work, low priority.

**Quota bucket**: the tokens one account may spend at Gmail, refilled at 200 units a second up to a 250 unit burst, under Gmail's own 250 a second. `mailrs_gmail::AccountQuota`, one per address in the OAuth client's `QuotaPool`, plus a project bucket every account waits on. Every call spends from it before it goes out, so batching and pacing show up here rather than in a 429. _Avoid_: rate limiter, throttle.

**Address book**: the contacts one Google account holds, stored on this computer: each person's name, addresses, photo, organization, and phone number. `mailrs_store::address_book` keeps one per account, and `mailrs_sync::ContactBook` reads it from the People API, walking the pages and keeping the sync token so later refreshes cost one call. It stays empty until the owner turns contacts on in Preferences, and turning them off deletes it. _Avoid_: Google Contacts, contacts (which also names the suggestion list).

**Contact**: one person in an address book, with every address Google holds for them, the primary one first. `mailrs_store::address_book::Contact`. _Avoid_: person, entry, card.

**Correspondent**: someone Penguin Mail found by reading stored mail rather than an address book. `mailrs_store::contacts::Correspondent` scores them by how often they come up, weighing someone written to above someone who only wrote. _Avoid_: contact, sender.

**Recipient suggestion**: one row of what the composer offers while an address is typed, and what search offers for a name. `mailrs_store::contacts::Suggestion` merges the address books with the correspondents: a contact comes first whatever the mail says, and mail orders the contacts among themselves. _Avoid_: completion, autocomplete entry.

**Contacts permission**: the Google access an account grants once so Penguin Mail may read its contacts. Sign-in leaves it out, and the window asks for it the first time somebody turns contacts on, so an account that never does is never asked. Without it every `ContactBook` call answers `Permitted::NeedsPermission`, as the settings calls do. _Avoid_: scope, consent.

**Contact card**: what the sender's face or name in a conversation opens: their photo, name, addresses, and organization, with the three things the app already does about a person. `mailrs::ui::contact_card`. A sender in no address book still gets one, built from the message header. _Avoid_: profile, popover, details.
**Invitation**: what one `text/calendar` part of a message says about one event: its title, when it runs, where, who is coming and what each of them said, the organizer, how it repeats, and the UID and sequence that tell one version of an event from the next. `mailrs_domain::invitation::Invitation`. _Avoid_: meeting request, calendar event, ICS.

**Answer**: Yes, No or Maybe. It is what a guest said about an invitation and what the user sends back, so one type covers both. `mailrs_domain::invitation::Answer`. It leaves by whichever of the two roads is open: Google Calendar for an event Google holds, and otherwise a reply. _Avoid_: RSVP, response, PARTSTAT.

**Reply**: the answer as RFC 5546 writes one: a mail to the organizer carrying a `METHOD:REPLY` calendar object with the invitation's UID and sequence, the organizer copied over, and the user as the only attendee. `mailrs_domain::invitation::reply` writes it and `mailrs_sync::invitations` sends it. Nothing but mail takes part, so it reaches an organizer on Exchange, an event Google never filed, and an invitation that arrived at an address the calendar does not belong to. _Avoid_: RSVP email, ICS reply, iTIP message.

**Counter proposal**: asking the organizer for another time, as a `METHOD:COUNTER` object with a new start and end. `mailrs_domain::invitation::counter`, sent the way a reply is. It settles nothing: the event stays where it was until the organizer answers, so the answer buttons stay where they were too. _Avoid_: reschedule, new time request, tentative.

**Scope**: whether an answer or a proposal covers the one occurrence an invitation names or the whole series. `mailrs_domain::invitation::Scope`. An occurrence carries the organizer's own `RECURRENCE-ID` in the reply and points the Google call at that instance; the series carries neither. The card asks only for an invitation that names one occurrence, and every other invitation is a series. _Avoid_: recurrence, range, this and future.

**Clash**: what the user already has on while an invitation's event would run, which the card says in a line: "You have Design crit then." `mailrs_sync::Invitations::busy` asks Google once per invitation, at background priority, and only for one still waiting on an answer. An event the user declined, one marked free, a cancelled one and an all-day one leave the hour open. _Avoid_: conflict, overlap, double booking.

**Invitation change**: what a message does to an event the user already has: it moves it, changes something else about it, or cancels it. `mailrs_sync::invitations::Change`. Only a version with a higher sequence changes anything, and the store keeps what each version changed, so reopening the message says the same thing twice rather than falling silent. Not to be confused with a settings `Change`, which is one named change to the preferences. _Avoid_: update, diff, revision.

**Event card**: the card above the message body that shows an invitation, with Yes, No and Maybe in it, the clash line, Propose New Time, and, for one occurrence of a repeating event, the choice between it and the series. `mailrs::ui::invitation::EventCard`. It says under the buttons where the last answer went, since an answer Google filed shows on the user's own calendar and a mailed reply does not. The message itself goes on being drawn below, so Google's own links keep working for anyone who declines the calendar permission. _Avoid_: banner, widget, preview.

**Calendar permission**: the Google Calendar access an account grants once, which lets Penguin Mail mark an answer on the user's own calendar and ask what else the hour holds. Sign-in never asks for it. Without it the answer still reaches the organizer as a reply, and the window offers Grant Access once a run rather than at every press. _Avoid_: scope, consent.

**Send-as address**: one address an account may send mail as: its own, or an alias whose owner has confirmed it. Gmail keeps a display name and a signature per address, so choosing one chooses all three. `mailrs_sync::SendAsAddress` is what Gmail reports; `mailrs::compose::SendAsAddress` is the copy Preferences keeps, so a composer opens without waiting on the network. An alias Gmail has not verified is left out, since Gmail would refuse to send from it. _Avoid_: alias, from address, sender.

**Identity**: one row of the composer's From list: a send-as address with the account it belongs to, the display name, and the signature that goes with it. `mailrs::compose::Identity`. A reply opens on the identity the mail was written to, a new message on the one that account last sent from. _Avoid_: account, profile, persona.

**Prose**: the parts of a draft a dictionary should judge. A quoted reply and a code block are not prose, because the buffer gives those lines their own kind; nor is a list marker, a run of inline code, or a picture, because those carry their own tags; nor is a word with a digit, an underscore, an `@` or a slash in it, because that is an address or an identifier. `mailrs::ui::composer::spell` reads all of this off the buffer rather than the text, so rich text and Markdown answer the same way. _Avoid_: body, content, checkable text.

**Squiggle**: the mark under a misspelling: a `gtk::TextTag` named `misspelled` with Pango's error underline. It lies beside the characters rather than in them, so it shows through bold, a link or a list, and it cannot reach the message: `richbuffer::read` builds the rich body from the block kinds, the four style tags and the link tags, and asks about nothing else. _Avoid_: highlight, underline, marker.

**Notification button**: one of Archive, Mark as Read, Delete and Reply on a new-mail desktop notification. `mailrs::notify::Button`, and Preferences chooses which of the four appear. A daemon that does not advertise the `actions` capability gets a plain notification with none of them, and the body still opens the conversation. The three that change mail go through `MailActions`, so they batch per account and Undo reverses them; Reply opens a composer. _Avoid_: notification action, hint.

**Template**: a saved body the composer drops into a message, with a name and an optional subject. `mailrs_store::templates::Template` keeps it as Markdown, which is what a rich body reads and writes, so an inserted template arrives with its styling. A template follows the person rather than an account, so every account sees the same list. _Avoid_: canned reply, snippet, boilerplate.

**Promise**: a phrase in what the writer typed that says a file is coming, such as "see attached" or "segue em anexo", with the sentence it sits in. `mailrs::attachcheck::promised` finds one in the subject and the body, reading only the writer's own words: the quoted original, the forwarded message and the signature are cut off first, and a link that spells out a phrase does not count. The composer asks before sending a message that makes a promise and carries no file, and Preferences turns the question off. _Avoid_: reminder, warning, attachment hint.

**Placeholder**: a name in double braces that a template fills in as it goes into a message: `{{first_name}}`, `{{name}}` and `{{email}}` from the first recipient, `{{subject}}`, and `{{date}}`. `mailrs::templates::expand` fills them on the way in rather than on the way out, so the saved template still reads `{{first_name}}` the next time; a name it does not know stays as written, since a body may hold braces of its own. _Avoid_: variable, token, merge field.

**Signature**: what the person's gpg made of a signed part: who signed it, whether the text is the text that was signed, and how far the trust database vouches for the signer. `mailrs_pgp::Signature`. The verdict and the trust answer different questions, so a key nobody has vouched for still gives a good signature, and a signature from a key this computer lacks is unchecked rather than bad. _Avoid_: verification result, signature check, validation.

**Canonical part**: the bytes a signature is actually made over: the part with CRLF line endings throughout and anything a mail server might rewrite encoded out of reach, which means whitespace at the end of a line and bytes above ASCII. `mailrs_pgp::mime::canonical` makes it, and the same bytes go into the message, so the two cannot drift apart. _Avoid_: normalized body, cleaned part.

**Inline PGP**: armor written into a `text/plain` body rather than into MIME parts, either an encrypted message or text left readable with its signature underneath. `mailrs_pgp::inline` finds the block among whatever a mail client wrote around it and opens it. RFC 3156 came later and never replaced the habit. _Avoid_: legacy PGP, armored text.

**Recipient key**: what gpg holds for one address a message is going to: a key it would encrypt to, or nothing. `mailrs_pgp::Recipient`. The composer offers encryption when every recipient has one and names the ones that do not; how far each key is trusted belongs in a warning, not in that decision. _Avoid_: public key, certificate, contact key.

**Mark**: what the protection card says about a message: a line naming what happened, a line saying how much it is worth, and which of the three tones the card takes. `mailrs::pgp::Mark`, which both engines fill. Good, bad and unchecked stay three answers rather than two, because a signature from a key this computer lacks is not a bad signature. _Avoid_: status, badge, verdict (which is the engine's word for one half of this).

**Protection card**: the card above the message body that shows a mark. `mailrs::ui::pgp::PgpCard`, named for the standard it was built for and since taken over by both. It sits above the event card, has no buttons, and stays put once the engine has answered, so a message that was opened out of its ciphertext goes on saying it arrived encrypted. _Avoid_: banner, badge, security indicator.

**Protection**: the wrapper a message arrived in and the standard that wrote it: `multipart/signed` or `multipart/encrypted` under RFC 3156, and under S/MIME the same `multipart/signed` plus the two blob shapes of `application/pkcs7-mime`. `mailrs_domain::Protection`, which `mailrs_gmail::body::extract_body` reads off the top-level part and only there, since a signed part further down belongs to a message somebody forwarded. Naming the standard is how the app knows which engine opens the message. The parts themselves stay out of the body: a signature covers the bytes as they were sent, so whoever checks one fetches the raw message. Inline PGP carries no wrapper and is found in the text instead. _Avoid_: encryption status, security level.

**Certificate**: what gpgsm holds for one address: the subject it names, the address on it, and its fingerprint. `mailrs_smime::Certificate`. The composer offers encryption when every recipient has one, the way it does for a recipient key, and how far the chain behind it reaches belongs in a warning rather than in that decision. _Avoid_: key, public key, cert.

**Chain**: how far the line of certificates behind a signature got: to a root in the person's own trust list, or not that far. `mailrs_smime::Chain`. It answers who the signer is, which is a different question from whether the text is the text that was signed, and a corporate certificate from an authority this computer has never been told about gives a good signature with an untrusted chain. _Avoid_: trust, validation, path.

**Standard**: which of the two, OpenPGP or S/MIME, signs or encrypts a message on the way out. `mailrs::smime::Standard`, which the composer sets on the draft: the recipients decide it for an encrypted message and the sender's own key or certificate for one that is only signed. OpenPGP wins when both could carry it, so that nothing about a message the app already knew how to send changes the day gpgsm turns up. _Avoid_: scheme, protocol, format.

**Find bar**: the bar Ctrl+F puts over an open conversation, holding the query, the count of matches, and an arrow each way. `mailrs::ui::find::FindBar` asks WebKit's own `FindController` for the text and counts the steps itself, since WebKit reports how many matches a page holds but never which one it highlighted. A query in lower case matches either case and a capital asks for that capital. Every message of the thread opens while the bar is up, because the stylesheet hides a closed message's body and WebKit finds nothing there. _Avoid_: search bar, which is the mailbox search, filter.
