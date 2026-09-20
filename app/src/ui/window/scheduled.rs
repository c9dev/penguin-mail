//! The Send Later mailbox and the window's part in Undo Send.

use std::rc::Rc;

use gtk::glib;
use mailrs_store::scheduled;

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Shows "Sending…" with an Undo button for `seconds`.
    pub fn offer_undo_send(&self, seconds: u32, on_undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder()
            .title(gettext("Sending…"))
            .button_label(gettext("Undo"))
            .timeout(seconds)
            .priority(adw::ToastPriority::High)
            .build();
        toast.connect_button_clicked(move |_| on_undo());
        self.toasts.add_toast(toast);
    }

    pub fn toast_text(&self, text: &str) {
        self.toast(text);
    }

    /// Refreshes counts, and the list when it shows Send Later or Remind Me.
    pub fn scheduled_changed(self: &Rc<Self>) {
        self.refresh_counts();
        if matches!(
            *self.mailbox.borrow(),
            Mailbox::Scheduled | Mailbox::Reminders
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
                    for item in scheduled::list(c)? {
                        let hit = targets.iter().any(|t| {
                            t.account_id == item.account_id
                                && (t.message_id.as_deref() == Some(item.message_id.as_str())
                                    || t.thread_id == item.thread_id)
                        });
                        if hit {
                            scheduled::remove(c, item.account_id, &item.draft_id)?;
                        }
                    }
                    Ok(())
                })
                .await;
            match removed {
                Ok(()) => {
                    this.conversation.clear();
                    this.scheduled_changed();
                    this.toast(&if count == 1 {
                        gettext("Won't be sent. The message is in Drafts.")
                    } else {
                        gettext("Won't be sent. The messages are in Drafts.")
                    });
                }
                Err(err) => this.toast(&fill(
                    &gettext("Could not cancel: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }
}
