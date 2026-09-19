//! Remind Me: archive now, back in the inbox later.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::{ThreadSummary, system_label};
use mailrs_store::reminders::{self, Reminder};
use mailrs_store::threads;
use mailrs_sync::TriageAction;

use super::{MainWindow, Target};
use crate::format::future_date;

impl MainWindow {
    /// Archives the targets and brings them back to the inbox at `at`.
    pub(super) fn remind(self: &Rc<Self>, at: i64) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let rows = self.list.selected_rows();
        let open_subject = self.conversation.with_open(|o| o.subject.clone());
        let items: Vec<Reminder> = targets
            .iter()
            .map(|t| Reminder {
                account_id: t.account_id,
                thread_id: t.thread_id.clone(),
                subject: rows
                    .iter()
                    .find(|r| r.account_id == t.account_id && r.id == t.thread_id)
                    .map(|r| r.subject.clone())
                    .or_else(|| open_subject.clone())
                    .unwrap_or_default(),
                remind_at: at,
            })
            .collect();
        self.core.spawn_write(move |c| {
            for item in &items {
                reminders::set(c, item)?;
            }
            Ok(())
        });
        let when = future_date(at, chrono::Local::now());
        let next = self.list.neighbour_of_selected();
        self.conversation.clear();
        self.list.unselect();
        if let Some(next) = next {
            self.list
                .select(next.account_id, &next.id, next.message_id.as_deref());
        }
        self.apply_with(
            targets,
            TriageAction::Archive,
            true,
            Some(format!("Will remind you {when}")),
        );
        self.reminders_changed();
    }

    pub(super) fn remind_custom(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Some(at) = crate::ui::when::pick_time(
                &this.window,
                "Remind Me",
                "The conversation leaves the inbox now and comes back at this time, marked unread.",
                "Remind Me",
            )
            .await
            {
                this.remind(at);
            }
        });
    }

    /// Lists conversations waiting to come back, soonest first.
    pub(super) fn load_reminders(self: &Rc<Self>, generation: u64) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .read(|c| {
                    let mut rows = Vec::new();
                    for item in reminders::list(c)? {
                        let stored = threads::get_thread(c, item.account_id, &item.thread_id)?;
                        rows.push((item, stored));
                    }
                    Ok(rows)
                })
                .await;
            if this.list_generation.get() != generation {
                return;
            }
            let Ok(loaded) = loaded else {
                return this.toast("Could not load reminders");
            };
            let now = chrono::Local::now();
            let rows: Vec<ThreadSummary> = loaded
                .into_iter()
                .map(|(item, stored)| {
                    let mut row = stored.unwrap_or_else(|| ThreadSummary {
                        account_id: item.account_id,
                        id: item.thread_id.clone(),
                        subject: item.subject.clone(),
                        message_count: 1,
                        ..ThreadSummary::default()
                    });
                    row.snippet = format!("Returns {}", future_date(item.remind_at, now));
                    row.last_message_at = item.remind_at;
                    row
                })
                .collect();
            this.list.set_rows(rows, "No Reminders", "alarm-symbolic");
            this.follow_selection();
        });
    }

    /// Cancels reminders and puts the conversations back in the inbox now.
    pub(super) fn cancel_reminders(self: &Rc<Self>, targets: Vec<Target>) {
        if targets.is_empty() {
            return;
        }
        let gone = targets.clone();
        self.core.spawn_write(move |c| {
            for target in &gone {
                reminders::remove(c, target.account_id, &target.thread_id)?;
            }
            Ok(())
        });
        self.conversation.clear();
        self.apply_with(
            targets,
            TriageAction::Relabel {
                add: vec![system_label::INBOX.into()],
                remove: vec![],
            },
            false,
            None,
        );
        self.toast("Back in the Inbox");
        let this = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            this.reminders_changed();
        });
    }

    /// Refreshes counts, and the list when it shows Remind Me.
    pub fn reminders_changed(self: &Rc<Self>) {
        self.scheduled_changed();
    }
}
