//! Desktop notifications for new mail, and the buttons on them.
//!
//! This module says what each notification shows and which message its
//! clicks act on; `app::notifications` posts them through the desktop. A
//! click comes back as the app action [`CHOSEN`] with a token the app
//! handed out, and [`Issued`] turns that token back
//! into a [`Request`]. Nothing here touches GTK or the store, so the
//! routing and the state check live under unit tests.

use std::collections::{HashMap, HashSet, VecDeque};

use mailrs_domain::{AccountId, MessageMeta, Role, Target};
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

/// The ids among `asked` that have settled, given `still`, the ids the
/// store says are unread and in the Inbox now. Everything else counts:
/// mail read, filed elsewhere, or no longer held.
pub fn settled_among(asked: &[String], still: &HashSet<String>) -> Vec<String> {
    asked
        .iter()
        .filter(|id| !still.contains(*id))
        .cloned()
        .collect()
}

/// One new-mail notification: what it says, the message its clicks act
/// on, and its buttons. A summary of several messages has no message and
/// no buttons, but still counts each one, so it can go once they are all
/// read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub target: Option<Target>,
    pub buttons: Vec<Button>,
    /// Every message the notification announced: the one in `target`, or
    /// all the ones a summary counted. Each names its message id, which
    /// is how the app later asks the store whether it is still unread.
    pub counted: Vec<Target>,
}

/// The notifications for `messages`: one each for up to three, one summary
/// beyond that. With `previews` off, they say only how much mail arrived.
pub fn notices(messages: Vec<MessageMeta>, previews: bool, buttons: Vec<Button>) -> Vec<Notice> {
    let notice = |title, body, target: Option<Target>| Notice {
        title,
        body,
        buttons: if target.is_some() {
            buttons.clone()
        } else {
            Vec::new()
        },
        counted: match &target {
            Some(target) => vec![target.clone()],
            None => messages.iter().map(target_of).collect(),
        },
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

/// One notification on screen: the messages it counted that are still
/// unread, each as its thread id and message id, and the one its clicks
/// act on, which a summary of several has none of. A settled message
/// drops out, so a summary that counted a week of mail shrinks as the
/// mail is read and asks the store about each message once.
#[derive(Debug)]
struct Shown {
    account_id: AccountId,
    pending: Vec<(String, String)>,
    acts_on: Option<(Target, Vec<Button>)>,
}

/// The notifications on screen, by the token each was posted with. The app
/// action that brings a click back can be activated by any program on the
/// session bus, so it carries only a token, and only a token handed out
/// here, for a button that notification offered, asks for anything: the
/// message a click acts on comes from this list, never from the caller.
#[derive(Debug, Default)]
pub struct Issued {
    order: VecDeque<String>,
    shown: HashMap<String, Shown>,
}

impl Issued {
    /// How many notifications keep answering. A desktop keeps far fewer on
    /// screen, and the oldest goes once this many are newer.
    const KEPT: usize = 200;

    /// Records a notification posted under `token`, which the caller makes
    /// unguessable, whose clicks act on `target`. Answers the token of the
    /// oldest notification when this one pushes it out, for the caller to
    /// withdraw: its clicks would do nothing from now on.
    pub fn issue(&mut self, token: String, target: Target, buttons: Vec<Button>) -> Option<String> {
        let shown = Shown {
            account_id: target.account_id,
            pending: pending_of(std::slice::from_ref(&target)),
            acts_on: Some((target, buttons)),
        };
        self.keep(token, shown)
    }

    /// Records a summary posted under `token` that counted `counted`, all
    /// from one account, and acts on none of them. Its token never
    /// answers a click; it is kept so the summary can go once every
    /// message it counted is read. Answers an evicted token as `issue`.
    pub fn issue_summary(&mut self, token: String, counted: Vec<Target>) -> Option<String> {
        let first = counted.first()?;
        let shown = Shown {
            account_id: first.account_id,
            pending: pending_of(&counted),
            acts_on: None,
        };
        self.keep(token, shown)
    }

    fn keep(&mut self, token: String, shown: Shown) -> Option<String> {
        let evicted = if self.order.len() == Self::KEPT {
            self.order.pop_front()
        } else {
            None
        };
        if let Some(oldest) = &evicted {
            self.shown.remove(oldest);
        }
        self.order.push_back(token.clone());
        self.shown.insert(token, shown);
        evicted
    }

    /// The notifications still waiting on a message in `thread_ids` of
    /// `account_id`, oldest first: each token with the ids of those
    /// messages, for the caller to ask the store about. One event can name
    /// every thread of a first sync, so the ids go into a set once and
    /// each kept token looks its threads up there.
    pub fn in_threads(
        &self,
        account_id: AccountId,
        thread_ids: &[String],
    ) -> Vec<(String, Vec<String>)> {
        if self.order.is_empty() {
            return Vec::new();
        }
        let changed: HashSet<&str> = thread_ids.iter().map(String::as_str).collect();
        self.order
            .iter()
            .filter_map(|token| {
                let shown = self.shown.get(token)?;
                if shown.account_id != account_id {
                    return None;
                }
                let asked: Vec<String> = shown
                    .pending
                    .iter()
                    .filter(|(thread_id, _)| changed.contains(thread_id.as_str()))
                    .map(|(_, message_id)| message_id.clone())
                    .collect();
                (!asked.is_empty()).then(|| (token.clone(), asked))
            })
            .collect()
    }

    /// Stops waiting on the messages `settled` for `token`. Answers true
    /// once nothing is left, having forgotten the token: the caller
    /// withdraws the notification then.
    pub fn settle(&mut self, token: &str, settled: &[String]) -> bool {
        let Some(shown) = self.shown.get_mut(token) else {
            return false;
        };
        let settled: HashSet<&str> = settled.iter().map(String::as_str).collect();
        shown
            .pending
            .retain(|(_, message_id)| !settled.contains(message_id.as_str()));
        if !shown.pending.is_empty() {
            return false;
        }
        self.forget(token);
        true
    }

    /// How many messages `token` still waits on.
    #[cfg(test)]
    fn pending(&self, token: &str) -> usize {
        self.shown.get(token).map_or(0, |shown| shown.pending.len())
    }

    /// Stops answering for `token`, once its notification is withdrawn.
    fn forget(&mut self, token: &str) {
        if self.shown.remove(token).is_some() {
            self.order.retain(|kept| kept != token);
        }
    }

    /// What a click with `parameter` asks for, or `None` for a token this
    /// copy never issued or a button its notification did not show.
    pub fn request(&self, parameter: &str) -> Option<Request> {
        let (token, key) = parameter.split_once('/')?;
        let (target, buttons) = self.shown.get(token)?.acts_on.as_ref()?;
        let choice = choice_of(key, buttons)?;
        Some(Request {
            target: target.clone(),
            choice,
        })
    }
}

/// The thread and message id of each target, for a notification to wait
/// on. Every target a notice counts names its message.
fn pending_of(targets: &[Target]) -> Vec<(String, String)> {
    targets
        .iter()
        .filter_map(|t| Some((t.thread_id.clone(), t.message_id.clone()?)))
        .collect()
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
            Some(Request {
                target: thread("t1"),
                choice: Choice::Button(Button::Archive)
            })
        );
        assert_eq!(
            issued.request(&parameter("tok1", OPEN_KEY)),
            Some(Request {
                target: thread("t1"),
                choice: Choice::Open
            })
        );
    }

    /// Any program on the session bus can activate the app's actions, so
    /// a made-up token, or a button the notification never showed, must
    /// not reach the mail.
    #[test]
    fn only_a_token_this_copy_issued_asks_for_anything() {
        let mut issued = Issued::default();
        issued.issue("tok1".into(), thread("t1"), vec![Button::MarkRead]);
        assert_eq!(
            issued.request(&parameter("guess", Button::MarkRead.key())),
            None
        );
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
        for n in 0..Issued::KEPT {
            let evicted = issued.issue(format!("tok{n}"), thread(&format!("t{n}")), vec![]);
            assert_eq!(evicted, None, "there was room for tok{n}");
        }
        let newest = format!("tok{}", Issued::KEPT);
        let evicted = issued.issue(newest.clone(), thread("tN"), vec![]);
        assert_eq!(
            evicted.as_deref(),
            Some("tok0"),
            "the caller withdraws what was evicted"
        );
        assert_eq!(issued.request(&parameter("tok0", OPEN_KEY)), None);
        assert!(issued.request(&parameter(&newest, OPEN_KEY)).is_some());
    }

    #[test]
    fn a_thread_change_lists_notifications_oldest_first() {
        let mut issued = Issued::default();
        issued.issue("second".into(), thread("t2"), vec![]);
        issued.issue("first".into(), thread("t1"), vec![]);
        let tokens: Vec<String> = issued
            .in_threads(1, &["t1".to_string(), "t2".to_string()])
            .into_iter()
            .map(|(token, _)| token)
            .collect();
        assert_eq!(tokens, ["second", "first"]);
    }

    #[test]
    fn a_thread_change_finds_the_notifications_for_its_messages() {
        let mut issued = Issued::default();
        issued.issue("tok1".into(), thread("t1"), vec![]);
        issued.issue("tok2".into(), thread("t2"), vec![]);
        let mut other = thread("t1");
        other.account_id = 2;
        issued.issue("tok3".into(), other, vec![]);
        let found = issued.in_threads(1, &["t1".to_string(), "t9".to_string()]);
        assert_eq!(found, vec![("tok1".to_string(), vec!["t1-m".to_string()])]);
        assert!(issued.in_threads(3, &["t1".to_string()]).is_empty());
    }

    #[test]
    fn a_withdrawn_notification_stops_answering() {
        let mut issued = Issued::default();
        issued.issue("tok1".into(), thread("t1"), vec![Button::Reply]);
        assert!(issued.settle("tok1", &["t1-m".to_string()]));
        assert_eq!(issued.request(&parameter("tok1", OPEN_KEY)), None);
        assert!(issued.in_threads(1, &["t1".to_string()]).is_empty());
        // A token never issued, or settled twice, is nothing to settle.
        assert!(!issued.settle("never", &["t1-m".to_string()]));
        assert!(!issued.settle("tok1", &["t1-m".to_string()]));
        assert_eq!(issued.request(&parameter("tok1", OPEN_KEY)), None);
    }

    /// The ids the store would answer as still unread in the Inbox.
    fn still(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    /// A ThreadsChanged arrives, the store says one message was read and
    /// another is gone: those two tokens go, and the one still unread in
    /// the inbox stays on screen.
    #[test]
    fn a_thread_change_withdraws_the_notifications_whose_mail_settled() {
        let mut issued = Issued::default();
        issued.issue("read".into(), thread("t1"), vec![]);
        issued.issue("kept".into(), thread("t2"), vec![]);
        issued.issue("gone".into(), thread("t3"), vec![]);
        let asked = issued.in_threads(1, &["t1".into(), "t2".into(), "t3".into()]);
        let ids: Vec<String> = asked.iter().flat_map(|(_, ids)| ids.clone()).collect();
        assert_eq!(ids, ["t1-m", "t2-m", "t3-m"]);
        let found = still(&["t2-m"]);
        let withdrawn: Vec<String> = asked
            .into_iter()
            .filter(|(token, ids)| issued.settle(token, &settled_among(ids, &found)))
            .map(|(token, _)| token)
            .collect();
        assert_eq!(withdrawn, ["read", "gone"]);
        assert_eq!(
            issued.request(&parameter("read", OPEN_KEY)),
            None,
            "withdrawn, so forgotten"
        );
        assert!(issued.request(&parameter("kept", OPEN_KEY)).is_some());
    }

    /// A summary of several messages has no body action and no buttons,
    /// so a click carrying its token must ask for nothing, however the
    /// parameter is spelled.
    #[test]
    fn a_summary_token_answers_no_click() {
        let mut issued = Issued::default();
        issued.issue_summary("sum".into(), vec![thread("t1"), thread("t2")]);
        assert_eq!(issued.request(&parameter("sum", OPEN_KEY)), None);
        assert_eq!(
            issued.request(&parameter("sum", Button::Archive.key())),
            None
        );
    }

    /// A change to one thread the summary counted asks only for that
    /// thread's message. Once it settles it drops out, so a later change
    /// to the same thread asks for nothing, and the summary goes when
    /// the last message it counted has settled.
    #[test]
    fn a_summary_goes_once_every_message_it_counted_settled() {
        let mut issued = Issued::default();
        issued.issue_summary("sum".into(), vec![thread("t1"), thread("t2"), thread("t3")]);
        let asked = issued.in_threads(1, &["t2".to_string()]);
        assert_eq!(asked, vec![("sum".to_string(), vec!["t2-m".to_string()])]);

        let still_unread = still(&["t2-m"]);
        assert!(!issued.settle("sum", &settled_among(&asked[0].1, &still_unread)));
        assert_eq!(
            issued.in_threads(1, &["t2".to_string()]),
            asked,
            "still waited on"
        );

        let read = still(&[]);
        assert!(
            !issued.settle("sum", &settled_among(&asked[0].1, &read)),
            "two are left"
        );
        assert!(
            issued.in_threads(1, &["t2".to_string()]).is_empty(),
            "nothing is read twice"
        );

        let asked = issued.in_threads(1, &["t1".to_string(), "t3".to_string()]);
        assert_eq!(
            asked,
            vec![(
                "sum".to_string(),
                vec!["t1-m".to_string(), "t3-m".to_string()]
            )]
        );
        let one_gone = still(&[]);
        assert_eq!(
            settled_among(&asked[0].1, &one_gone),
            ["t1-m", "t3-m"],
            "mail the store dropped counts as settled"
        );
        assert!(
            issued.settle("sum", &["t1-m".to_string(), "t3-m".to_string()]),
            "the last one went"
        );
        assert!(issued.in_threads(1, &["t1".to_string()]).is_empty());
    }

    #[test]
    fn a_summary_keeps_only_what_it_still_waits_on() {
        let mut issued = Issued::default();
        issued.issue_summary("sum".into(), vec![thread("t1"), thread("t2")]);
        assert_eq!(issued.pending("sum"), 2);
        assert!(!issued.settle("sum", &["t1-m".to_string()]));
        assert_eq!(issued.pending("sum"), 1);
        assert!(
            !issued.settle("sum", &["never".to_string()]),
            "an id it never counted changes nothing"
        );
        assert_eq!(issued.pending("sum"), 1);
        assert!(
            !issued.settle("never", &["t2-m".to_string()]),
            "a token never issued is nothing to settle"
        );
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
        let counted: Vec<_> = shown[0]
            .counted
            .iter()
            .map(|t| t.thread_id.as_str())
            .collect();
        assert_eq!(
            counted,
            ["t0", "t1", "t2", "t3"],
            "it still knows which mail it counted"
        );

        let two: Vec<_> = (0..2)
            .map(|n| meta(&format!("m{n}"), &format!("t{n}"), 0, &["INBOX"]))
            .collect();
        let shown = notices(two, true, vec![Button::Archive]);
        assert_eq!(shown.len(), 2);
        assert!(
            shown
                .iter()
                .all(|n| n.target.is_some() && n.buttons == [Button::Archive])
        );
        assert!(
            shown
                .iter()
                .all(|n| n.counted.len() == 1 && Some(&n.counted[0]) == n.target.as_ref())
        );
    }

    #[test]
    fn without_previews_one_message_still_gets_its_buttons() {
        use mailrs_sync::fake::meta;
        let one = vec![meta("m1", "t1", 0, &["INBOX"])];
        let shown = notices(one, false, vec![Button::Reply]);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].body, "");
        assert_eq!(shown[0].buttons, [Button::Reply]);
        assert_eq!(
            shown[0].target.as_ref().map(|t| t.thread_id.as_str()),
            Some("t1")
        );
    }

    #[test]
    fn without_previews_several_messages_make_one_summary_that_counts_them_all() {
        use mailrs_sync::fake::meta;
        let two: Vec<_> = (0..2)
            .map(|n| meta(&format!("m{n}"), &format!("t{n}"), 0, &["INBOX", "UNREAD"]))
            .collect();
        let shown = notices(two, false, Button::ALL.to_vec());
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].target, None);
        assert!(shown[0].buttons.is_empty());
        let counted: Vec<_> = shown[0]
            .counted
            .iter()
            .map(|t| t.thread_id.as_str())
            .collect();
        assert_eq!(counted, ["t0", "t1"]);
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
