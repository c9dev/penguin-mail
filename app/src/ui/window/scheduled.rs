//! The Send Later mailbox and the window's part in Undo Send.

use std::rc::Rc;

use gtk::glib;

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::gettext;
use mailrs_store::outbox::Queued;

use crate::app::Signature;
use crate::compose::Draft;

impl MainWindow {
    /// Shows "Sending…" with an Undo button for `seconds`.
    pub(super) fn offer_undo_send(&self, seconds: u32, on_undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder()
            .title(gettext("Sending…"))
            .button_label(gettext("Undo"))
            .timeout(seconds)
            .priority(adw::ToastPriority::High)
            .build();
        toast.connect_button_clicked(move |_| on_undo());
        self.toasts.add_toast(toast);
    }

    /// Refreshes counts, the list when it shows one of the mailboxes that
    /// read what is waiting, and a queued message on screen, which may have
    /// gone out or failed again.
    pub(super) fn scheduled_changed(self: &Rc<Self>) {
        self.refresh_counts();
        self.refresh_open_thread();
        if matches!(
            *self.mailbox.borrow(),
            Mailbox::Scheduled | Mailbox::Outbox | Mailbox::Reminders
        ) {
            self.reload_list();
        }
    }

    /// Stops scheduled sends. Each message ends up in Gmail's Drafts; one
    /// Gmail could not take opens in a composer instead, since its bytes
    /// here are the only copy.
    pub(super) fn cancel_scheduled(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        targets: Vec<Target>,
    ) {
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let outbox = this.core.outbox();
            let removed = this
                .core
                .call(async move { outbox.cancel_scheduled(&targets).await })
                .await;
            let cancelled = match removed {
                Ok(cancelled) => cancelled,
                Err(err) => return this.failed(&gettext("Could not cancel: {reason}"), &err),
            };
            let mut reopened = 0;
            for message in &cancelled.unsaved {
                if this.reopen_cancelled(message).await {
                    reopened += 1;
                }
            }
            this.left_queue(&view);
            this.scheduled_changed();
            let kept = cancelled.unsaved.len() - reopened;
            this.toast(&cancelled_line(cancelled.in_drafts, reopened, kept));
        });
    }

    /// Opens a cancelled message Gmail could not take in a composer, then
    /// drops it from the table. False when it could not be reopened, which
    /// leaves it waiting in Send Later.
    async fn reopen_cancelled(self: &Rc<Self>, message: &Queued) -> bool {
        let Ok(draft) = serde_json::from_str::<Draft>(&message.composer) else {
            return false;
        };
        let Some(app) = self.app.upgrade() else {
            return false;
        };
        let Some(composer) = app.open_composer(draft, Signature::AsWritten) else {
            return false;
        };
        composer.mark_unsaved();
        let (outbox, id) = (self.core.outbox(), message.id);
        if let Err(err) = self
            .core
            .call(async move { outbox.drop_one(id).await })
            .await
        {
            tracing::warn!(error = %err, "could not drop a cancelled message after reopening it");
        }
        true
    }
}

/// What the toast says after Cancel Send: where the messages went. One
/// Gmail could not take is open in a composer, and one that could not
/// even be reopened is still waiting.
fn cancelled_line(in_drafts: usize, reopened: usize, kept: usize) -> String {
    match (in_drafts, reopened, kept) {
        (_, _, 1..) => gettext(
            "Gmail is out of reach and Penguin Mail cannot reopen a message, so it stays in Send Later.",
        ),
        (0, 1, 0) => {
            gettext("Won't be sent. Gmail is out of reach, so the message is open for you to save.")
        }
        (0, _, 0) => gettext(
            "Won't be sent. Gmail is out of reach, so the messages are open for you to save.",
        ),
        (1, 0, 0) => gettext("Won't be sent. The message is in Drafts."),
        (_, 0, 0) => gettext("Won't be sent. The messages are in Drafts."),
        _ => gettext(
            "Won't be sent. Some are in Drafts, and the ones Gmail could not take are open for you to save.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_send_says_where_each_message_went() {
        assert_eq!(
            cancelled_line(1, 0, 0),
            "Won't be sent. The message is in Drafts."
        );
        assert_eq!(
            cancelled_line(2, 0, 0),
            "Won't be sent. The messages are in Drafts."
        );
        assert!(cancelled_line(0, 1, 0).contains("open for you to save"));
        assert!(cancelled_line(0, 2, 0).contains("messages are open"));
        assert!(cancelled_line(1, 1, 0).contains("Some are in Drafts"));
        assert!(cancelled_line(0, 0, 1).contains("stays in Send Later"));
    }
}
