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

**Tool call**: one thing the assistant asks the app to do, by name and with JSON: list a mailbox, organize mail, change a setting. `mailrs::assistant::tools` declares what the model may call and `mailrs::assistant::run` runs it against the modules. The tools change mail through the same `MailActions` and `AccountSettings` the window uses, so the assistant cannot do anything the user could not. _Avoid_: function call, command, action.

**Desk**: the port the tools read the window through: the preferences, the accounts and their labels, the view, and what is on screen, meaning the mailbox, the open conversation, and the selected rows. `mailrs::assistant::run::Desk`. Every method gives back plain data, so a test fills one in without a widget. _Avoid_: context, state, session.

**Effect port**: the port the tools change the window through: approve, open, compose, send, copy, ask for the settings permission, and the rest. `mailrs::assistant::run::Effects`. The window is one adapter behind it and the tests are another, so the whole tool loop runs with no GTK. Not to be confused with a settings `Effect`, which names a part of the window a preference left stale. _Avoid_: side effect, callback, handler.

**In-memory Gmail**: `mailrs_sync::fake::FakeGmail`, one account's mailbox held in memory behind the same `GmailApi` seam as the real client. It answers Gmail's search language, keeps a history log, and takes writes. Sync's tests and `penguin-mail --demo` both run on it, so there is one fake to keep honest rather than two; sync ships it under the `fake` feature so only the app and the tests carry it. _Avoid_: mock, stub, demo API.
