//! The ways between mail and the calendar that live in the window: the
//! next event at the foot of the mail sidebar, read through the calendar
//! run's rule ([`NextCard`]), and the click that opens it in the calendar.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::EpochMillis;
use mailrs_store::calendar::CalendarScope;
use mailrs_sync::now_millis;

use super::MainWindow;
use crate::settings::Space;
use crate::ui::calendar::next::{self, NextUp};
use crate::ui::calendar::run::{Answer, Card, CardRead, NextCard, Work};

/// The window behind the next-event card's port. It holds the window
/// weakly, since the window owns the card's reads.
pub(super) struct NextPort(pub(super) Weak<MainWindow>);

impl Card for NextPort {
    fn now(&self) -> EpochMillis {
        now_millis()
    }

    fn read(&self, from: EpochMillis, to: EpochMillis) -> Answer<'_, Result<CardRead, String>> {
        let Some(win) = self.0.upgrade() else {
            return Box::pin(async { Err("the window closed".to_string()) });
        };
        let (core, copy) = (Rc::clone(&win.core), win.core.calendar_copy());
        Box::pin(async move {
            core.call(async move {
                let accounts = copy.every_account().await?;
                let found = copy.occurrences(&accounts, from, to, CalendarScope::Shown).await?;
                let mut colours = HashMap::new();
                for listed in copy.listed(&accounts).await? {
                    for calendar in listed.calendars {
                        colours.insert((listed.account_id, calendar.id), calendar.color);
                    }
                }
                Ok::<_, mailrs_sync::SyncError>((found, colours))
            })
            .await
            .map_err(|err| err.to_string())
        })
    }

    fn show(&self, next: Option<(NextUp, String)>) {
        let Some(win) = self.0.upgrade() else { return };
        match next {
            Some((up, colour)) => {
                let words = next::words(&up, now_millis(), &chrono::Local);
                win.sidebar.show_next(&words, &colour);
                *win.next_up.borrow_mut() = Some(up);
            }
            None => {
                win.sidebar.hide_next();
                *win.next_up.borrow_mut() = None;
            }
        }
    }

    fn spawn(&self, work: Work) {
        glib::spawn_future_local(work);
    }
}

/// The card's reads, for the window to hold.
pub(super) fn next_card(window: Weak<MainWindow>) -> Rc<NextCard> {
    Rc::new(NextCard::new(Rc::new(NextPort(window))))
}

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
        self.next_card.refresh();
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
