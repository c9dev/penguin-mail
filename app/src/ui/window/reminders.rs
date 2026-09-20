//! Remind Me: archive now, back in the inbox later.

use std::rc::Rc;

use gtk::glib;
use mailrs_sync::{History, MailAction};

use super::{MainWindow, Target};
use crate::format::future_date;
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Archives the targets and brings them back to the inbox at `at`.
    pub(super) fn remind(self: &Rc<Self>, at: i64) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let when = future_date(at, chrono::Local::now());
        let next = self.list.neighbour_of_selected();
        self.conversation.clear();
        self.list.unselect();
        if let Some(next) = next {
            self.list
                .select(next.account_id, &next.id, next.message_id.as_deref());
        }
        self.perform(
            targets,
            MailAction::Remind { at },
            History::Record,
            Some(fill(&gettext("Will remind you {when}"), &[("when", &when)])),
        );
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

    /// Cancels reminders and puts the conversations back in the inbox now.
    pub(super) fn cancel_reminders(self: &Rc<Self>, targets: Vec<Target>) {
        if targets.is_empty() {
            return;
        }
        self.conversation.clear();
        self.perform(targets, MailAction::CancelReminder, History::Skip, None);
        self.toast(&gettext("Back in the Inbox"));
    }

    /// Refreshes counts, and the list when it shows Remind Me.
    pub fn reminders_changed(self: &Rc<Self>) {
        self.scheduled_changed();
    }
}
