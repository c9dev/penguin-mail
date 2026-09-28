//! The ways between mail and the calendar that live in the window: the
//! next event at the foot of the mail sidebar, and (Task B3) the
//! invitations waiting for an answer at the foot of the calendar
//! sidebar. Both read the calendar's copy from the store and draw what
//! the plain modules in `ui::calendar` decide.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::AccountId;
use mailrs_store::calendar::CalendarScope;
use mailrs_sync::now_millis;

use super::MainWindow;
use crate::settings::Space;
use crate::ui::calendar::next;

impl MainWindow {
    /// Starts the next-event card: read now, then each minute, and again
    /// whenever the calendar's copy changes.
    pub(super) fn install_next_event(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.sidebar.next.button.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.open_next_event();
            }
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(60, move || {
            let Some(win) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            win.refresh_next_event();
            glib::ControlFlow::Continue
        });
        self.refresh_next_event();
    }

    /// Reads the shown calendars of every account for the next event and
    /// puts it on the card, or takes the card away.
    pub fn refresh_next_event(self: &Rc<Self>) {
        let now = now_millis();
        let Some(day_ends) = crate::format::local(now)
            .and_then(|today| today.date_naive().succ_opt())
            .and_then(|day| day.and_hms_opt(0, 0, 0))
            .and_then(|midnight| midnight.and_local_timezone(chrono::Local).earliest())
            .map(|midnight| midnight.timestamp_millis())
        else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let read = this
                .core
                .read(move |c| {
                    let accounts: Vec<AccountId> = mailrs_store::accounts::list_accounts(c)?
                        .into_iter()
                        .map(|a| a.id)
                        .collect();
                    // What overlaps the next three hours: the events under
                    // way and the ones about to start.
                    let found = mailrs_store::calendar::occurrences(
                        c,
                        &accounts,
                        now,
                        now + next::AHEAD,
                        CalendarScope::Shown,
                    )?;
                    let mut colours = HashMap::new();
                    for &account_id in &accounts {
                        for calendar in mailrs_store::calendar::calendars(c, account_id)? {
                            colours.insert((account_id, calendar.id), calendar.color);
                        }
                    }
                    Ok((found, colours))
                })
                .await;
            let (found, colours) = match read {
                Ok(read) => read,
                Err(err) => {
                    tracing::info!(%err, "could not read the next event");
                    return;
                }
            };
            match next::next_up(&found, now, day_ends) {
                Some(up) => {
                    let words = next::words(&up, now, &chrono::Local);
                    this.sidebar.show_next(&words, &next::colour(up.occurrence(), &colours));
                    *this.next_up.borrow_mut() = Some(up);
                }
                None => {
                    this.sidebar.hide_next();
                    *this.next_up.borrow_mut() = None;
                }
            }
        });
    }

    /// Switches to the calendar on the next event, with its popover open.
    fn open_next_event(self: &Rc<Self>) {
        let Some(up) = self.next_up.borrow().clone() else {
            return;
        };
        let o = up.occurrence();
        self.show_space(Space::Calendar);
        self.calendar.open(o.account_id, &o.event.calendar, &o.event.id, o.start);
    }
}
