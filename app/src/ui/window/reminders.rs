//! Remind Me: archive now, back in the inbox later.

use std::rc::Rc;

use gtk::glib;
use mailrs_sync::{History, MailAction};

use super::{MainWindow, Target};
use crate::format::future_date;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Archives the targets and brings them back to the inbox at `at`.
    pub(super) fn remind(self: &Rc<Self>, at: i64) {
        let targets = self.reach(&self.conversation).targets;
        if targets.is_empty() {
            return;
        }
        let when = future_date(at, chrono::Local::now());
        let action = MailAction::Remind { at };
        self.follow_out(&self.conversation, &action);
        self.perform(
            targets,
            action,
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

    /// Cancels reminders on the targets, which `view` reached, and puts
    /// the conversations back in the inbox now.
    pub(super) fn cancel_reminders(self: &Rc<Self>, view: &ConversationView, targets: Vec<Target>) {
        if targets.is_empty() {
            return;
        }
        let action = MailAction::CancelReminder;
        self.follow_out(view, &action);
        self.perform(targets, action, History::Skip, None);
        self.toast(&gettext("Back in the Inbox"));
    }

    /// Refreshes counts, and the list when it shows Remind Me.
    pub fn reminders_changed(self: &Rc<Self>) {
        self.scheduled_changed();
    }
}
