//! Desktop notifications for new mail, and the buttons on them.
//!
//! A notification the user acts on sends a [`Request`] back over an
//! `async-channel`, which the app reads on the GTK loop. Nothing here
//! touches GTK or the store, so the routing and the state check live under
//! unit tests even though the notification daemon does not.

use std::sync::OnceLock;

use mailrs_domain::{MessageMeta, Target, system_label};
use mailrs_sync::{MailAction, TriageAction};
use serde::{Deserialize, Serialize};

use crate::APP_ID;

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
    /// Every button, in the order a notification shows them.
    pub const ALL: [Button; 4] = [
        Button::Archive,
        Button::MarkRead,
        Button::Delete,
        Button::Reply,
    ];

    /// The action key the daemon sends back when the button is clicked.
    fn key(self) -> &'static str {
        match self {
            Button::Archive => "archive",
            Button::MarkRead => "mark-read",
            Button::Delete => "delete",
            Button::Reply => "reply",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Button::Archive => "Archive",
            Button::MarkRead => "Mark as Read",
            Button::Delete => "Delete",
            Button::Reply => "Reply",
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

/// Whether `button` still has work to do on a message carrying `labels`.
/// A notification sits on screen long after its mail moved, so the app
/// reads the store before it acts and drops the action if the mail beat
/// the user to it.
pub fn still_applies(button: Button, labels: &[String]) -> bool {
    let has = |label: &str| labels.iter().any(|l| l == label);
    match button {
        Button::Archive => has(system_label::INBOX),
        Button::MarkRead => has(system_label::UNREAD),
        Button::Delete => !has(system_label::TRASH),
        Button::Reply => true,
    }
}

/// Shows notifications for `messages`: one each for up to three, one
/// summary beyond that. Clicking one sends its thread to `chosen`, as does
/// each of `buttons`. With `previews` off, notifications say only how much
/// mail arrived.
pub fn announce(
    messages: Vec<MessageMeta>,
    previews: bool,
    buttons: Vec<Button>,
    chosen: async_channel::Sender<Request>,
) {
    if !previews {
        let summary = if messages.len() == 1 {
            "New message".to_string()
        } else {
            format!("{} new messages", messages.len())
        };
        let target = (messages.len() == 1).then(|| target_of(&messages[0]));
        show(summary, String::new(), target, buttons, chosen);
        return;
    }
    if messages.len() <= 3 {
        for message in messages {
            let sender = message
                .from
                .as_ref()
                .map(|a| a.display().to_string())
                .unwrap_or_else(|| "New message".into());
            let subject = if message.subject.trim().is_empty() {
                "(no subject)".to_string()
            } else {
                message.subject.clone()
            };
            let body = format!("{subject}\n{}", message.snippet);
            show(
                sender,
                body,
                Some(target_of(&message)),
                buttons.clone(),
                chosen.clone(),
            );
        }
    } else {
        let senders: Vec<String> = messages
            .iter()
            .filter_map(|m| m.from.as_ref().map(|a| a.display().to_string()))
            .take(3)
            .collect();
        show(
            format!("{} new messages", messages.len()),
            format!("From {}", senders.join(", ")),
            None,
            buttons,
            chosen,
        );
    }
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

fn show(
    summary: String,
    body: String,
    target: Option<Target>,
    buttons: Vec<Button>,
    chosen: async_channel::Sender<Request>,
) {
    std::thread::spawn(move || {
        let target = target.filter(|_| takes_actions());
        let mut notification = notify_rust::Notification::new();
        notification
            .appname("Penguin Mail")
            .summary(&summary)
            .body(&escape(&body))
            .icon(APP_ID)
            .hint(notify_rust::Hint::Category("email.arrived".into()))
            .hint(notify_rust::Hint::DesktopEntry(APP_ID.into()));
        if target.is_some() {
            notification.action("default", "Open");
            for button in &buttons {
                notification.action(button.key(), button.label());
            }
        }
        match notification.show() {
            Ok(handle) => {
                let Some(target) = target else { return };
                handle.wait_for_action(|key| {
                    if let Some(choice) = choice_of(key, &buttons) {
                        let _ = chosen.send_blocking(Request { target, choice });
                    }
                });
            }
            Err(err) => tracing::warn!(error = %err, "could not show a notification"),
        }
    });
}

/// The choice an action key stands for. A daemon also reports a closed
/// notification through this path, and one closed without a click asks for
/// nothing.
fn choice_of(key: &str, buttons: &[Button]) -> Option<Choice> {
    if key == "default" {
        return Some(Choice::Open);
    }
    buttons
        .iter()
        .find(|button| button.key() == key)
        .map(|button| Choice::Button(*button))
}

/// Whether the running notification daemon invokes actions, asked once.
/// Without that capability the buttons would sit on screen doing nothing,
/// so a plain notification goes out instead.
fn takes_actions() -> bool {
    static TAKES: OnceLock<bool> = OnceLock::new();
    *TAKES.get_or_init(|| match notify_rust::get_capabilities() {
        Ok(capabilities) => capabilities.iter().any(|c| c == "actions"),
        Err(err) => {
            tracing::warn!(error = %err, "could not ask the notification daemon what it does");
            false
        }
    })
}

/// Notification bodies accept a little markup, so text must be escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
    fn an_action_the_mail_no_longer_needs_is_dropped() {
        let inbox = [system_label::INBOX.to_string(), system_label::UNREAD.into()];
        assert!(Button::ALL.into_iter().all(|b| still_applies(b, &inbox)));

        let read = [system_label::INBOX.to_string()];
        assert!(!still_applies(Button::MarkRead, &read));
        assert!(still_applies(Button::Archive, &read));

        let archived = [system_label::UNREAD.to_string()];
        assert!(!still_applies(Button::Archive, &archived));
        assert!(still_applies(Button::MarkRead, &archived));

        let trashed = [system_label::TRASH.to_string()];
        assert!(!still_applies(Button::Delete, &trashed));
        assert!(
            still_applies(Button::Reply, &trashed),
            "answering mail somebody trashed is still their business"
        );
    }
}
