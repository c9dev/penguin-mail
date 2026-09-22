//! The Send Later mailbox and the window's part in Undo Send.

use std::rc::Rc;

use gtk::glib;

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use mailrs_domain::translate::gettext;

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

    /// Refreshes counts, and the list when it shows one of the mailboxes
    /// that read what is waiting.
    pub(super) fn scheduled_changed(self: &Rc<Self>) {
        self.refresh_counts();
        if matches!(
            *self.mailbox.borrow(),
            Mailbox::Scheduled | Mailbox::Outbox | Mailbox::Reminders
        ) {
            self.reload_list();
        }
    }

    /// Stops scheduled sends. The drafts stay in Gmail's Drafts.
    pub(super) fn cancel_scheduled(self: &Rc<Self>, targets: Vec<Target>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let count = targets.len();
            let outbox = this.core.outbox();
            let removed = this
                .core
                .call(async move { outbox.cancel_scheduled(&targets).await })
                .await;
            match removed {
                Ok(_) => {
                    this.conversation.leave();
                    this.scheduled_changed();
                    this.toast(&if count == 1 {
                        gettext("Won't be sent. The message is in Drafts.")
                    } else {
                        gettext("Won't be sent. The messages are in Drafts.")
                    });
                }
                Err(err) => this.failed(&gettext("Could not cancel: {reason}"), &err),
            }
        });
    }
}
