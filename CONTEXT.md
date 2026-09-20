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

**Undo**: the one recorded mail action that Ctrl+Z or a toast's Undo button reverses. It puts back labels, earlier flag colours, and earlier reminders, and it works once. The window and the assistant share it. _Avoid_: history, revert.

**Mailbox**: one choice in the sidebar, and what the thread list then shows: a label in one account or across all, a folder, a search, a smart mailbox, a flag colour, VIP mail, Follow Up, Remind Me, or Send Later. `mailrs_sync::Mailbox`. _Avoid_: view, source, box.

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

**Answer**: Yes, No or Maybe. It is what a guest said about an invitation and what the user sends back through Google Calendar, so one type covers both. `mailrs_domain::invitation::Answer`. _Avoid_: RSVP, response, PARTSTAT.

**Invitation change**: what a message does to an event the user already has: it moves it, changes something else about it, or cancels it. `mailrs_sync::invitations::Change`. Only a version with a higher sequence changes anything, and the store keeps what each version changed, so reopening the message says the same thing twice rather than falling silent. Not to be confused with a settings `Change`, which is one named change to the preferences. _Avoid_: update, diff, revision.

**Event card**: the card above the message body that shows an invitation, with Yes, No and Maybe in it. `mailrs::ui::invitation::EventCard`. The message itself goes on being drawn below, so Google's own links keep working for anyone who declines the calendar permission. _Avoid_: banner, widget, preview.

**Calendar permission**: the Google Calendar access an account grants once so Penguin Mail may answer invitations for it. Without it every `Invitations::answer` call answers `Permitted::NeedsPermission`, and the window offers Grant Access rather than showing an error. Sign-in never asks for it. _Avoid_: scope, consent.

**Send-as address**: one address an account may send mail as: its own, or an alias whose owner has confirmed it. Gmail keeps a display name and a signature per address, so choosing one chooses all three. `mailrs_sync::SendAsAddress` is what Gmail reports; `mailrs::compose::SendAsAddress` is the copy Preferences keeps, so a composer opens without waiting on the network. An alias Gmail has not verified is left out, since Gmail would refuse to send from it. _Avoid_: alias, from address, sender.

**Identity**: one row of the composer's From list: a send-as address with the account it belongs to, the display name, and the signature that goes with it. `mailrs::compose::Identity`. A reply opens on the identity the mail was written to, a new message on the one that account last sent from. _Avoid_: account, profile, persona.

**Prose**: the parts of a draft a dictionary should judge. A quoted reply and a code block are not prose, because the buffer gives those lines their own kind; nor is a list marker, a run of inline code, or a picture, because those carry their own tags; nor is a word with a digit, an underscore, an `@` or a slash in it, because that is an address or an identifier. `mailrs::ui::composer::spell` reads all of this off the buffer rather than the text, so rich text and Markdown answer the same way. _Avoid_: body, content, checkable text.

**Squiggle**: the mark under a misspelling: a `gtk::TextTag` named `misspelled` with Pango's error underline. It lies beside the characters rather than in them, so it shows through bold, a link or a list, and it cannot reach the message: `richbuffer::read` builds the rich body from the block kinds, the four style tags and the link tags, and asks about nothing else. _Avoid_: highlight, underline, marker.
