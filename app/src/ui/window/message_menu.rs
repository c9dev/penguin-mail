//! The menu one message of an open conversation carries.
//!
//! A thread on screen is several messages, and a reader who wants only the
//! third one out of the inbox has no button for it: the header acts on the
//! whole conversation. A right click inside a message opens this menu, and
//! every item in it changes that message alone, the way a per-message label
//! change does in Gmail.
//!
//! [`groups`] decides which items a message gets, with no widget in sight:
//! the mailbox says whether Delete moves mail or erases it, and the
//! message's own marks word the read and flag items. What the items then
//! do is [`MESSAGE_ACTIONS`](super::shortcuts::MESSAGE_ACTIONS), each
//! carrying the message id as its target, so one archive goes on the undo
//! stack like any other.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{FlagColor, Folder, Role, Target};

use super::MainWindow;
use super::press::{Button, Press, Scope};
use super::reach::Reach;
use super::triage::Marks;
use crate::open_thread::OpenThread;
use crate::ui::Mailbox;
use crate::ui::conversation::{Action, ConversationView};

/// One message of the open thread, as far as its menu cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Message {
    pub id: String,
    pub unread: bool,
    pub flagged: bool,
    /// A draft has not gone out, so nobody can answer it.
    pub draft: bool,
    /// The sender's address, for Copy Address. `None` when the message
    /// names no sender.
    pub sender: Option<String>,
    /// The sender as the message header shows them, for the heading over
    /// the menu.
    pub from: Option<String>,
    /// The only message of its thread, so a change to it takes the whole
    /// conversation out of the mailbox on screen.
    pub alone: bool,
}

/// What the menu needs about one message of `open`. `None` when the thread
/// holds no such message, or when it is a queued message that Gmail has
/// never seen and none of these actions can reach.
pub(super) fn message_of(open: &OpenThread, message_id: &str) -> Option<Message> {
    if open.queued.is_some() {
        return None;
    }
    let meta = open.messages.iter().find(|m| m.id == message_id)?;
    Some(Message {
        id: meta.id.clone(),
        unread: meta.is_unread(),
        flagged: meta.is_flagged(),
        draft: meta.in_role(Role::Drafts),
        sender: meta
            .from
            .as_ref()
            .map(|from| from.email.clone())
            .filter(|email| !email.trim().is_empty()),
        from: meta
            .from
            .as_ref()
            .map(|from| from.display().to_string())
            .filter(|shown| !shown.trim().is_empty()),
        alone: open.messages.len() == 1,
    })
}

/// What an item of that menu applies to: the message alone, never the
/// conversation around it. `None` when the thread has moved on since the
/// menu opened.
pub(super) fn target_of(open: &OpenThread, message_id: &str) -> Option<Target> {
    open.messages.iter().find(|m| m.id == message_id)?;
    Some(Target {
        account_id: open.account_id,
        thread_id: open.thread_id.clone(),
        message_id: Some(message_id.to_string()),
    })
}

/// One entry of a message's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Item {
    Reply,
    ReplyAll,
    Forward,
    Archive,
    Trash,
    DeleteForever,
    MarkRead,
    MarkUnread,
    Flag,
    Unflag,
    /// Opens the seven colours the flag button offers, plus Clear Flag.
    FlagColor,
    Label,
    Export,
    CopyAddress,
}

impl Item {
    /// The words the reader sees. They name no message: the heading over
    /// the menu says which one it opened on, and a screen reader reads
    /// that heading before the items under it.
    pub(super) fn label(self) -> String {
        match self {
            Item::Reply => gettext("Reply"),
            Item::ReplyAll => gettext("Reply All"),
            Item::Forward => gettext("Forward"),
            Item::Archive => gettext("Archive"),
            Item::Trash => gettext("Move to Trash"),
            Item::DeleteForever => gettext("Delete Forever"),
            Item::MarkRead => gettext("Mark as Read"),
            Item::MarkUnread => gettext("Mark as Unread"),
            Item::Flag => gettext("Flag"),
            Item::Unflag => gettext("Unflag"),
            Item::FlagColor => gettext("Flag Color"),
            Item::Label => gettext("Labels…"),
            Item::Export => gettext("Export…"),
            Item::CopyAddress => gettext("Copy Address"),
        }
    }

    /// The `win.` action the item activates, which takes the message id as
    /// its target so the item names the message it changes. `None` for
    /// Flag Color, which opens a submenu whose own items each carry a
    /// colour beside that id.
    pub(super) fn action(self) -> Option<&'static str> {
        Some(match self {
            Item::Reply => "win.message-reply",
            Item::ReplyAll => "win.message-reply-all",
            Item::Forward => "win.message-forward",
            Item::Archive => "win.message-archive",
            Item::Trash | Item::DeleteForever => "win.message-trash",
            Item::MarkRead | Item::MarkUnread => "win.message-toggle-read",
            Item::Flag | Item::Unflag => "win.message-flag",
            Item::FlagColor => return None,
            Item::Label => "win.message-label",
            Item::Export => "win.message-export",
            Item::CopyAddress => "win.message-copy-address",
        })
    }
}

/// The menu for `message` in `mailbox`, in groups the menu draws apart.
/// An item that would do nothing here is left out rather than greyed.
pub(super) fn groups(message: &Message, mailbox: &Mailbox) -> Vec<Vec<Item>> {
    let mut groups = Vec::new();
    if !message.draft {
        groups.push(vec![Item::Reply, Item::ReplyAll, Item::Forward]);
    }
    let mut filing = vec![Item::Archive];
    filing.extend(delete_item(mailbox));
    filing.push(match message.unread {
        true => Item::MarkRead,
        false => Item::MarkUnread,
    });
    groups.push(filing);
    groups.push(vec![
        match message.flagged {
            true => Item::Unflag,
            false => Item::Flag,
        },
        Item::FlagColor,
        Item::Label,
    ]);
    let mut out = vec![Item::Export];
    if message.sender.is_some() {
        out.push(Item::CopyAddress);
    }
    groups.push(out);
    groups
}

/// What Delete comes to on one message. In the Trash there is nowhere
/// further to move it, so it is erased. A mailbox that lists something
/// other than mail has Delete call that off for the whole conversation,
/// which is no change to one message, so the item stays out.
fn delete_item(mailbox: &Mailbox) -> Option<Item> {
    match mailbox {
        Mailbox::Scheduled | Mailbox::Outbox | Mailbox::Reminders | Mailbox::FollowUp => None,
        _ if mailbox.folder() == Some(Folder::Trash) => Some(Item::DeleteForever),
        _ => Some(Item::Trash),
    }
}

/// The heading over the menu. The items under it are worded as the header
/// buttons are, so this line is what says the menu changes one message and
/// which one, to a reader and to a screen reader alike.
fn heading(message: &Message) -> String {
    match &message.from {
        Some(from) => fill(&gettext("Message from {sender}"), &[("sender", from)]),
        None => gettext("This Message"),
    }
}

/// The menu model for `message`, every item carrying its id.
fn menu_model(message: &Message, groups: &[Vec<Item>]) -> gio::Menu {
    let message_id = &message.id;
    let menu = gio::Menu::new();
    for (index, group) in groups.iter().enumerate() {
        let section = gio::Menu::new();
        for item in group {
            let Some(action) = item.action() else {
                section.append_submenu(Some(&item.label()), &flag_colors(message_id));
                continue;
            };
            let entry = gio::MenuItem::new(Some(&item.label()), None);
            entry.set_action_and_target_value(Some(action), Some(&message_id.to_variant()));
            section.append_item(&entry);
        }
        menu.append_section((index == 0).then(|| heading(message)).as_deref(), &section);
    }
    menu
}

/// The colours the flag button offers, worded for one message.
fn flag_colors(message_id: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    let colors = gio::Menu::new();
    for color in FlagColor::ALL {
        let entry = gio::MenuItem::new(Some(&color.name()), None);
        entry.set_action_and_target_value(
            Some("win.message-flag-color"),
            Some(&(color.as_str(), message_id).to_variant()),
        );
        colors.append_item(&entry);
    }
    menu.append_section(None, &colors);
    let clear = gio::Menu::new();
    let entry = gio::MenuItem::new(Some(&gettext("Clear Flag")), None);
    entry.set_action_and_target_value(
        Some("win.message-flag-color"),
        Some(&("none", message_id).to_variant()),
    );
    clear.append_item(&entry);
    menu.append_section(None, &clear);
    menu
}

impl MainWindow {
    /// Opens the menu for one message of `view`, at the point the reader
    /// asked from.
    pub(super) fn open_message_menu(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        message_id: &str,
        x: f64,
        y: f64,
    ) {
        let Some(message) = view.find(|open| message_of(open, message_id)) else {
            return;
        };
        let model = menu_model(&message, &groups(&message, &self.mailbox_of(view)));
        view.popup_message_menu(&model, x, y);
    }

    /// Runs an archive, trash or mark on one message. It reads the same
    /// table the header buttons read, with that message's own marks in
    /// place of the conversation's.
    pub(super) fn organize_message(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        action: &Action,
        message_id: &str,
    ) {
        if let Some(button) = Button::of(action) {
            self.press_message(view, message_id, Press::Button(button));
        }
    }

    /// Flags one message in `color`, or takes its flag off with `None`.
    pub(super) fn flag_message(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        message_id: &str,
        color: Option<FlagColor>,
    ) {
        self.press_message(view, message_id, Press::Flag(color));
    }

    /// Carries out `press` on one message of `view`. A thread of one
    /// message leaves the mailbox with that message; a longer one stays,
    /// and so does the reader.
    fn press_message(self: &Rc<Self>, view: &Rc<ConversationView>, message_id: &str, press: Press) {
        let Some(message) = view.find(|open| message_of(open, message_id)) else {
            return;
        };
        let Some(target) = self.message_target(view, message_id) else {
            return;
        };
        let reach = Reach {
            targets: vec![target],
            marks: Marks {
                unread: message.unread,
                flagged: message.flagged,
            },
            muted: false,
            mailbox: self.mailbox_of(view),
        };
        let scope = Scope::Message {
            alone: message.alone,
        };
        self.press_on(view, reach, scope, press);
    }

    /// Opens the label list over one message, which adds and removes that
    /// message's labels rather than the conversation's.
    pub(super) fn label_message(self: &Rc<Self>, view: &Rc<ConversationView>, message_id: &str) {
        let Some(target) = self.message_target(view, message_id) else {
            return;
        };
        let applied = view
            .find(|open| {
                open.messages
                    .iter()
                    .find(|m| m.id == message_id)
                    .map(|m| m.label_ids.iter().cloned().collect())
            })
            .unwrap_or_default();
        // No conversation to move on from: the thread keeps its other
        // messages, so the reader stays where they are.
        let popover = self.label_popover_for(vec![target], applied, None);
        view.popup_where_menu_was(&popover);
    }

    /// Puts one sender's address on the clipboard.
    pub(super) fn copy_sender_address(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        message_id: &str,
    ) {
        let Some(address) = view.find(|open| message_of(open, message_id)?.sender) else {
            return;
        };
        view.page.clipboard().set_text(&address);
        self.toast(&fill(
            &gettext("Copied {address}"),
            &[("address", &address)],
        ));
    }

    /// What an action on one message of `view` applies to.
    fn message_target(&self, view: &ConversationView, message_id: &str) -> Option<Target> {
        view.find(|open| target_of(open, message_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::{Address, MessageMeta, system_label};
    use crate::ui::Standard;
    use std::collections::HashMap;

    const ACCOUNT: mailrs_domain::AccountId = 1;

    fn meta(id: &str, labels: &[&str]) -> MessageMeta {
        MessageMeta {
            account_id: ACCOUNT,
            id: id.to_string(),
            thread_id: "t1".to_string(),
            rfc822_msgid: None,
            from: Some(Address {
                name: Some("Ana".to_string()),
                email: format!("{id}@example.com"),
            }),
            to: Vec::new(),
            cc: Vec::new(),
            subject: "Kite plans".to_string(),
            date: 0,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
            list_unsubscribe: None,
            one_click: false,
        }
    }

    fn open(messages: Vec<MessageMeta>) -> OpenThread {
        OpenThread::new(
            &Target::thread(ACCOUNT, "t1"),
            "Kite plans".to_string(),
            messages,
            HashMap::new(),
            vec!["me@example.com".to_string()],
        )
    }

    fn message() -> Message {
        Message {
            id: "m2".into(),
            unread: false,
            flagged: false,
            draft: false,
            sender: Some("ana@example.com".into()),
            from: Some("Ana".into()),
            alone: false,
        }
    }

    fn inbox() -> Mailbox {
        Mailbox::Unified(Standard::Inbox)
    }

    fn folder(folder: Folder) -> Mailbox {
        Mailbox::Folder {
            account_id: None,
            folder,
        }
    }

    fn items(message: &Message, mailbox: &Mailbox) -> Vec<Item> {
        groups(message, mailbox).concat()
    }

    #[test]
    fn an_item_names_the_message_it_was_opened_on_and_not_its_neighbours() {
        let thread = open(vec![
            meta("a", &[system_label::INBOX]),
            meta("b", &[system_label::INBOX]),
            meta("c", &[system_label::INBOX]),
        ]);
        assert_eq!(
            target_of(&thread, "b"),
            Some(Target {
                account_id: ACCOUNT,
                thread_id: "t1".to_string(),
                message_id: Some("b".to_string()),
            })
        );
        assert_eq!(target_of(&thread, "d"), None, "a message that is not there");
    }

    #[test]
    fn a_message_carries_its_own_marks_rather_than_the_threads() {
        let thread = open(vec![
            meta("a", &[system_label::UNREAD, system_label::STARRED]),
            meta("b", &[]),
        ]);
        let unread = message_of(&thread, "a").expect("the first message");
        assert!(unread.unread && unread.flagged && !unread.alone);
        assert_eq!(unread.sender.as_deref(), Some("a@example.com"));
        let read = message_of(&thread, "b").expect("the second message");
        assert!(!read.unread && !read.flagged);
    }

    #[test]
    fn a_thread_of_one_message_says_that_message_is_alone() {
        let thread = open(vec![meta("a", &[system_label::INBOX])]);
        assert!(message_of(&thread, "a").expect("the message").alone);
    }

    #[test]
    fn a_queued_message_has_no_menu_because_gmail_has_never_seen_it() {
        let mut thread = open(vec![meta("a", &[])]);
        thread.queued = Some(crate::open_thread::Unsent {
            line: "Sends in a minute".to_string(),
            stuck: false,
        });
        assert_eq!(message_of(&thread, "a"), None);
    }

    #[test]
    fn the_heading_names_the_sender_of_the_message_the_menu_opened_on() {
        assert!(heading(&message()).contains("Ana"));
        let anonymous = Message {
            from: None,
            ..message()
        };
        assert!(!heading(&anonymous).trim().is_empty());
    }

    #[test]
    fn a_message_in_the_inbox_gets_every_item() {
        assert_eq!(
            items(&message(), &inbox()),
            vec![
                Item::Reply,
                Item::ReplyAll,
                Item::Forward,
                Item::Archive,
                Item::Trash,
                Item::MarkUnread,
                Item::Flag,
                Item::FlagColor,
                Item::Label,
                Item::Export,
                Item::CopyAddress,
            ]
        );
    }

    #[test]
    fn delete_erases_a_message_that_is_already_in_the_trash() {
        assert!(items(&message(), &folder(Folder::Trash)).contains(&Item::DeleteForever));
        assert!(!items(&message(), &folder(Folder::Trash)).contains(&Item::Trash));
        assert!(items(&message(), &folder(Folder::Junk)).contains(&Item::Trash));
    }

    #[test]
    fn a_mailbox_that_lists_something_else_offers_no_delete() {
        for mailbox in [
            Mailbox::Scheduled,
            Mailbox::Outbox,
            Mailbox::Reminders,
            Mailbox::FollowUp,
        ] {
            let items = items(&message(), &mailbox);
            assert!(!items.contains(&Item::Trash), "{mailbox:?}");
            assert!(!items.contains(&Item::DeleteForever), "{mailbox:?}");
            assert!(items.contains(&Item::Archive), "{mailbox:?}");
        }
    }

    #[test]
    fn the_read_and_flag_items_follow_the_message_rather_than_the_thread() {
        let unread = Message {
            unread: true,
            flagged: true,
            ..message()
        };
        let items = items(&unread, &inbox());
        assert!(items.contains(&Item::MarkRead));
        assert!(!items.contains(&Item::MarkUnread));
        assert!(items.contains(&Item::Unflag));
        assert!(!items.contains(&Item::Flag));
    }

    #[test]
    fn a_draft_has_nobody_to_answer() {
        let draft = Message {
            draft: true,
            ..message()
        };
        let items = items(&draft, &inbox());
        assert!(!items.contains(&Item::Reply));
        assert!(!items.contains(&Item::ReplyAll));
        assert!(!items.contains(&Item::Forward));
        assert!(items.contains(&Item::Archive));
    }

    #[test]
    fn a_message_with_no_sender_offers_no_address_to_copy() {
        let anonymous = Message {
            sender: None,
            from: None,
            ..message()
        };
        assert!(!items(&anonymous, &inbox()).contains(&Item::CopyAddress));
        assert!(items(&anonymous, &inbox()).contains(&Item::Export));
    }

    #[test]
    fn every_item_a_menu_shows_says_what_it_does_and_names_an_action() {
        let states = [
            message(),
            Message {
                unread: true,
                flagged: true,
                ..message()
            },
            Message {
                draft: true,
                ..message()
            },
        ];
        for message in states {
            for mailbox in [inbox(), folder(Folder::Trash), Mailbox::Reminders] {
                for item in items(&message, &mailbox) {
                    assert!(!item.label().trim().is_empty(), "{item:?}");
                    let names_an_action = item
                        .action()
                        .is_none_or(|action| action.starts_with("win.message-"));
                    assert!(names_an_action, "{item:?}");
                }
            }
        }
    }
}
