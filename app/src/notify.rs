//! Desktop notifications for new mail, and the buttons on them.
//!
//! This module says what each notification shows and which message its
//! clicks act on; the app posts them as `gio::Notification`s, which go
//! through the notification portal inside a Flatpak and straight to the
//! desktop outside one. A click comes back as the app action [`CHOSEN`]
//! with a token the app handed out, and [`Issued`] turns that token back
//! into a [`Request`]. Nothing here touches GTK or the store, so the
//! routing and the state check live under unit tests.

use std::collections::{HashMap, VecDeque};

use mailrs_domain::{MessageMeta, Role, Target};
use mailrs_sync::{MailAction, TriageAction};
use serde::{Deserialize, Serialize};

use mailrs_domain::translate::{fill, fill_plural, gettext};

/// The app action every click on a new-mail notification activates, with
/// `<token>/<key>` as its parameter: the token [`Issued::issue`] gave the
/// notification, and the key of the button pressed, or `default` for the
/// notification itself.
pub const CHOSEN: &str = "notification-chosen";

/// A button a new-mail notification can carry. Clicking the body opens the
/// conversation, so that is not one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Button {
    Archive,
    MarkRead,
    Delete,
    Reply,
}

impl Button {
    /// The buttons a new copy shows. GNOME Shell draws at most three on a
    /// notification and drops the rest, so the default is three, with
    /// Reply among them rather than Archive.
    pub const DEFAULT: [Button; 3] = [Button::MarkRead, Button::Delete, Button::Reply];

    /// Every button, in the order a notification shows them.
    pub const ALL: [Button; 4] = [
        Button::Archive,
        Button::MarkRead,
        Button::Delete,
        Button::Reply,
    ];

    /// The key a click on this button sends back.
    pub fn key(self) -> &'static str {
        match self {
            Button::Archive => "archive",
            Button::MarkRead => "mark-read",
            Button::Delete => "delete",
            Button::Reply => "reply",
        }
    }

    pub fn label(self) -> String {
        match self {
            Button::Archive => gettext("Archive"),
            Button::MarkRead => gettext("Mark as Read"),
            Button::Delete => gettext("Delete"),
            Button::Reply => gettext("Reply"),
        }
    }

    /// The mail action the button runs, or `None` for Reply, which opens a
    /// composer instead. Every one of these goes through `MailActions`, so
    /// it batches per account and Undo reverses it.
    pub fn action(self) -> Option<MailAction> {
        let triage = match self {
            Button::Archive => TriageAction::Archive,
            Button::MarkRead => TriageAction::MarkRead,
            Button::Delete => TriageAction::Trash,
            Button::Reply => return None,
        };
        Some(MailAction::Triage(triage))
    }
}

/// What the user did with a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Clicked the body, which opens the conversation.
    Open,
    Button(Button),
}

/// What a notification asks the app to do, and to which message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub target: Target,
    pub choice: Choice,
}

/// Whether `button` still has work to do on `message`: Archive while it
/// sits in the inbox, Mark as Read while it is unread, Delete until it is
/// in the Trash. A notification sits on screen long after its mail moved,
/// so the app reads the store before it acts and drops the action if the
/// mail beat the user to it.
pub fn still_applies(button: Button, message: &MessageMeta) -> bool {
    match button {
        Button::Archive => message.in_role(Role::Inbox),
        Button::MarkRead => message.is_unread(),
        Button::Delete => !message.in_role(Role::Trash),
        Button::Reply => true,
    }
}

/// One new-mail notification: what it says, the message its clicks act
/// on, and its buttons. A summary of several messages has no message and
/// no buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub target: Option<Target>,
    pub buttons: Vec<Button>,
}

/// The notifications for `messages`: one each for up to three, one summary
/// beyond that. With `previews` off, they say only how much mail arrived.
pub fn notices(messages: Vec<MessageMeta>, previews: bool, buttons: Vec<Button>) -> Vec<Notice> {
    let notice = |title, body, target: Option<Target>| Notice {
        title,
        body,
        buttons: if target.is_some() { buttons.clone() } else { Vec::new() },
        target,
    };
    let count = messages.len();
    let how_many = || {
        fill_plural(
            "{count} new message",
            "{count} new messages",
            count,
            &[("count", &count.to_string())],
        )
    };
    if !previews {
        let target = (count == 1).then(|| target_of(&messages[0]));
        return vec![notice(how_many(), String::new(), target)];
    }
    if count <= 3 {
        return messages
            .iter()
            .map(|message| {
                let sender = message
                    .from
                    .as_ref()
                    .map(|a| a.display().to_string())
                    .unwrap_or_else(|| gettext("New message"));
                let subject = if message.subject.trim().is_empty() {
                    gettext("(no subject)")
                } else {
                    message.subject.clone()
                };
                let body = format!("{subject}\n{}", message.snippet);
                notice(sender, body, Some(target_of(message)))
            })
            .collect();
    }
    let senders: Vec<String> = messages
        .iter()
        .filter_map(|m| m.from.as_ref().map(|a| a.display().to_string()))
        .take(3)
        .collect();
    let body = fill(
        &gettext("From {senders}"),
        &[("senders", &senders.join(", "))],
    );
    vec![notice(how_many(), body, None)]
}

/// The message a notification announced, not its whole thread: archiving
/// from a notification should leave the replies the reader has not seen.
fn target_of(message: &MessageMeta) -> Target {
    Target {
        account_id: message.account_id,
        thread_id: message.thread_id.clone(),
        message_id: Some(message.id.clone()),
    }
}

/// The parameter [`CHOSEN`] carries for a click on `key` of the
/// notification `token` was issued for.
pub fn parameter(token: &str, key: &str) -> String {
    format!("{token}/{key}")
}

/// The key a click on the notification itself sends.
pub const OPEN_KEY: &str = "default";

/// The notifications on screen, by the token each was posted with. The app
/// action that brings a click back can be activated by any program on the
/// session bus, so it carries only a token, and only a token handed out
/// here, for a button that notification offered, asks for anything: the
/// message a click acts on comes from this list, never from the caller.
#[derive(Debug, Default)]
pub struct Issued {
    order: VecDeque<String>,
    shown: HashMap<String, (Target, Vec<Button>)>,
}

impl Issued {
    /// How many notifications keep answering. A desktop keeps far fewer on
    /// screen, and the oldest goes once this many are newer.
    const KEPT: usize = 200;

    /// Records a notification posted under `token`, which the caller makes
    /// unguessable.
    pub fn issue(&mut self, token: String, target: Target, buttons: Vec<Button>) {
        if self.order.len() == Self::KEPT
            && let Some(oldest) = self.order.pop_front()
        {
            self.shown.remove(&oldest);
        }
        self.order.push_back(token.clone());
        self.shown.insert(token, (target, buttons));
    }

    /// What a click with `parameter` asks for, or `None` for a token this
    /// copy never issued or a button its notification did not show.
    pub fn request(&self, parameter: &str) -> Option<Request> {
        let (token, key) = parameter.split_once('/')?;
        let (target, buttons) = self.shown.get(token)?;
        let choice = choice_of(key, buttons)?;
        Some(Request { target: target.clone(), choice })
    }
}

/// The choice an action key stands for, among the buttons a notification
/// showed.
fn choice_of(key: &str, buttons: &[Button]) -> Option<Choice> {
    if key == OPEN_KEY {
        return Some(Choice::Open);
    }
    buttons
        .iter()
        .find(|button| button.key() == key)
        .map(|button| Choice::Button(*button))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_routes_back_from_its_action_key() {
        for button in Button::ALL {
            assert_eq!(
                choice_of(button.key(), &Button::ALL),
                Some(Choice::Button(button)),
                "{} lost its key",
                button.label()
            );
        }
        assert_eq!(choice_of("default", &Button::ALL), Some(Choice::Open));
    }

    #[test]
    fn a_closed_notification_asks_for_nothing() {
        assert_eq!(choice_of("__closed", &Button::ALL), None);
        assert_eq!(choice_of("archive", &[]), None, "a button nobody offered");
        assert_eq!(choice_of("", &Button::ALL), None);
    }

    fn thread(id: &str) -> Target {
        Target {
            account_id: 1,
            thread_id: id.into(),
            message_id: Some(format!("{id}-m")),
        }
    }

    #[test]
    fn a_click_comes_back_to_the_message_its_notification_announced() {
        let mut issued = Issued::default();
        issued.issue("tok1".into(), thread("t1"), vec![Button::Archive]);
        assert_eq!(
            issued.request(&parameter("tok1", Button::Archive.key())),
            Some(Request { target: thread("t1"), choice: Choice::Button(Button::Archive) })
        );
        assert_eq!(
            issued.request(&parameter("tok1", OPEN_KEY)),
            Some(Request { target: thread("t1"), choice: Choice::Open })
        );
    }

    /// Any program on the session bus can activate the app's actions, so
    /// a made-up token, or a button the notification never showed, must
    /// not reach the mail.
    #[test]
    fn only_a_token_this_copy_issued_asks_for_anything() {
        let mut issued = Issued::default();
        issued.issue("tok1".into(), thread("t1"), vec![Button::MarkRead]);
        assert_eq!(issued.request(&parameter("guess", Button::MarkRead.key())), None);
        assert_eq!(
            issued.request(&parameter("tok1", Button::Delete.key())),
            None,
            "Delete was not on that notification"
        );
        assert_eq!(issued.request("tok1"), None, "no key at all");
    }

    #[test]
    fn the_oldest_notification_stops_answering_once_enough_are_newer() {
        let mut issued = Issued::default();
        for n in 0..=Issued::KEPT {
            issued.issue(format!("tok{n}"), thread(&format!("t{n}")), vec![]);
        }
        assert_eq!(issued.request(&parameter("tok0", OPEN_KEY)), None);
        let newest = format!("tok{}", Issued::KEPT);
        assert!(issued.request(&parameter(&newest, OPEN_KEY)).is_some());
    }

    #[test]
    fn a_summary_of_many_messages_has_no_buttons_and_acts_on_nothing() {
        use mailrs_sync::fake::meta;
        let four: Vec<_> = (0..4)
            .map(|n| meta(&format!("m{n}"), &format!("t{n}"), 0, &["INBOX", "UNREAD"]))
            .collect();
        let shown = notices(four, true, Button::ALL.to_vec());
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].target, None);
        assert!(shown[0].buttons.is_empty());

        let two: Vec<_> = (0..2)
            .map(|n| meta(&format!("m{n}"), &format!("t{n}"), 0, &["INBOX"]))
            .collect();
        let shown = notices(two, true, vec![Button::Archive]);
        assert_eq!(shown.len(), 2);
        assert!(shown.iter().all(|n| n.target.is_some() && n.buttons == [Button::Archive]));
    }

    #[test]
    fn without_previews_one_message_still_gets_its_buttons() {
        use mailrs_sync::fake::meta;
        let one = vec![meta("m1", "t1", 0, &["INBOX"])];
        let shown = notices(one, false, vec![Button::Reply]);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].body, "");
        assert_eq!(shown[0].buttons, [Button::Reply]);
        assert_eq!(shown[0].target.as_ref().map(|t| t.thread_id.as_str()), Some("t1"));
    }

    #[test]
    fn three_buttons_change_mail_and_reply_opens_a_composer() {
        assert_eq!(
            Button::Archive.action(),
            Some(MailAction::Triage(TriageAction::Archive))
        );
        assert_eq!(
            Button::MarkRead.action(),
            Some(MailAction::Triage(TriageAction::MarkRead))
        );
        assert_eq!(
            Button::Delete.action(),
            Some(MailAction::Triage(TriageAction::Trash))
        );
        assert_eq!(Button::Reply.action(), None);
    }

    #[test]
    fn a_button_stops_applying_once_its_work_is_done() {
        use mailrs_sync::fake::meta;
        let unread_in_inbox = meta("m1", "t1", 0, &["INBOX", "UNREAD"]);
        let read_elsewhere = meta("m2", "t1", 0, &["Label_1"]);
        let trashed = meta("m3", "t1", 0, &["TRASH"]);
        assert!(still_applies(Button::Archive, &unread_in_inbox));
        assert!(!still_applies(Button::Archive, &read_elsewhere));
        assert!(still_applies(Button::MarkRead, &unread_in_inbox));
        assert!(!still_applies(Button::MarkRead, &read_elsewhere));
        assert!(still_applies(Button::Delete, &read_elsewhere));
        assert!(!still_applies(Button::Delete, &trashed));
        assert!(
            still_applies(Button::Reply, &trashed),
            "answering mail somebody trashed is still their business"
        );
    }
}
