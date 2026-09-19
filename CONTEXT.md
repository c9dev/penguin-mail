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
