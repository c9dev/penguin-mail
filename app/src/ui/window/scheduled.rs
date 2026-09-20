//! The Send Later mailbox and the window's part in Undo Send.

use std::rc::Rc;

use gtk::glib;
use mailrs_store::outbox;
use mailrs_sync::outbox_id;

use super::{MainWindow, Target};
use crate::ui::Mailbox;

impl MainWindow {
    /// Shows "Sending…" with an Undo button for `seconds`.
    pub fn offer_undo_send(&self, seconds: u32, on_undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder()
            .title("Sending…")
            .button_label("Undo")
            .timeout(seconds)
            .priority(adw::ToastPriority::High)
            .build();
        toast.connect_button_clicked(move |_| on_undo());
        self.toasts.add_toast(toast);
    }

    pub fn toast_text(&self, text: &str) {
        self.toast(text);
    }

    /// Refreshes counts, and the list when it shows one of the mailboxes
    /// that read what is waiting.
    pub fn scheduled_changed(self: &Rc<Self>) {
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
            let removed = this
                .core
                .write(move |c| {
                    for item in outbox::scheduled(c)? {
                        // A row names its Gmail thread, or, for a message
                        // Gmail has never seen, its own place in the table.
                        let hit = targets.iter().any(|t| {
                            t.account_id == item.account_id
                                && (outbox_id(&t.thread_id) == Some(item.id)
                                    || item.thread_id.as_deref() == Some(t.thread_id.as_str())
                                    || (item.message_id.is_some()
                                        && t.message_id == item.message_id))
                        });
                        if hit {
                            outbox::remove(c, item.id)?;
                        }
                    }
                    Ok(())
                })
                .await;
            match removed {
                Ok(()) => {
                    this.conversation.clear();
                    this.scheduled_changed();
                    this.toast(if count == 1 {
                        "Won't be sent. The message is in Drafts."
                    } else {
                        "Won't be sent. The messages are in Drafts."
                    });
                }
                Err(err) => this.toast(&format!("Could not cancel: {err}")),
            }
        });
    }
}
