//! The two spaces beside the sidebar, mail and the calendar: switching
//! between them, the calendar's own keys, the Show Declined Events
//! entry, and what the window does when the calendar copy changes.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};
use mailrs_store::calendar as store;
use mailrs_sync::calendar_copy::{Refreshed, TurnedDown};

use super::shortcuts::{CalendarKey, Route, calendar_key, route};
use super::{MainWindow, toast_title};
use crate::settings::{Change, Space};
use crate::ui::calendar::range::ViewKind;

/// What a toast says about a change the provider turned down.
fn turned_down_words(turned_down: &TurnedDown) -> String {
    match &turned_down.reason {
        None => fill(
            &gettext("Your change to “{title}” was not saved because it changed elsewhere"),
            &[("title", &turned_down.title)],
        ),
        Some(reason) => fill(
            &gettext("Your change to “{title}” was not saved: {reason}"),
            &[("title", &turned_down.title), ("reason", reason)],
        ),
    }
}

impl MainWindow {
    /// The space on screen.
    pub(super) fn space(&self) -> Space {
        match self.spaces.visible_child_name().as_deref() {
            Some("calendar") => Space::Calendar,
            _ => Space::Mail,
        }
    }

    /// Whether a main window action runs while the space on screen shows,
    /// for the actions installed outside [`super::shortcuts::MAIN_ACTIONS`].
    pub(super) fn runs_here(&self, name: &str) -> bool {
        route(self.space(), name) == Route::Run
    }

    /// Shows `space` through the switch, which then shows it and
    /// remembers it. The calendar stays away while the switch is hidden.
    pub(super) fn show_space(self: &Rc<Self>, space: Space) {
        if space == Space::Calendar && !self.sidebar.switch.is_visible() {
            return;
        }
        let name = match space {
            Space::Mail => "mail",
            Space::Calendar => "calendar",
        };
        self.sidebar.switch.set_active_name(Some(name));
    }

    /// Shows what the switch picked and remembers it for the next start.
    fn space_picked(self: &Rc<Self>) {
        let space = match self.sidebar.switch.active_name().as_deref() {
            Some("calendar") => Space::Calendar,
            _ => Space::Mail,
        };
        if space == self.space() {
            return;
        }
        match space {
            Space::Mail => {
                self.spaces.set_visible_child_name("mail");
                self.sidebar.show_mail();
            }
            Space::Calendar => {
                self.spaces.set_visible_child_name("calendar");
                self.sidebar
                    .show_calendar(self.calendar.sidebar.upcast_ref());
                // The calendar's letters answer only inside its page, and
                // the list that had the focus is gone.
                self.calendar.take_focus();
            }
        }
        if let Some(action) = self
            .actions
            .lookup_action("show-declined-events")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(space == Space::Calendar);
        }
        if let Some(app) = self.app.upgrade()
            && app.settings_with(|s| s.space) != space
        {
            app.change_settings(Change::Space(space));
        }
    }

    /// Connects the switch, the calendar's keys and the declined events
    /// entry.
    pub(super) fn install_spaces(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.sidebar.switch.connect_active_name_notify(move |_| {
            if let Some(win) = weak.upgrade() {
                win.space_picked();
            }
        });

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
            Some(win) => win.calendar_key_pressed(key, modifiers),
            None => glib::Propagation::Proceed,
        });
        self.calendar.page.add_controller(keys);

        let on = self.settings().show_declined_events;
        let declined =
            gio::SimpleAction::new_stateful("show-declined-events", None, &on.to_variant());
        let weak = Rc::downgrade(self);
        declined.connect_change_state(move |action, value| {
            let (Some(win), Some(on)) = (weak.upgrade(), value.and_then(|v| v.get::<bool>()))
            else {
                return;
            };
            action.set_state(&on.to_variant());
            if let Some(app) = win.app.upgrade() {
                app.change_settings(Change::ShowDeclinedEvents(on));
            }
            win.calendar.reload();
        });
        declined.set_enabled(false);
        self.actions.add_action(&declined);
    }

    /// A key on the calendar page. Letters give way while a field has the
    /// focus, as mail's do.
    fn calendar_key_pressed(
        self: &Rc<Self>,
        key: gdk::Key,
        modifiers: gdk::ModifierType,
    ) -> glib::Propagation {
        if self.typing() || self.space() != Space::Calendar {
            return glib::Propagation::Proceed;
        }
        let Some(command) = calendar_key(key, modifiers) else {
            return glib::Propagation::Proceed;
        };
        let calendar = &self.calendar;
        match command {
            CalendarKey::Today => calendar.today(),
            CalendarKey::Day => calendar.set_kind(ViewKind::Day),
            CalendarKey::Week => calendar.set_kind(ViewKind::Week),
            CalendarKey::Month => calendar.set_kind(ViewKind::Month),
            CalendarKey::Previous => calendar.step(-1),
            CalendarKey::Next => calendar.step(1),
            CalendarKey::Search => calendar.focus_search(),
        }
        glib::Propagation::Stop
    }

    /// Hands the calendar the accounts just read, and shows or hides the
    /// switch: it shows once a running account offers a calendar (ruling
    /// R9). The first read also goes back to the space shown last.
    pub(super) fn accounts_for_calendar(self: &Rc<Self>, accounts: &[Account]) {
        let mut started = Vec::new();
        let mut waiting = false;
        for account in accounts {
            match self.core.account(account.id) {
                Some(running) => started.push(running.services().offers()),
                None => waiting = true,
            }
        }
        let remembered = self.settings().space;
        let visible = crate::offered::shows_space_switch(&started, waiting, remembered);
        self.sidebar.set_switch_visible(visible);
        if !visible && self.space() == Space::Calendar {
            self.show_space(Space::Mail);
        }
        if visible && !self.space_restored.get() {
            self.space_restored.set(true);
            self.show_space(remembered);
        }
        let calendar_accounts = accounts
            .iter()
            .map(|account| {
                (
                    account.clone(),
                    self.offers(account.id),
                    self.withheld(account.id),
                )
            })
            .collect();
        self.calendar.set_accounts(calendar_accounts);
    }

    /// What the window does after the calendar copy read the provider:
    /// the view reads the copy again when something changed, each change
    /// the provider turned down gets a toast, and an account that wants a
    /// permission redraws its row in the calendar's sidebar.
    pub fn calendar_refreshed(self: &Rc<Self>, refreshed: &Refreshed) {
        if refreshed.events > 0
            || !refreshed.turned_down.is_empty()
            || !refreshed.needs_permission.is_empty()
        {
            self.calendar.reload();
        }
        for turned_down in &refreshed.turned_down {
            self.say_turned_down(turned_down.clone());
        }
    }

    /// Toasts a change the provider turned down, with Show Event while
    /// the copy still holds the event; the provider may have deleted it.
    fn say_turned_down(self: &Rc<Self>, turned_down: TurnedDown) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (account_id, calendar, id) = (
                turned_down.account_id,
                turned_down.calendar.clone(),
                turned_down.event.clone(),
            );
            let still_there = this
                .core
                .read(move |c| store::event(c, account_id, &calendar, &id))
                .await
                .is_ok_and(|event| event.is_some());
            let toast = adw::Toast::builder()
                .title(toast_title(&turned_down_words(&turned_down)))
                .timeout(8)
                .build();
            if still_there {
                toast.set_button_label(Some(&gettext("Show Event")));
                let weak = Rc::downgrade(&this);
                toast.connect_button_clicked(move |_| {
                    let Some(win) = weak.upgrade() else { return };
                    win.show_space(Space::Calendar);
                    win.calendar.open(
                        turned_down.account_id,
                        &turned_down.calendar,
                        &turned_down.event,
                    );
                });
            }
            this.toasts.add_toast(toast);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turned_down(reason: Option<&str>) -> TurnedDown {
        TurnedDown {
            account_id: 1,
            calendar: "primary".into(),
            event: "e1".into(),
            title: "Dentist".into(),
            reason: reason.map(str::to_string),
        }
    }

    #[test]
    fn a_change_made_elsewhere_names_the_event() {
        assert_eq!(
            turned_down_words(&turned_down(None)),
            "Your change to “Dentist” was not saved because it changed elsewhere"
        );
    }

    #[test]
    fn a_refused_change_names_the_reason() {
        assert_eq!(
            turned_down_words(&turned_down(Some("the calendar is read-only"))),
            "Your change to “Dentist” was not saved: the calendar is read-only"
        );
    }
}
