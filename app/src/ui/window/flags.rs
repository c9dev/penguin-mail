//! Flags: Gmail's star, with Apple Mail's colours kept on this computer.

use std::rc::Rc;

use mailrs_domain::FlagColor;
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
}
