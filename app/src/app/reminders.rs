//! Event reminders on the desktop: the check that reads the calendar
//! copy and posts what is due, the timer that brings the next check, and
//! the three things a reminder offers. What is due and when to look
//! again come from [`crate::event_reminders::plan`]; this file reads the
//! store, posts, and records.
//!
//! Notifications go through `gio::Notification`. GNOME Shell and the
//! portal hand a click back to the app's bus name, which the process
//! owns again after the idle restart, so a reminder left in the message
//! tray still works after it.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::calendar::Calendar;
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::accounts;
use mailrs_store::calendar::{self, CalendarScope};
use mailrs_store::event_reminders::{self as shown_log, Key};
use mailrs_sync::now_millis;

use super::App;
use crate::event_reminders::notice;
use crate::event_reminders::plan::{self, Due, LOOK_AHEAD, LOOK_BACK, SNOOZE};

impl App {
    /// Registers the reminder actions and runs the first check. Each
    /// check sets the timer for the next.
    pub(super) fn start_event_reminders(self: &Rc<Self>) {
        self.install_reminder_actions();
        self.check_event_reminders();
    }

    /// Reads the copy, posts what is due, records it, and sets the timer.
    /// A call while a check runs is dropped: the running one sets the
    /// timer when it ends, at most a minute away.
    pub(crate) fn check_event_reminders(self: &Rc<Self>) {
        if self.reminders_running.replace(true) {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let now = now_millis();
            let read = this
                .core
                .read(move |c| {
                    let ids: Vec<AccountId> =
                        accounts::list_accounts(c)?.into_iter().map(|a| a.id).collect();
                    let occurrences =
                        calendar::occurrences(c, &ids, now - LOOK_BACK, now + LOOK_AHEAD, CalendarScope::Shown)?;
                    let mut calendars: HashMap<(AccountId, String), Calendar> = HashMap::new();
                    for &id in &ids {
                        for held in calendar::calendars(c, id)? {
                            calendars.insert((id, held.id.clone()), held);
                        }
                    }
                    Ok((occurrences, calendars, shown_log::log(c)?))
                })
                .await;
            let next = match read {
                Ok((occurrences, calendars, log)) => {
                    let enabled = this.settings_with(|s| s.event_reminders);
                    let plan = plan::plan(&occurrences, &calendars, &log, now, enabled);
                    for due in &plan.post {
                        this.post_reminder(due, now);
                    }
                    let done: Vec<(Key, EpochMillis)> = plan
                        .post
                        .iter()
                        .chain(&plan.quiet)
                        .map(|due| (due.key.clone(), due.ends))
                        .collect();
                    // Pruning runs on every check, whether or not
                    // anything is newly due, so a row past its keep time
                    // never lingers through a quiet stretch.
                    if let Err(err) = this.core.write(move |c| shown_log::mark_shown(c, &done, now)).await {
                        tracing::warn!(error = %err, "could not record the event reminders shown");
                    }
                    plan.next
                }
                Err(err) => {
                    tracing::warn!(error = %err, "could not read the calendar for reminders");
                    None
                }
            };
            this.reminders_running.set(false);
            this.wake_reminders_at(next, now);
        });
    }

    fn wake_reminders_at(self: &Rc<Self>, next: Option<EpochMillis>, now: EpochMillis) {
        if let Some(pending) = self.reminder_wake.take() {
            pending.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(plan::wait(next, now), move || {
            let Some(app) = weak.upgrade() else { return };
            // This source has run, so there is nothing left to remove.
            app.reminder_wake.take();
            app.check_event_reminders();
        });
        self.reminder_wake.replace(Some(source));
    }

    fn post_reminder(&self, due: &Due, now: EpochMillis) {
        let shown = notice::notice(due, now, &chrono::Local);
        let notification = gio::Notification::new(&shown.title);
        notification.set_body(Some(&shown.body));
        notification.set_priority(gio::NotificationPriority::High);
        notification.set_icon(&gio::ThemedIcon::new(crate::APP_ID));
        let target = shown.target.to_variant();
        notification.set_default_action_and_target_value(
            &format!("app.{}", notice::SHOW_EVENT),
            Some(&target),
        );
        for button in &shown.buttons {
            notification.add_button_with_target_value(
                &button.label(),
                &format!("app.{}", button.action()),
                Some(&target),
            );
        }
        self.gio.send_notification(Some(&shown.id), &notification);
        tracing::info!(event = %due.key.event, minutes = due.key.minutes, "posted an event reminder");
    }

    fn install_reminder_actions(self: &Rc<Self>) {
        let add = |name: &str, run: fn(&Rc<App>, Key)| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, parameter| {
                let key = parameter
                    .and_then(|p| p.get::<String>())
                    .and_then(|target| notice::key_of(&target));
                if let (Some(app), Some(key)) = (weak.upgrade(), key) {
                    run(&app, key);
                }
            });
            self.gio.add_action(&action);
        };
        add(notice::SHOW_EVENT, |app, key| {
            app.show_window()
                .show_event(key.account_id, &key.calendar, &key.event, key.start);
        });
        add(notice::JOIN_EVENT, |app, key| app.join_event(key));
        add(notice::SNOOZE_REMINDER, |app, key| app.snooze_reminder(key));
    }

    /// Opens the event's call. The link comes from the stored event, not
    /// from the notification, so nothing else on the session bus can use
    /// this action to open an address of its choosing.
    fn join_event(self: &Rc<Self>, key: Key) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = this
                .core
                .read(move |c| calendar::event(c, key.account_id, &key.calendar, &key.event))
                .await;
            let Some(link) = found
                .ok()
                .flatten()
                .and_then(|event| event.conference)
                .filter(|link| notice::joinable(link))
            else {
                return;
            };
            if let Err(err) = gio::AppInfo::launch_default_for_uri(&link, None::<&gio::AppLaunchContext>) {
                tracing::warn!(error = %err, "could not open the call");
            }
        });
    }

    fn snooze_reminder(self: &Rc<Self>, key: Key) {
        // Some notification servers keep a notification after a button
        // is pressed; this one comes back in five minutes instead.
        self.gio.withdraw_notification(&plan::notification_id(&key));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let now = now_millis();
            if let Err(err) = this
                .core
                .write(move |c| shown_log::snooze(c, &key, now, now + SNOOZE))
                .await
            {
                tracing::warn!(error = %err, "could not snooze the reminder");
            }
            this.check_event_reminders();
        });
    }
}
