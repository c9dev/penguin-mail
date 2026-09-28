//! The Send Later mailbox and the window's part in Undo Send.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use mailrs_domain::EpochMillis;
use mailrs_domain::translate::gettext;
use mailrs_store::outbox::Queued;
use mailrs_sync::now_millis;

use super::undo_send::{self, Surface, Waiting};
use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;

use crate::app::Signature;
use crate::compose::Draft;

/// Undo Send as the window holds it: the sends waiting out their delay,
/// what calls each one back, and the toast that stands in for the pill
/// while the sidebar is away.
#[derive(Default)]
pub(super) struct UndoSends {
    waiting: Waiting,
    undo: HashMap<u64, Box<dyn Fn()>>,
    toast: Option<adw::Toast>,
    ticking: bool,
}

impl MainWindow {
    /// Starts Undo Send for one message: the pill at the foot of the
    /// sidebar counts down its `seconds`, or a toast does while the
    /// sidebar is collapsed or hidden. `on_undo` calls the send back.
    pub(super) fn offer_undo_send(self: &Rc<Self>, seconds: u32, on_undo: impl Fn() + 'static) {
        {
            let mut sends = self.undo_sends.borrow_mut();
            let id = sends.waiting.add(now_millis(), seconds);
            sends.undo.insert(id, Box::new(on_undo));
        }
        // A new send changes how long the toast should stay, so a toast
        // already up gives way to one for all of them.
        self.drop_undo_toast();
        self.place_undo_send();
        self.tick_undo_send();
    }

    /// Calls back the newest waiting send, from the pill or the toast.
    pub(super) fn call_back_send(self: &Rc<Self>) {
        let undo = {
            let mut sends = self.undo_sends.borrow_mut();
            let Some(id) = sends.waiting.newest() else {
                return;
            };
            sends.waiting.remove(id);
            // A clicked toast dismisses itself; the sends still waiting
            // get a new one from place_undo_send.
            sends.toast = None;
            sends.undo.remove(&id)
        };
        if let Some(undo) = undo {
            undo();
        }
        self.place_undo_send();
    }

    /// Puts the countdown where it belongs now: the pill while the sidebar
    /// is beside the list, a toast otherwise, and neither once nothing
    /// waits.
    pub(super) fn place_undo_send(self: &Rc<Self>) {
        let left = self.undo_sends.borrow().waiting.left(now_millis());
        let Some(left) = left else {
            self.sidebar.hide_undo();
            self.drop_undo_toast();
            return;
        };
        match undo_send::surface(self.split.is_collapsed(), self.split.shows_sidebar()) {
            Surface::Pill => {
                self.drop_undo_toast();
                self.sidebar.show_undo(&undo_send::countdown(left));
            }
            Surface::Toast => {
                self.sidebar.hide_undo();
                if self.undo_sends.borrow().toast.is_none() {
                    self.raise_undo_toast(left);
                }
            }
        }
    }

    /// "Sending…" with Undo, up for as long as the longest wait lasts.
    fn raise_undo_toast(self: &Rc<Self>, left: EpochMillis) {
        let toast = adw::Toast::builder()
            .title(gettext("Sending…"))
            .button_label(gettext("Undo"))
            .timeout(((left + 999) / 1_000).max(1) as u32)
            .priority(adw::ToastPriority::High)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.call_back_send();
            }
        });
        self.toasts.add_toast(toast.clone());
        self.undo_sends.borrow_mut().toast = Some(toast);
    }

    fn drop_undo_toast(&self) {
        let toast = self.undo_sends.borrow_mut().toast.take();
        if let Some(toast) = toast {
            toast.dismiss();
        }
    }

    /// Counts the pill down four times a second while anything waits, and
    /// stops once nothing does. The label changes only when its words do.
    fn tick_undo_send(self: &Rc<Self>) {
        if std::mem::replace(&mut self.undo_sends.borrow_mut().ticking, true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(win) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let still = {
                let mut sends = win.undo_sends.borrow_mut();
                for id in sends.waiting.tick(now_millis()) {
                    sends.undo.remove(&id);
                }
                sends.ticking = !sends.waiting.is_empty();
                sends.ticking
            };
            win.place_undo_send();
            if still {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    /// Wires the pill, and moves the countdown between the pill and a
    /// toast as the sidebar comes and goes.
    pub(super) fn install_undo_send(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.sidebar.undo.button.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.call_back_send();
            }
        });
        for property in ["collapsed", "show-sidebar"] {
            let weak = Rc::downgrade(self);
            self.split.connect_notify_local(Some(property), move |_, _| {
                if let Some(win) = weak.upgrade() {
                    win.place_undo_send();
                }
            });
        }
    }

    /// Refreshes counts, the list when it shows one of the mailboxes that
    /// read what is waiting, and a queued message on screen, which may have
    /// gone out or failed again.
    pub(super) fn scheduled_changed(self: &Rc<Self>) {
        self.refresh_counts();
        self.refresh_open_thread();
        if matches!(
            self.shown(),
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
        if !self.open_unsent(draft) {
            return false;
        }
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

    /// Opens a composer on a message whose only copy this computer holds,
    /// marked unsaved, so closing it asks before the message is lost. The
    /// window's Cancel Send and Edit and the assistant's cancel_send all
    /// reopen a queued message this way. False when the app is closing.
    pub(super) fn open_unsent(&self, draft: Draft) -> bool {
        let Some(app) = self.app.upgrade() else {
            return false;
        };
        let Some(composer) = app.open_composer(draft, Signature::AsWritten) else {
            return false;
        };
        composer.mark_unsaved();
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
