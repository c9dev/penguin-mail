//! Desktop notifications for new mail.

use mailrs_domain::{AccountId, MessageMeta};

use crate::APP_ID;

/// Shows notifications for `messages`: one each for up to three, one
/// summary beyond that. Clicking one sends its thread to `open`.
/// With `previews` off, notifications say only how much mail arrived.
pub fn announce(
    messages: Vec<MessageMeta>,
    previews: bool,
    open: async_channel::Sender<(AccountId, String)>,
) {
    if !previews {
        let summary = if messages.len() == 1 {
            "New message".to_string()
        } else {
            format!("{} new messages", messages.len())
        };
        let target =
            (messages.len() == 1).then(|| (messages[0].account_id, messages[0].thread_id.clone()));
        show(summary, String::new(), target, open);
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
                Some((message.account_id, message.thread_id.clone())),
                open.clone(),
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
            open,
        );
    }
}

fn show(
    summary: String,
    body: String,
    target: Option<(AccountId, String)>,
    open: async_channel::Sender<(AccountId, String)>,
) {
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification
            .appname("mailrs")
            .summary(&summary)
            .body(&escape(&body))
            .icon(APP_ID)
            .hint(notify_rust::Hint::Category("email.arrived".into()))
            .hint(notify_rust::Hint::DesktopEntry(APP_ID.into()));
        if target.is_some() {
            notification.action("default", "Open");
        }
        match notification.show() {
            Ok(handle) => {
                if let Some(target) = target {
                    handle.wait_for_action(|action| {
                        if action == "default" {
                            let _ = open.send_blocking(target);
                        }
                    });
                }
            }
            Err(err) => tracing::warn!(error = %err, "could not show a notification"),
        }
    });
}

/// Notification bodies accept a little markup, so text must be escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
