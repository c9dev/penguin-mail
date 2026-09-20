//! The Outbox mailbox: what a person does with a message that has not
//! gone out. Edit reopens the composer on the draft the outbox kept, Send
//! Now skips the rest of its wait, and Delete drops it, as the Delete key
//! does. The three actions are off everywhere else, which keeps them out
//! of the row menu of ordinary mail.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_sync::{Mailbox, Posted, outbox_id};

use super::MainWindow;
use crate::compose::Draft;

/// What one of the Outbox's actions runs.
struct OutboxAction(fn(&Rc<MainWindow>));

impl MainWindow {
    pub(super) fn install_outbox_actions(self: &Rc<Self>) {
        let each = [
            ("outbox-send", OutboxAction(MainWindow::send_queued)),
            ("outbox-edit", OutboxAction(MainWindow::edit_queued)),
            ("outbox-delete", OutboxAction(MainWindow::drop_queued)),
        ];
        for (name, OutboxAction(run)) in each {
            let action = gio::SimpleAction::new(name, None);
            action.set_enabled(false);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(win) = weak.upgrade() {
                    run(&win);
                }
            });
            self.actions.add_action(&action);
        }
    }

    /// Turns the Outbox's own actions on while it is the mailbox on screen.
    pub(super) fn follow_outbox(self: &Rc<Self>) {
        let showing = *self.mailbox.borrow() == Mailbox::Outbox;
        for name in ["outbox-send", "outbox-edit", "outbox-delete"] {
            if let Some(action) = self.actions.lookup_action(name) {
                action
                    .downcast_ref::<gio::SimpleAction>()
                    .expect("the outbox actions are simple actions")
                    .set_enabled(showing);
            }
        }
    }

    /// The waiting messages the selected rows stand for.
    fn queued_rows(&self) -> Vec<i64> {
        self.list
            .selected_rows()
            .iter()
            .filter_map(|row| outbox_id(&row.id))
            .collect()
    }

    /// Sends the selected messages now rather than at the end of their wait.
    fn send_queued(self: &Rc<Self>) {
        let waiting = self.queued_rows();
        if waiting.is_empty() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            for id in waiting {
                let outbox = this.core.outbox();
                let posted = this
                    .core
                    .call(async move { outbox.send_one(id).await })
                    .await;
                match posted {
                    Ok(Posted::Sent(_)) => this.toast("Message sent"),
                    Ok(Posted::Waiting(_)) => this.toast("Still not sent. It stays in the Outbox."),
                    Ok(Posted::Refused(problem)) => this.toast(&format!("Not sent: {problem}")),
                    Err(err) => this.toast(&format!("Not sent: {err}")),
                }
            }
            this.conversation.clear();
            this.scheduled_changed();
        });
    }

    /// Opens the selected message in a composer and takes it out of the
    /// outbox, so the writer decides again when it goes.
    fn edit_queued(self: &Rc<Self>) {
        let Some(id) = self.queued_rows().first().copied() else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let outbox = this.core.outbox();
            let found = this.core.call(async move { outbox.find(id).await }).await;
            let Ok(Some(message)) = found else {
                return this.toast("That message is no longer waiting.");
            };
            let Ok(draft) = serde_json::from_str::<Draft>(&message.composer) else {
                return this.toast("Penguin Mail cannot reopen this message. Send it as it is.");
            };
            let outbox = this.core.outbox();
            if let Err(err) = this
                .core
                .call(async move { outbox.drop_one(id).await })
                .await
            {
                return this.toast(&format!("Could not open it: {err}"));
            }
            this.conversation.clear();
            this.scheduled_changed();
            if let Some(app) = this.app.upgrade()
                && let Some(composer) = app.compose(draft)
            {
                composer.mark_unsaved();
            }
        });
    }

    /// Drops the selected messages. Nothing goes out and nothing is kept.
    pub(super) fn drop_queued(self: &Rc<Self>) {
        let waiting = self.queued_rows();
        if waiting.is_empty() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let count = waiting.len();
            for id in waiting {
                let outbox = this.core.outbox();
                if let Err(err) = this
                    .core
                    .call(async move { outbox.drop_one(id).await })
                    .await
                {
                    return this.toast(&format!("Could not delete: {err}"));
                }
            }
            this.conversation.clear();
            this.scheduled_changed();
            this.toast(if count == 1 {
                "Deleted. It will not be sent."
            } else {
                "Deleted. They will not be sent."
            });
        });
    }
}
