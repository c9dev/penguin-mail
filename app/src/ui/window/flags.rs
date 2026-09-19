//! Flags: Gmail's star, with Apple Mail's colours kept on this computer.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::FlagColor;
use mailrs_store::threads;
use mailrs_sync::{History, MailAction};

use super::{MainWindow, Target};

impl MainWindow {
    /// Flags the targets with `color`, or takes the flag off with `None`.
    pub(super) fn flag(self: &Rc<Self>, color: Option<FlagColor>) {
        let targets = self.targets();
        self.flag_targets(targets, color);
    }

    /// Flags `targets`. A colour picked here becomes the one the flag
    /// button uses next.
    pub(super) fn flag_targets(self: &Rc<Self>, targets: Vec<Target>, color: Option<FlagColor>) {
        if targets.is_empty() {
            return;
        }
        if let (Some(color), Some(app)) = (color, self.app.upgrade()) {
            app.update_settings(|s| s.flag_color = color);
        }
        self.perform(targets, MailAction::Flag(color), History::Record, None);
    }

    /// Re-reads the open conversation's flag colour. The store's change
    /// events do not carry it, so an undo needs this.
    pub(super) fn refresh_flag_color(self: &Rc<Self>) {
        let Some((account_id, thread_id)) = self
            .conversation
            .with_open(|o| (o.account_id, o.thread_id.clone()))
        else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let key = thread_id.clone();
            let Ok(summary) = this
                .core
                .read(move |c| threads::get_thread(c, account_id, &key))
                .await
            else {
                return;
            };
            if this.conversation.is_showing(account_id, &thread_id) {
                this.conversation
                    .with_open(|o| o.flag_color = summary.and_then(|s| s.flag_color));
                this.conversation.render_buttons();
            }
        });
    }

    /// The flag button and Ctrl+Shift+L: flag in the last colour used, or
    /// take the flag off when everything is flagged already.
    pub(super) fn toggle_flag(self: &Rc<Self>) {
        let (_, all_flagged) = self.target_marks();
        if all_flagged {
            self.flag(None);
        } else {
            self.flag(Some(self.settings().flag_color));
        }
    }
}
