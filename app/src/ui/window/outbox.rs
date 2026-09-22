//! The Outbox mailbox: what a person does with a message that has not
//! gone out. Edit reopens the composer on the draft the outbox kept, Send
//! Now skips the rest of its wait, and Delete drops it, as the Delete key
//! does. The three actions are off everywhere else, which keeps them out
//! of the row menu of ordinary mail.
//!
//! They act on what their view reaches, as the mail buttons do: the
//! selected rows in the main window, and the one message in a window of
//! its own opened from the Outbox.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_sync::{Mailbox, Posted, outbox_id};

use super::MainWindow;
use crate::app::Signature;
use crate::compose::Draft;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::gettext;

/// What one of the Outbox's actions runs.
struct OutboxAction(fn(&Rc<MainWindow>, &Rc<ConversationView>));

const NAMES: [&str; 3] = ["outbox-send", "outbox-edit", "outbox-delete"];

impl MainWindow {
    /// Adds the Outbox's actions to `group`, acting on what `view`
    /// reaches. The main window's start off and follow the mailbox on
    /// screen through [`MainWindow::follow_outbox`]. A window of its own
    /// keeps the mailbox it was opened from, so its actions are on for
    /// good when that was the Outbox.
    pub(super) fn install_outbox_actions(
        self: &Rc<Self>,
        group: &gio::SimpleActionGroup,
        view: &Rc<ConversationView>,
    ) {
        let each = [
            OutboxAction(MainWindow::send_queued),
            OutboxAction(MainWindow::edit_queued),
            OutboxAction(MainWindow::drop_queued),
        ];
        let on = view.detached() && self.mailbox_of(view) == Mailbox::Outbox;
        for (name, OutboxAction(run)) in NAMES.into_iter().zip(each) {
            let action = gio::SimpleAction::new(name, None);
            action.set_enabled(on);
            let (win, target) = (Rc::downgrade(self), Rc::downgrade(view));
            action.connect_activate(move |_, _| {
                if let (Some(win), Some(view)) = (win.upgrade(), target.upgrade()) {
                    run(&win, &view);
                }
            });
            group.add_action(&action);
        }
    }

    /// Turns the Outbox's own actions on while it is the mailbox on screen.
    pub(super) fn follow_outbox(self: &Rc<Self>) {
        let showing = *self.mailbox.borrow() == Mailbox::Outbox;
        for name in NAMES {
            if let Some(action) = self.actions.lookup_action(name) {
                action
                    .downcast_ref::<gio::SimpleAction>()
                    .expect("the outbox actions are simple actions")
                    .set_enabled(showing);
            }
        }
    }

    /// The waiting messages an action on `view` reaches.
    fn queued_in(&self, view: &ConversationView) -> Vec<i64> {
        self.reach(view)
            .targets
            .iter()
            .filter_map(|target| outbox_id(&target.thread_id))
            .collect()
    }

    /// Puts away what `view` showed once it has left the queue. A window
    /// of its own has nothing else to show, so it closes.
    pub(super) fn left_queue(&self, view: &ConversationView) {
        match view.detached() {
            true => view.close_detached(),
            false => self.conversation.leave(),
        }
    }

    /// Sends the waiting messages now rather than at the end of their wait.
    fn send_queued(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let waiting = self.queued_in(view);
        if waiting.is_empty() {
            return;
        }
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            for id in waiting {
                let outbox = this.core.outbox();
                let posted = this
                    .core
                    .call(async move { outbox.send_one(id).await })
                    .await;
                match posted {
                    Ok(Posted::Sent(_)) => this.toast(&gettext("Message sent")),
                    Ok(Posted::Waiting(_)) => {
                        this.toast(&gettext("Still not sent. It stays in the Outbox."))
                    }
                    Ok(Posted::Refused(problem)) => {
                        this.failed(&gettext("Not sent: {reason}"), &problem)
                    }
                    Err(err) => this.failed(&gettext("Not sent: {reason}"), &err),
                }
            }
            this.left_queue(&view);
            this.scheduled_changed();
        });
    }

    /// Opens the waiting message in a composer and takes it out of the
    /// outbox, so the writer decides again when it goes.
    fn edit_queued(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let Some(id) = self.queued_in(view).first().copied() else {
            return;
        };
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let outbox = this.core.outbox();
            let found = this.core.call(async move { outbox.find(id).await }).await;
            let Ok(Some(message)) = found else {
                return this.toast(&gettext("That message is no longer waiting."));
            };
            let Ok(draft) = serde_json::from_str::<Draft>(&message.composer) else {
                return this.toast(&gettext(
                    "Penguin Mail cannot reopen this message. Send it as it is.",
                ));
            };
            let outbox = this.core.outbox();
            if let Err(err) = this
                .core
                .call(async move { outbox.drop_one(id).await })
                .await
            {
                return this.failed(&gettext("Could not open it: {reason}"), &err);
            }
            this.left_queue(&view);
            this.scheduled_changed();
            if let Some(app) = this.app.upgrade()
                && let Some(composer) = app.open_composer(draft, Signature::AsWritten)
            {
                composer.mark_unsaved();
            }
        });
    }

    /// Drops the waiting messages. Nothing goes out and nothing is kept.
    pub(super) fn drop_queued(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let waiting = self.queued_in(view);
        if waiting.is_empty() {
            return;
        }
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let count = waiting.len();
            for id in waiting {
                let outbox = this.core.outbox();
                if let Err(err) = this
                    .core
                    .call(async move { outbox.drop_one(id).await })
                    .await
                {
                    return this.failed(&gettext("Could not delete: {reason}"), &err);
                }
            }
            this.left_queue(&view);
            this.scheduled_changed();
            this.toast(&if count == 1 {
                gettext("Deleted. It will not be sent.")
            } else {
                gettext("Deleted. They will not be sent.")
            });
        });
    }
}
