//! The Send Later mailbox and the window's part in Undo Send.

use std::rc::Rc;

use gtk::glib;

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::gettext;
use mailrs_sync::Cancelled;

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

    /// Stops scheduled sends. The drafts stay in Gmail's Drafts, and a
    /// message Gmail never had is gone.
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
            match removed {
                Ok(cancelled) => {
                    this.left_queue(&view);
                    this.scheduled_changed();
                    this.toast(&cancelled_line(cancelled));
                }
                Err(err) => this.failed(&gettext("Could not cancel: {reason}"), &err),
            }
        });
    }
}

/// What the toast says after Cancel Send. A message scheduled while Gmail
/// was out of reach has no draft to fall back to, so the toast must not
/// send the writer to Drafts to look for it.
fn cancelled_line(cancelled: Cancelled) -> String {
    match (cancelled.in_drafts, cancelled.deleted) {
        (0, 1) => gettext("Won't be sent. Gmail never had a draft of it, so the message is gone."),
        (0, _) => gettext("Won't be sent. Gmail never had drafts of them, so the messages are gone."),
        (1, 0) => gettext("Won't be sent. The message is in Drafts."),
        (_, 0) => gettext("Won't be sent. The messages are in Drafts."),
        _ => gettext("Won't be sent. The messages Gmail had are in Drafts, and the others are gone."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(in_drafts: usize, deleted: usize) -> String {
        cancelled_line(Cancelled { in_drafts, deleted })
    }

    #[test]
    fn cancel_send_sends_the_writer_to_drafts_only_for_what_gmail_holds() {
        assert_eq!(line(1, 0), "Won't be sent. The message is in Drafts.");
        assert_eq!(line(2, 0), "Won't be sent. The messages are in Drafts.");
        assert!(!line(0, 1).contains("Drafts"), "{}", line(0, 1));
        assert!(!line(0, 3).contains("Drafts"), "{}", line(0, 3));
        assert!(line(1, 1).contains("others are gone"), "{}", line(1, 1));
    }
}
