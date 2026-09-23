//! Remind Me: archive now, back in the inbox later.

use std::rc::Rc;

use gtk::glib;

use super::MainWindow;
use super::press::Press;
use crate::format::future_date;
use mailrs_domain::translate::gettext;

impl MainWindow {
    /// Archives the targets and brings them back to the inbox at `at`.
    pub(super) fn remind(self: &Rc<Self>, at: i64) {
        let when = future_date(at, chrono::Local::now());
        let view = Rc::clone(&self.conversation);
        self.press(&view, Press::Remind { at, when });
    }

    pub(super) fn remind_custom(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Some(at) = crate::ui::when::pick_time(
                &this.window,
                &gettext("Remind Me"),
                &gettext(
                    "The conversation leaves the inbox now and comes back at this time, \
                     marked unread.",
                ),
                &gettext("Remind Me"),
            )
            .await
            {
                this.remind(at);
            }
        });
    }

    /// Refreshes counts, and the list when it shows Remind Me.
    pub fn reminders_changed(self: &Rc<Self>) {
        self.scheduled_changed();
    }
}
