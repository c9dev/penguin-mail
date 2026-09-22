//! Flags: Gmail's star, with Apple Mail's colours kept on this computer.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::FlagColor;
use mailrs_store::threads;
use mailrs_sync::{History, MailAction};

use super::{MainWindow, Target};
use crate::settings::Change;

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
            app.change_settings(Change::FlagColor(color));
        }
        self.perform(targets, MailAction::Flag(color), History::Record, None);
    }

    /// Re-reads the flag colour of every conversation on screen, the ones
    /// in windows of their own among them. The store's change events do
    /// not carry the colour, so an undo needs this.
    pub(super) fn refresh_flag_color(self: &Rc<Self>) {
        for view in self.views() {
            let Some(target) = view.read(|o| o.target()) else {
                continue;
            };
            let this = Rc::clone(self);
            glib::spawn_future_local(async move {
                let (account_id, key) = (target.account_id, target.thread_id.clone());
                let Ok(summary) = this
                    .core
                    .read(move |c| threads::get_thread(c, account_id, &key))
                    .await
                else {
                    return;
                };
                if view.is_showing(&target) {
                    view.set_flag_color(summary.and_then(|s| s.flag_color));
                }
            });
        }
    }
}
