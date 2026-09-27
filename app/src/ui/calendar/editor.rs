//! The event editor: one column of fields in the spec's order, a More row
//! for the rarer ones, and a page for a custom repeat. It shows a `Draft`
//! and changes it through the draft's setters; the rules live there.
//!
//! Every handler below closes over `Rc::downgrade(&editor)`, never
//! `Rc::clone`. The dialog's own widget tree holds the handlers, and a
//! handler holding a strong `Rc<Editor>` back into a tree the `Editor`
//! itself owns (through `dialog` and `nav`) would keep both alive
//! forever, zone list and all. The one strong reference that keeps the
//! editor alive for as long as the dialog is open lives in a closure on
//! the dialog's own `closed` signal, which nothing in `Editor` points
//! back to.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, NaiveDate, NaiveTime, Timelike, Weekday};
use chrono_tz::{TZ_VARIANTS, Tz};
use gtk::glib;
use mailrs_domain::calendar::repeat::{Custom, Ends, Frequency, Repeat};
use mailrs_domain::calendar::{self, Calendar, EVENT_COLORS, Guest, Reminder, ReminderMethod};
use mailrs_domain::translate::{date_locale, fill, gettext, ngettext};
use mailrs_domain::{AccountId, EpochMillis};

use super::draft::{self, Draft};
use super::layout;
use super::tint;
use super::words::{self, REMINDER_CHOICES};
use crate::compose::{is_address, parse_recipients};
use crate::ui;
use crate::ui::autocomplete::{self, Contacts};

/// The calendars a new event may go on, and every calendar by key.
pub struct Choices {
    pub writable: Vec<(AccountId, String, Calendar)>,
    pub calendars: HashMap<(AccountId, String), Calendar>,
}

struct Editor {
    dialog: adw::Dialog,
    nav: adw::NavigationView,
    draft: RefCell<Draft>,
    save: gtk::Button,
    view_zone: Tz,
    /// The end row's own date button and its popover calendar, set again
    /// from the draft whenever the start moves it (Step 3).
    end_date: RefCell<Option<(gtk::MenuButton, gtk::Calendar)>>,
    end_time: RefCell<Option<gtk::DropDown>>,
    /// Set while a handler is writing a widget from the draft rather than
    /// the other way round, so that write does not read back as a change.
    quiet: Cell<bool>,
    /// Once the person has picked a time zone of their own, a calendar
    /// change stops moving it for them.
    zone_touched: Cell<bool>,
    title_row: RefCell<Option<adw::EntryRow>>,
    repeat_row: RefCell<Option<adw::ComboRow>>,
    /// The Repeats row's selected index the draft actually holds, put
    /// back when the Custom page is left without pressing Done.
    repeat_confirmed: Cell<u32>,
    repeat_quiet: Cell<bool>,
    guests_group: RefCell<Option<adw::PreferencesGroup>>,
    guest_rows: RefCell<Vec<adw::ActionRow>>,
    reminders_group: RefCell<Option<adw::PreferencesGroup>>,
    /// The calendar's own reminders, shown while `draft.reminders` is
    /// `None`. Follows the calendar row on a new event.
    reminders_default: RefCell<Vec<Reminder>>,
    reminder_rows: RefCell<Vec<adw::ComboRow>>,
    /// The Custom Repeat page's content, rebuilt each time the Repeats
    /// row asks for it rather than kept live, so a start moved after the
    /// page was last open still seeds the right default weekday.
    custom_container: gtk::Box,
    custom_state: RefCell<Custom>,
}

/// Shows the editor over `parent`. `on_save` gets the draft when the
/// person presses Save.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    draft: Draft,
    choices: Choices,
    contacts: Contacts,
    on_save: impl Fn(Draft) + 'static,
) {
    let title = if draft.is_new() {
        gettext("New Event")
    } else {
        gettext("Edit Event")
    };
    let save = gtk::Button::builder()
        .label(gettext("Save"))
        .css_classes(["suggested-action"])
        .sensitive(draft.can_save())
        .build();
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let nav = adw::NavigationView::new();
    let dialog = adw::Dialog::builder()
        .title(&title)
        .content_width(480)
        .content_height(720)
        .child(&nav)
        .build();
    let view_zone = draft::local_zone();
    let repeat_confirmed = repeat_index(&draft.repeat);

    let editor = Rc::new(Editor {
        dialog: dialog.clone(),
        nav: nav.clone(),
        draft: RefCell::new(draft),
        save: save.clone(),
        view_zone,
        end_date: RefCell::new(None),
        end_time: RefCell::new(None),
        quiet: Cell::new(false),
        zone_touched: Cell::new(false),
        title_row: RefCell::new(None),
        repeat_row: RefCell::new(None),
        repeat_confirmed: Cell::new(repeat_confirmed),
        repeat_quiet: Cell::new(false),
        guests_group: RefCell::new(None),
        guest_rows: RefCell::new(Vec::new()),
        reminders_group: RefCell::new(None),
        reminders_default: RefCell::new(Vec::new()),
        reminder_rows: RefCell::new(Vec::new()),
        custom_container: gtk::Box::new(gtk::Orientation::Vertical, 0),
        custom_state: RefCell::new(Custom {
            every: 1,
            frequency: Frequency::Weekly,
            days: Vec::new(),
            ends: Ends::Never,
        }),
    });

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&editor.form(&choices, contacts)));
    let form_page = adw::NavigationPage::builder()
        .title(&title)
        .tag("form")
        .child(&toolbar)
        .build();
    nav.add(&form_page);
    // The Title field takes the focus each time the form shows, so Enter
    // saves rather than pressing Cancel, the first button in the header.
    // A guest's Title is insensitive and cannot take it; the focus then
    // stays where the dialog puts it.
    let weak = Rc::downgrade(&editor);
    form_page.connect_shown(move |_| {
        if let Some(this) = weak.upgrade()
            && let Some(row) = this.title_row.borrow().as_ref()
        {
            row.grab_focus();
        }
    });

    // The Custom Repeat page is built once, here, and kept in the
    // NavigationView for the dialog's whole life (`nav.add`, never
    // `nav.remove`): a page only ever pushed loses its parent once
    // popped, and an accessibility walk still holding it then crashes
    // the app (libadwaita-dialog-traps). `refresh_custom_page` rebuilds
    // its contents from the draft each time it is about to show.
    let done = gtk::Button::builder()
        .label(gettext("Done"))
        .css_classes(["suggested-action"])
        .build();
    let custom_header = adw::HeaderBar::new();
    custom_header.pack_end(&done);
    let custom_toolbar = adw::ToolbarView::new();
    custom_toolbar.add_top_bar(&custom_header);
    custom_toolbar.set_content(Some(&editor.custom_container));
    let custom_page = adw::NavigationPage::builder()
        .title(gettext("Custom Repeat"))
        .tag("custom")
        .child(&custom_toolbar)
        .build();
    nav.add(&custom_page);

    let weak = Rc::downgrade(&editor);
    done.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.commit_custom();
        }
    });
    let weak = Rc::downgrade(&editor);
    nav.connect_popped(move |_, page| {
        if page.tag().as_deref() == Some("custom")
            && let Some(this) = weak.upgrade()
        {
            this.revert_repeat_row();
        }
    });

    let weak = Rc::downgrade(&editor);
    cancel.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.dialog.close();
        }
    });
    let weak = Rc::downgrade(&editor);
    save.connect_clicked(move |_| {
        let Some(this) = weak.upgrade() else { return };
        let draft = this.draft.borrow().clone();
        if draft.can_save() {
            this.dialog.close();
            on_save(draft);
        }
    });
    // The one strong reference. Dropping it here, rather than in any
    // widget's own handler, is what lets the whole tree free once the
    // dialog closes.
    let held = RefCell::new(Some(Rc::clone(&editor)));
    dialog.connect_closed(move |_| {
        held.borrow_mut().take();
    });

    dialog.set_default_widget(Some(&save));
    dialog.present(Some(parent));
}

impl Editor {
    fn refresh_save(&self) {
        self.save.set_sensitive(self.draft.borrow().can_save());
    }

    /// The form page's content, in the spec's order.
    fn form(self: &Rc<Self>, choices: &Choices, contacts: Contacts) -> adw::PreferencesPage {
        let limited = self
            .draft
            .borrow()
            .base
            .as_ref()
            .is_some_and(draft::limited);
        let page = adw::PreferencesPage::new();
        let title_group = self.title_group();
        let when_group = self.when_group();
        let repeat_group = self.repeat_group();
        page.add(&title_group);
        page.add(&when_group);
        page.add(&repeat_group);
        page.add(&self.calendar_group(choices));
        let place_group = self.place_group();
        page.add(&place_group);
        page.add(&self.guests_group_widget(contacts));
        page.add(&self.reminders_group_widget(choices));
        let notes_group = self.notes_group();
        page.add(&notes_group);
        page.add(&self.more_group());
        if limited {
            let organizer = self.organizer_words();
            title_group.set_sensitive(false);
            when_group.set_sensitive(false);
            when_group.set_description(Some(&fill(
                &gettext("{organizer} organizes this event, so its time, place and guests stay as they set them."),
                &[("organizer", &organizer)],
            )));
            repeat_group.set_sensitive(false);
            place_group.set_sensitive(false);
            notes_group.set_sensitive(false);
            if let Some(group) = self.guests_group.borrow().as_ref() {
                group.set_sensitive(false);
            }
        }
        page
    }

    /// The name or address of whoever organizes a limited event, for the
    /// When group's description.
    fn organizer_words(&self) -> String {
        let draft = self.draft.borrow();
        let Some(base) = &draft.base else {
            return String::new();
        };
        base.guests
            .iter()
            .find(|g| g.organizer)
            .map(|g| g.name.clone().unwrap_or_else(|| g.email.clone()))
            .or_else(|| base.organizer.clone())
            .unwrap_or_default()
    }

    // ---- Title ----

    fn title_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let row = adw::EntryRow::builder().title(gettext("Title")).build();
        row.set_text(&self.draft.borrow().title);
        let weak = Rc::downgrade(self);
        row.connect_changed(move |row| {
            let Some(this) = weak.upgrade() else { return };
            this.draft.borrow_mut().title = row.text().to_string();
            this.refresh_save();
        });
        let weak = Rc::downgrade(self);
        row.connect_entry_activated(move |_| {
            let Some(this) = weak.upgrade() else { return };
            if this.draft.borrow().can_save() {
                this.save.emit_clicked();
            }
        });
        group.add(&row);
        self.title_row.replace(Some(row));
        group
    }

    // ---- When ----

    fn when_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let all_day = adw::SwitchRow::builder()
            .title(gettext("All day"))
            .active(self.draft.borrow().all_day)
            .build();

        let (start, end) = {
            let d = self.draft.borrow();
            (d.start, d.end)
        };
        let start_zone: Tz = self.draft.borrow().zone.parse().unwrap_or(self.view_zone);
        let start_day = local_day(start, start_zone);
        let start_time = local_time(start, start_zone);
        let end_day = local_day(end, start_zone);
        let end_time_value = local_time(end, start_zone);

        let start_date = date_button(start_day, &gettext("Start date"), {
            let weak = Rc::downgrade(self);
            move |day| {
                if let Some(this) = weak.upgrade() {
                    this.set_start_date(day);
                }
            }
        });
        let start_time_drop = time_dropdown(start_time);
        ui::name(&start_time_drop, &gettext("Start time"));
        let weak = Rc::downgrade(self);
        start_time_drop.connect_selected_notify(move |drop| {
            let Some(this) = weak.upgrade() else { return };
            if this.quiet.get() {
                return;
            }
            let Some(time) = selected_time(drop) else {
                return;
            };
            this.set_start_time(time);
        });

        let end_date_calendar = gtk::Calendar::new();
        end_date_calendar.set_date(&day_to_glib(end_day));
        let end_date_button = gtk::MenuButton::builder()
            .label(format_date(end_day))
            .popover(&gtk::Popover::builder().child(&end_date_calendar).build())
            .valign(gtk::Align::Center)
            .build();
        ui::name(&end_date_button, &gettext("End date"));
        let weak = Rc::downgrade(self);
        let shown = end_date_button.clone();
        end_date_calendar.connect_day_selected(move |calendar| {
            let Some(this) = weak.upgrade() else { return };
            if this.quiet.get() {
                return;
            }
            let picked = calendar.date();
            if let Some(day) = NaiveDate::from_ymd_opt(
                picked.year(),
                picked.month() as u32,
                picked.day_of_month() as u32,
            ) {
                shown.set_label(&format_date(day));
                shown.popdown();
                this.set_end_date(day);
            }
        });
        *self.end_date.borrow_mut() = Some((end_date_button.clone(), end_date_calendar));

        let end_time_drop = time_dropdown(end_time_value);
        ui::name(&end_time_drop, &gettext("End time"));
        let weak = Rc::downgrade(self);
        end_time_drop.connect_selected_notify(move |drop| {
            let Some(this) = weak.upgrade() else { return };
            if this.quiet.get() {
                return;
            }
            let Some(time) = selected_time(drop) else {
                return;
            };
            this.set_end_time(time);
        });
        *self.end_time.borrow_mut() = Some(end_time_drop.clone());

        let starts = adw::ActionRow::builder().title(gettext("Starts")).build();
        starts.add_suffix(&start_date);
        starts.add_suffix(&start_time_drop);
        let ends = adw::ActionRow::builder().title(gettext("Ends")).build();
        ends.add_suffix(&end_date_button);
        ends.add_suffix(&end_time_drop);

        let weak = Rc::downgrade(self);
        let (t1, t2) = (start_time_drop.clone(), end_time_drop.clone());
        all_day.connect_active_notify(move |row| {
            let Some(this) = weak.upgrade() else { return };
            this.draft.borrow_mut().set_all_day(row.is_active());
            t1.set_visible(!row.is_active());
            t2.set_visible(!row.is_active());
            this.show_end();
        });
        start_time_drop.set_visible(!all_day.is_active());
        end_time_drop.set_visible(!all_day.is_active());

        group.add(&all_day);
        group.add(&starts);
        group.add(&ends);
        group
    }

    fn set_start_date(self: &Rc<Self>, day: NaiveDate) {
        let zone: Tz = self.draft.borrow().zone.parse().unwrap_or(self.view_zone);
        let time = {
            let d = self.draft.borrow();
            local_time(d.start, zone)
        };
        let at = layout::instant_at(day, time_to_hours(time), &zone);
        self.draft.borrow_mut().set_start(at);
        self.show_end();
        self.refresh_save();
    }

    fn set_start_time(self: &Rc<Self>, time: NaiveTime) {
        let zone: Tz = self.draft.borrow().zone.parse().unwrap_or(self.view_zone);
        let day = {
            let d = self.draft.borrow();
            local_day(d.start, zone)
        };
        let at = layout::instant_at(day, time_to_hours(time), &zone);
        self.draft.borrow_mut().set_start(at);
        self.show_end();
    }

    fn set_end_date(self: &Rc<Self>, day: NaiveDate) {
        let zone: Tz = self.draft.borrow().zone.parse().unwrap_or(self.view_zone);
        let time = {
            let d = self.draft.borrow();
            local_time(d.end, zone)
        };
        let at = layout::instant_at(day, time_to_hours(time), &zone);
        self.draft.borrow_mut().set_end(at);
    }

    fn set_end_time(self: &Rc<Self>, time: NaiveTime) {
        let zone: Tz = self.draft.borrow().zone.parse().unwrap_or(self.view_zone);
        let day = {
            let d = self.draft.borrow();
            local_day(d.end, zone)
        };
        let at = layout::instant_at(day, time_to_hours(time), &zone);
        self.draft.borrow_mut().set_end(at);
    }

    /// Sets the end row's widgets from the draft without their own
    /// handlers reading the change back in.
    fn show_end(&self) {
        self.quiet.set(true);
        let zone: Tz = self.draft.borrow().zone.parse().unwrap_or(self.view_zone);
        let end = self.draft.borrow().end;
        let day = local_day(end, zone);
        let time = local_time(end, zone);
        if let Some((button, calendar)) = self.end_date.borrow().as_ref() {
            button.set_label(&format_date(day));
            calendar.set_date(&day_to_glib(day));
        }
        if let Some(drop) = self.end_time.borrow().as_ref() {
            select_time(drop, time);
        }
        self.quiet.set(false);
    }

    // ---- Repeats ----

    fn repeat_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let row = adw::ComboRow::builder().title(gettext("Repeats")).build();
        crate::ui::name_combo_row_items(&row);
        let names: Vec<String> = REPEAT_PRESETS
            .iter()
            .map(words::repeat_words)
            .chain([gettext("Custom…")])
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        row.set_model(Some(&gtk::StringList::new(&refs)));
        let repeat = self.draft.borrow().repeat.clone();
        row.set_selected(self.repeat_confirmed.get());
        if matches!(repeat, Repeat::Custom(_) | Repeat::Kept(_)) {
            row.set_subtitle(&words::repeat_words(&repeat));
        }
        let weak = Rc::downgrade(self);
        row.connect_selected_notify(move |row| {
            let Some(this) = weak.upgrade() else { return };
            if this.repeat_quiet.get() {
                return;
            }
            let index = row.selected();
            if index as usize == REPEAT_PRESETS.len() {
                this.open_custom_page();
                return;
            }
            let preset = REPEAT_PRESETS[index as usize].clone();
            this.draft.borrow_mut().repeat = preset;
            row.set_subtitle("");
            this.repeat_confirmed.set(index);
        });
        *self.repeat_row.borrow_mut() = Some(row.clone());
        group.add(&row);
        group
    }

    fn open_custom_page(self: &Rc<Self>) {
        let start_day = {
            let draft = self.draft.borrow();
            let zone: Tz = draft.zone.parse().unwrap_or(self.view_zone);
            local_day(draft.start, zone)
        };
        let seed = match &self.draft.borrow().repeat {
            Repeat::Custom(custom) => custom.clone(),
            _ => Custom {
                every: 1,
                frequency: Frequency::Weekly,
                days: vec![start_day.weekday()],
                ends: Ends::Never,
            },
        };
        *self.custom_state.borrow_mut() = seed;
        self.refresh_custom_page();
        self.nav.push_by_tag("custom");
    }

    /// Rebuilds the Custom Repeat page's fields from `custom_state`.
    fn refresh_custom_page(self: &Rc<Self>) {
        while let Some(child) = self.custom_container.first_child() {
            self.custom_container.remove(&child);
        }
        let page = adw::PreferencesPage::new();
        let main = adw::PreferencesGroup::new();

        let start_day = {
            let draft = self.draft.borrow();
            let zone: Tz = draft.zone.parse().unwrap_or(self.view_zone);
            local_day(draft.start, zone)
        };
        let state = self.custom_state.borrow().clone();
        let every = adw::SpinRow::with_range(1.0, 99.0, 1.0);
        every.set_title(&gettext("Every"));
        every.set_value(f64::from(state.every));
        let unit = adw::ComboRow::new();
        crate::ui::name_combo_row_items(&unit);
        unit.set_title(&gettext("Unit"));
        unit.set_model(Some(&unit_model(state.every)));
        unit.set_selected(frequency_index(state.frequency));

        let weekdays = adw::ActionRow::builder().title(gettext("On")).build();
        let days_box = gtk::Box::builder()
            .spacing(4)
            .css_classes(["linked"])
            .valign(gtk::Align::Center)
            .build();
        for day in WEEK {
            let label = weekday_name(day);
            let toggle = gtk::ToggleButton::builder()
                .active(state.days.contains(&day))
                .build();
            toggle.set_child(Some(&gtk::Label::new(Some(&first_letter(&label)))));
            ui::name(&toggle, &label);
            let weak = Rc::downgrade(self);
            toggle.connect_toggled(move |toggle| {
                let Some(this) = weak.upgrade() else { return };
                let mut state = this.custom_state.borrow_mut();
                state.days.retain(|d| *d != day);
                if toggle.is_active() {
                    state.days.push(day);
                }
            });
            days_box.append(&toggle);
        }
        weekdays.add_suffix(&days_box);
        weekdays.set_visible(state.frequency == Frequency::Weekly);

        let weak = Rc::downgrade(self);
        let unit_widget = unit.clone();
        every.connect_changed(move |row| {
            let Some(this) = weak.upgrade() else { return };
            let n = row.value().round().clamp(1.0, 99.0) as u32;
            this.custom_state.borrow_mut().every = n;
            let selected = unit_widget.selected();
            unit_widget.set_model(Some(&unit_model(n)));
            unit_widget.set_selected(selected);
        });

        let weak = Rc::downgrade(self);
        let weekdays_row = weekdays.clone();
        unit.connect_selected_notify(move |row| {
            let Some(this) = weak.upgrade() else { return };
            let frequency = FREQUENCIES[row.selected() as usize];
            this.custom_state.borrow_mut().frequency = frequency;
            weekdays_row.set_visible(frequency == Frequency::Weekly);
        });

        main.add(&every);
        main.add(&unit);
        main.add(&weekdays);

        let stops = adw::PreferencesGroup::builder()
            .title(gettext("Stops"))
            .build();
        let never_check = gtk::CheckButton::new();
        let on_date_check = gtk::CheckButton::new();
        on_date_check.set_group(Some(&never_check));
        let after_check = gtk::CheckButton::new();
        after_check.set_group(Some(&never_check));
        ui::name(&never_check, &gettext("Never"));
        ui::name(&on_date_check, &gettext("On a date"));
        ui::name(&after_check, &gettext("After"));

        let never_row = adw::ActionRow::builder()
            .title(gettext("Never"))
            .activatable_widget(&never_check)
            .build();
        never_row.add_prefix(&never_check);

        let ends_day = match state.ends {
            Ends::On(day) => day,
            _ => start_day,
        };
        let end_date_button = date_button(ends_day, &gettext("End date of the repeat"), {
            let weak = Rc::downgrade(self);
            let on_date_check = on_date_check.clone();
            move |day| {
                let Some(this) = weak.upgrade() else { return };
                this.custom_state.borrow_mut().ends = Ends::On(day);
                on_date_check.set_active(true);
            }
        });
        let on_date_row = adw::ActionRow::builder()
            .title(gettext("On a date"))
            .activatable_widget(&on_date_check)
            .build();
        on_date_row.add_prefix(&on_date_check);
        on_date_row.add_suffix(&end_date_button);

        let times = if let Ends::After(n) = state.ends {
            n
        } else {
            1
        };
        let count = gtk::SpinButton::with_range(1.0, 999.0, 1.0);
        count.set_value(f64::from(times));
        let count_label = gtk::Label::new(Some(&ngettext("time", "times", times)));
        ui::name(&count, &gettext("Number of times"));
        let weak = Rc::downgrade(self);
        let (after_check_for_count, count_label_for_count) =
            (after_check.clone(), count_label.clone());
        count.connect_value_changed(move |spin| {
            let Some(this) = weak.upgrade() else { return };
            let n = spin.value().round().clamp(1.0, 999.0) as u32;
            this.custom_state.borrow_mut().ends = Ends::After(n);
            count_label_for_count.set_label(&ngettext("time", "times", n));
            after_check_for_count.set_active(true);
        });
        let after_row = adw::ActionRow::builder()
            .title(gettext("After"))
            .activatable_widget(&after_check)
            .build();
        after_row.add_prefix(&after_check);
        let after_suffix = gtk::Box::builder()
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        after_suffix.append(&count);
        after_suffix.append(&count_label);
        after_row.add_suffix(&after_suffix);

        for check in [&never_check, &on_date_check, &after_check] {
            let weak = Rc::downgrade(self);
            let kind = if std::ptr::eq(check, &never_check) {
                0
            } else if std::ptr::eq(check, &on_date_check) {
                1
            } else {
                2
            };
            check.connect_toggled(move |check| {
                if !check.is_active() {
                    return;
                }
                let Some(this) = weak.upgrade() else { return };
                let mut state = this.custom_state.borrow_mut();
                state.ends = match kind {
                    0 => Ends::Never,
                    1 => match state.ends {
                        Ends::On(day) => Ends::On(day),
                        _ => Ends::On(start_day),
                    },
                    _ => match state.ends {
                        Ends::After(n) => Ends::After(n),
                        _ => Ends::After(1),
                    },
                };
            });
        }
        match state.ends {
            Ends::Never => never_check.set_active(true),
            Ends::On(_) => on_date_check.set_active(true),
            Ends::After(_) => after_check.set_active(true),
        }

        stops.add(&never_row);
        stops.add(&on_date_row);
        stops.add(&after_row);

        page.add(&main);
        page.add(&stops);
        self.custom_container.append(&page);
    }

    fn commit_custom(self: &Rc<Self>) {
        let custom = self.custom_state.borrow().clone();
        let words = words::repeat_words(&Repeat::Custom(custom.clone()));
        self.draft.borrow_mut().repeat = Repeat::Custom(custom);
        if let Some(row) = self.repeat_row.borrow().as_ref() {
            self.repeat_quiet.set(true);
            row.set_selected(REPEAT_PRESETS.len() as u32);
            row.set_subtitle(&words);
            self.repeat_quiet.set(false);
        }
        self.repeat_confirmed.set(REPEAT_PRESETS.len() as u32);
        self.nav.pop();
    }

    /// Puts the Repeats row back on the choice it had, for a pop that was
    /// not Done: the back button or Escape.
    fn revert_repeat_row(&self) {
        if let Some(row) = self.repeat_row.borrow().as_ref() {
            self.repeat_quiet.set(true);
            row.set_selected(self.repeat_confirmed.get());
            self.repeat_quiet.set(false);
        }
    }

    // ---- Calendar ----

    fn calendar_group(self: &Rc<Self>, choices: &Choices) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let draft = self.draft.borrow();
        if draft.is_new() {
            let row = adw::ComboRow::builder().title(gettext("Calendar")).build();
            crate::ui::name_combo_row_items(&row);
            // The model holds each entry's key, and the factory finds the
            // entry from the item it is handed. The closed row is a list
            // item too, and its position is not the selected index, so a
            // lookup by position showed one calendar while the draft held
            // another (libadwaita-dialog-traps).
            let keys: Vec<String> = choices
                .writable
                .iter()
                .map(|(account, _, c)| calendar_key(*account, &c.id))
                .collect();
            let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            row.set_model(Some(&gtk::StringList::new(&refs)));
            let entries = Rc::new(choices.writable.clone());
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                item.set_child(Some(&calendar_item_widget()));
            });
            let bind_entries = Rc::clone(&entries);
            factory.connect_bind(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(key) = item.item().and_downcast::<gtk::StringObject>() else {
                    return;
                };
                let Some((_, address, calendar)) = calendar_entry(&bind_entries, &key.string())
                else {
                    return;
                };
                if let Some(child) = item.child() {
                    fill_calendar_item(&child, calendar, address);
                }
            });
            row.set_factory(Some(&factory));
            if let Some(i) = choices
                .writable
                .iter()
                .position(|(a, _, c)| *a == draft.account_id && c.id == draft.calendar)
            {
                row.set_selected(i as u32);
            }
            drop(draft);
            let weak = Rc::downgrade(self);
            row.connect_selected_notify(move |row| {
                let Some(this) = weak.upgrade() else { return };
                let Some((account_id, _, calendar)) = entries.get(row.selected() as usize) else {
                    return;
                };
                let calendar = calendar.clone();
                let account_id = *account_id;
                {
                    let mut draft = this.draft.borrow_mut();
                    draft.account_id = account_id;
                    draft.calendar = calendar.id.clone();
                    if !this.zone_touched.get() && calendar.zone.parse::<Tz>().is_ok() {
                        draft.zone = calendar.zone.clone();
                    }
                }
                *this.reminders_default.borrow_mut() = calendar.reminders.clone();
                if this.draft.borrow().reminders.is_none() {
                    this.rebuild_reminders();
                }
                this.refresh_save();
            });
            group.add(&row);
        } else {
            let name = choices
                .calendars
                .get(&(draft.account_id, draft.calendar.clone()))
                .map(|c| c.name.clone())
                .unwrap_or_default();
            let row = adw::ActionRow::builder()
                .title(gettext("Calendar"))
                .subtitle(name)
                .build();
            group.add(&row);
        }
        group
    }

    // ---- Place ----

    fn place_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let row = adw::EntryRow::builder().title(gettext("Place")).build();
        row.set_text(&self.draft.borrow().place);
        let weak = Rc::downgrade(self);
        row.connect_changed(move |row| {
            let Some(this) = weak.upgrade() else { return };
            this.draft.borrow_mut().place = row.text().to_string();
        });
        group.add(&row);
        group
    }

    // ---- Guests ----

    fn guests_group_widget(self: &Rc<Self>, contacts: Contacts) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Guests"))
            .build();
        let entry = gtk::Entry::builder()
            .placeholder_text(gettext("Add guests"))
            .build();
        ui::name(&entry, &gettext("Add guests"));
        autocomplete::attach(&entry, contacts);
        let weak = Rc::downgrade(self);
        entry.connect_activate(move |entry| {
            let Some(this) = weak.upgrade() else { return };
            let mut invalid = Vec::new();
            let mut added = false;
            for address in parse_recipients(&entry.text()) {
                if is_address(&address.email) {
                    let mut draft = this.draft.borrow_mut();
                    if !draft
                        .guests
                        .iter()
                        .any(|g| g.email.eq_ignore_ascii_case(&address.email))
                    {
                        draft.guests.push(Guest {
                            email: address.email.clone(),
                            name: address.name.clone(),
                            ..Guest::default()
                        });
                        added = true;
                    }
                } else {
                    invalid.push(address.email.clone());
                }
            }
            if invalid.is_empty() {
                entry.set_text("");
                entry.remove_css_class("error");
                entry.set_tooltip_text(None);
            } else {
                let text = invalid.join(", ");
                entry.set_text(&text);
                entry.add_css_class("error");
                entry.set_tooltip_text(Some(&fill(
                    &gettext("Not an address: {text}"),
                    &[("text", &text)],
                )));
            }
            if added {
                this.rebuild_guests();
                this.refresh_save();
            }
        });
        group.add(&entry);
        *self.guests_group.borrow_mut() = Some(group.clone());
        self.rebuild_guests();
        group
    }

    /// Rebuilds every guest row from `draft.guests`, replacing whatever
    /// rows were there.
    fn rebuild_guests(self: &Rc<Self>) {
        let Some(group) = self.guests_group.borrow().clone() else {
            return;
        };
        for row in self.guest_rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let guests = self.draft.borrow().guests.clone();
        let mut rows = Vec::new();
        for guest in &guests {
            let shown = guest.name.clone().unwrap_or_else(|| guest.email.clone());
            let subtitle = fill(
                &gettext("{address} · {answer}"),
                &[
                    ("address", &guest.email),
                    ("answer", &words::answer_words(guest)),
                ],
            );
            let row = adw::ActionRow::builder()
                .title(shown.clone())
                .subtitle(subtitle)
                .build();
            if !guest.organizer {
                let remove = gtk::Button::builder()
                    .icon_name("window-close-symbolic")
                    .css_classes(["flat"])
                    .valign(gtk::Align::Center)
                    .build();
                ui::name(
                    &remove,
                    &fill(&gettext("Remove {guest}"), &[("guest", &shown)]),
                );
                let weak = Rc::downgrade(self);
                let email = guest.email.clone();
                remove.connect_clicked(move |_| {
                    let Some(this) = weak.upgrade() else { return };
                    this.draft
                        .borrow_mut()
                        .guests
                        .retain(|g| !g.email.eq_ignore_ascii_case(&email));
                    this.rebuild_guests();
                });
                row.add_suffix(&remove);
            }
            group.add(&row);
            rows.push(row);
        }
        *self.guest_rows.borrow_mut() = rows;
    }

    // ---- Reminders ----

    fn reminders_group_widget(self: &Rc<Self>, choices: &Choices) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Reminders"))
            .build();
        let draft = self.draft.borrow();
        let calendar_default = choices
            .calendars
            .get(&(draft.account_id, draft.calendar.clone()))
            .map(|c| c.reminders.clone())
            .unwrap_or_default();
        drop(draft);
        *self.reminders_default.borrow_mut() = calendar_default;
        *self.reminders_group.borrow_mut() = Some(group.clone());
        self.rebuild_reminders();
        group
    }

    /// Rebuilds the reminders group's rows from `draft.reminders`, or the
    /// calendar's own default while the person has changed none of them.
    fn rebuild_reminders(self: &Rc<Self>) {
        let Some(group) = self.reminders_group.borrow().clone() else {
            return;
        };
        for row in self.reminder_rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let inherited = self.draft.borrow().reminders.is_none();
        group.set_description(
            inherited
                .then(|| gettext("The calendar's default"))
                .as_deref(),
        );
        let list = self
            .draft
            .borrow()
            .reminders
            .clone()
            .unwrap_or_else(|| self.reminders_default.borrow().clone());
        let mut rows = Vec::new();
        for (i, reminder) in list.iter().enumerate() {
            let row = self.reminder_row(i, *reminder);
            group.add(&row);
            rows.push(row);
        }
        let add = adw::ButtonRow::builder()
            .title(gettext("Add Reminder"))
            .start_icon_name("list-add-symbolic")
            .build();
        let weak = Rc::downgrade(self);
        add.connect_activated(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let mut list = this
                .draft
                .borrow()
                .reminders
                .clone()
                .unwrap_or_else(|| this.reminders_default.borrow().clone());
            list.push(Reminder {
                minutes: 10,
                method: ReminderMethod::Notification,
            });
            this.draft.borrow_mut().reminders = Some(list);
            this.rebuild_reminders();
        });
        group.add(&add);
        *self.reminder_rows.borrow_mut() = rows;
    }

    fn reminder_row(self: &Rc<Self>, index: usize, reminder: Reminder) -> adw::ComboRow {
        let row = adw::ComboRow::builder()
            .title(words::reminder_words(reminder.minutes))
            .build();
        crate::ui::name_combo_row_items(&row);
        let values = reminder_choice_values(reminder.minutes);
        let names: Vec<String> = values.iter().map(|m| words::reminder_words(*m)).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        row.set_model(Some(&gtk::StringList::new(&refs)));
        if let Some(i) = values.iter().position(|m| *m == reminder.minutes) {
            row.set_selected(i as u32);
        }
        if reminder.method == ReminderMethod::Email {
            row.set_subtitle(&gettext("By email"));
        }
        let remove = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        ui::name(&remove, &gettext("Remove reminder"));
        row.add_suffix(&remove);

        let weak = Rc::downgrade(self);
        let values_for_combo = values.clone();
        row.connect_selected_notify(move |row| {
            let Some(this) = weak.upgrade() else { return };
            let Some(minutes) = values_for_combo.get(row.selected() as usize) else {
                return;
            };
            let mut list = this
                .draft
                .borrow()
                .reminders
                .clone()
                .unwrap_or_else(|| this.reminders_default.borrow().clone());
            if let Some(entry) = list.get_mut(index) {
                entry.minutes = *minutes;
            }
            this.draft.borrow_mut().reminders = Some(list);
            this.rebuild_reminders();
        });
        let weak = Rc::downgrade(self);
        remove.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let mut list = this
                .draft
                .borrow()
                .reminders
                .clone()
                .unwrap_or_else(|| this.reminders_default.borrow().clone());
            if index < list.len() {
                list.remove(index);
            }
            this.draft.borrow_mut().reminders = Some(list);
            this.rebuild_reminders();
        });
        row
    }

    // ---- Notes ----

    fn notes_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Notes"))
            .build();
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::Word)
            .height_request(88)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .build();
        view.buffer().set_text(&self.draft.borrow().notes);
        ui::name(&view, &gettext("Notes"));
        let frame = gtk::Frame::builder().child(&view).build();
        let weak = Rc::downgrade(self);
        view.buffer().connect_changed(move |buffer| {
            let Some(this) = weak.upgrade() else { return };
            let (start, end) = buffer.bounds();
            this.draft.borrow_mut().notes = buffer.text(&start, &end, false).to_string();
        });
        group.add(&frame);
        group
    }

    // ---- More ----

    fn more_group(self: &Rc<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let more = adw::ExpanderRow::builder().title(gettext("More")).build();
        let limited = self
            .draft
            .borrow()
            .base
            .as_ref()
            .is_some_and(draft::limited);
        let all_day = self.draft.borrow().all_day;

        let zone_row = adw::ComboRow::builder()
            .title(gettext("Time zone"))
            .enable_search(true)
            .build();
        crate::ui::name_combo_row_items(&zone_row);
        zone_row.set_visible(!all_day);
        zone_row.set_sensitive(!limited);
        let weak = Rc::downgrade(self);
        let built = Cell::new(false);
        let zone_row_for_expand = zone_row.clone();
        more.connect_expanded_notify(move |row| {
            if !row.is_expanded() || built.get() {
                return;
            }
            built.set(true);
            let Some(this) = weak.upgrade() else { return };
            let names: Vec<&str> = TZ_VARIANTS.iter().map(|tz| tz.name()).collect();
            let model = gtk::StringList::new(&names);
            let expression = gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                None::<gtk::Expression>,
                "string",
            );
            zone_row_for_expand.set_expression(Some(&expression));
            zone_row_for_expand.set_model(Some(&model));
            let current = this.draft.borrow().zone.clone();
            if let Some(i) = TZ_VARIANTS.iter().position(|tz| tz.name() == current) {
                zone_row_for_expand.set_selected(i as u32);
            }
            let weak = Rc::downgrade(&this);
            zone_row_for_expand.connect_selected_notify(move |row| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(tz) = TZ_VARIANTS.get(row.selected() as usize) {
                    this.draft.borrow_mut().zone = tz.name().to_string();
                    this.zone_touched.set(true);
                }
            });
        });
        more.add_row(&zone_row);

        let busy = adw::SwitchRow::builder()
            .title(gettext("Show as busy"))
            .active(self.draft.borrow().busy)
            .build();
        let weak = Rc::downgrade(self);
        busy.connect_active_notify(move |row| {
            if let Some(this) = weak.upgrade() {
                this.draft.borrow_mut().busy = row.is_active();
            }
        });
        more.add_row(&busy);

        let private = adw::SwitchRow::builder()
            .title(gettext("Private"))
            .subtitle(gettext(
                "Only people who can change this calendar see the details",
            ))
            .active(self.draft.borrow().private)
            .sensitive(!limited)
            .build();
        let weak = Rc::downgrade(self);
        private.connect_active_notify(move |row| {
            if let Some(this) = weak.upgrade() {
                this.draft.borrow_mut().private = row.is_active();
            }
        });
        more.add_row(&private);

        let colour_row = adw::ComboRow::builder().title(gettext("Color")).build();
        crate::ui::name_combo_row_items(&colour_row);
        let names: Vec<String> = std::iter::once(gettext("Calendar color"))
            .chain(colour_names())
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        colour_row.set_model(Some(&gtk::StringList::new(&refs)));
        let selected = self
            .draft
            .borrow()
            .color
            .as_deref()
            .and_then(calendar::color_id)
            .and_then(|id| EVENT_COLORS.iter().position(|(i, _)| *i == id))
            .map_or(0, |i| i + 1);
        colour_row.set_selected(selected as u32);
        let weak = Rc::downgrade(self);
        colour_row.connect_selected_notify(move |row| {
            let Some(this) = weak.upgrade() else { return };
            let index = row.selected() as usize;
            this.draft.borrow_mut().color =
                (index > 0).then(|| EVENT_COLORS[index - 1].1.to_string());
        });
        more.add_row(&colour_row);

        let has_conference = self
            .draft
            .borrow()
            .base
            .as_ref()
            .and_then(|b| b.conference.clone());
        let in_series = self
            .draft
            .borrow()
            .occurrence
            .as_ref()
            .is_some_and(|o| mailrs_domain::calendar::series::in_series(&o.event));
        if let Some(link) = has_conference {
            let row = adw::ActionRow::builder()
                .title(gettext("Google Meet"))
                .subtitle(link)
                .build();
            row.set_sensitive(!limited);
            more.add_row(&row);
        } else {
            let row = adw::SwitchRow::builder()
                .title(gettext("Add Google Meet"))
                .active(self.draft.borrow().add_meet)
                .build();
            row.set_sensitive(!limited);
            // A Meet request on a patch of one occurrence would add the
            // link to that occurrence alone; offering it there would read
            // as changing the whole series.
            row.set_visible(!in_series);
            let weak = Rc::downgrade(self);
            row.connect_active_notify(move |row| {
                if let Some(this) = weak.upgrade() {
                    this.draft.borrow_mut().add_meet = row.is_active();
                }
            });
            more.add_row(&row);
        }

        group.add(&more);
        group
    }
}

/// The Repeats row's fixed choices, in menu order. "Custom…" is the row
/// after the last of these, not a value of its own.
const REPEAT_PRESETS: [Repeat; 6] = [
    Repeat::Never,
    Repeat::EveryDay,
    Repeat::EveryWeekday,
    Repeat::EveryWeek,
    Repeat::EveryMonth,
    Repeat::EveryYear,
];

const FREQUENCIES: [Frequency; 4] = [
    Frequency::Daily,
    Frequency::Weekly,
    Frequency::Monthly,
    Frequency::Yearly,
];

const WEEK: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

/// The Repeats row's index for `repeat`: its preset's position, or the
/// row after the presets for a custom or a kept rule.
fn repeat_index(repeat: &Repeat) -> u32 {
    REPEAT_PRESETS
        .iter()
        .position(|p| p == repeat)
        .map_or(REPEAT_PRESETS.len() as u32, |i| i as u32)
}

fn frequency_index(frequency: Frequency) -> u32 {
    FREQUENCIES
        .iter()
        .position(|f| *f == frequency)
        .unwrap_or(1) as u32
}

fn unit_model(every: u32) -> gtk::StringList {
    let names = [
        ngettext("day", "days", every),
        ngettext("week", "weeks", every),
        ngettext("month", "months", every),
        ngettext("year", "years", every),
    ];
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    gtk::StringList::new(&refs)
}

/// A weekday's full name. The same words the invitation card's recurrence
/// line already uses (`domain/src/invitation/recurrence.rs`), read
/// through plain `gettext` rather than through a second function, so the
/// template gains no new msgid for them.
fn weekday_name(day: Weekday) -> String {
    match day {
        Weekday::Mon => gettext("Monday"),
        Weekday::Tue => gettext("Tuesday"),
        Weekday::Wed => gettext("Wednesday"),
        Weekday::Thu => gettext("Thursday"),
        Weekday::Fri => gettext("Friday"),
        Weekday::Sat => gettext("Saturday"),
        Weekday::Sun => gettext("Sunday"),
    }
}

fn first_letter(word: &str) -> String {
    word.chars().next().map(String::from).unwrap_or_default()
}

/// The eleven colour names, in [`EVENT_COLORS`] order.
fn colour_names() -> Vec<String> {
    vec![
        gettext("Lavender"),
        gettext("Sage"),
        gettext("Grape"),
        gettext("Flamingo"),
        gettext("Banana"),
        gettext("Tangerine"),
        gettext("Peacock"),
        gettext("Graphite"),
        gettext("Blueberry"),
        gettext("Basil"),
        gettext("Tomato"),
    ]
}

/// `REMINDER_CHOICES` with `minutes` added when it is not already one of
/// them, sorted.
fn reminder_choice_values(minutes: u32) -> Vec<u32> {
    let mut values: Vec<u32> = REMINDER_CHOICES.to_vec();
    if !values.contains(&minutes) {
        values.push(minutes);
        values.sort_unstable();
    }
    values
}

/// A button showing a date that opens a calendar in a popover.
fn date_button(
    day: NaiveDate,
    spoken: &str,
    on_pick: impl Fn(NaiveDate) + 'static,
) -> gtk::MenuButton {
    let calendar = gtk::Calendar::new();
    calendar.set_date(&day_to_glib(day));
    let button = gtk::MenuButton::builder()
        .label(format_date(day))
        .popover(&gtk::Popover::builder().child(&calendar).build())
        .valign(gtk::Align::Center)
        .build();
    ui::name(&button, spoken);
    let shown = button.clone();
    calendar.connect_day_selected(move |calendar| {
        let picked = calendar.date();
        if let Some(day) = NaiveDate::from_ymd_opt(
            picked.year(),
            picked.month() as u32,
            picked.day_of_month() as u32,
        ) {
            shown.set_label(&format_date(day));
            shown.popdown();
            on_pick(day);
        }
    });
    button
}

fn day_to_glib(day: NaiveDate) -> glib::DateTime {
    glib::DateTime::from_local(day.year(), day.month() as i32, day.day() as i32, 12, 0, 0.0)
        .expect("a real date")
}

fn format_date(day: NaiveDate) -> String {
    day.format_localized(&gettext("%a %-d %b"), date_locale())
        .to_string()
}

fn format_time(time: NaiveTime) -> String {
    // Neither `NaiveTime` nor `NaiveDateTime` has `format_localized`;
    // pairing the time with a fixed date and a zone costs nothing, since
    // `%H:%M` names no month or weekday.
    NaiveDate::from_ymd_opt(2000, 1, 1)
        .expect("a real date")
        .and_time(time)
        .and_utc()
        .format_localized(&gettext("%H:%M"), date_locale())
        .to_string()
}

fn time_dropdown(current: NaiveTime) -> gtk::DropDown {
    let times = draft::time_choices(current);
    let names: Vec<String> = times.iter().map(|t| format_time(*t)).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let drop = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&refs))
        .enable_search(true)
        .build();
    if let Some(i) = times.iter().position(|t| *t == current) {
        drop.set_selected(i as u32);
    }
    drop
}

/// The time an open `gtk::DropDown` built by [`time_dropdown`] holds now,
/// read back from its shown string.
fn selected_time(drop: &gtk::DropDown) -> Option<NaiveTime> {
    let text = drop
        .selected_item()
        .and_downcast::<gtk::StringObject>()?
        .string();
    NaiveTime::parse_from_str(&text, "%H:%M").ok()
}

fn select_time(drop: &gtk::DropDown, time: NaiveTime) {
    let times = draft::time_choices(time);
    let names: Vec<String> = times.iter().map(|t| format_time(*t)).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    drop.set_model(Some(&gtk::StringList::new(&refs)));
    if let Some(i) = times.iter().position(|t| *t == time) {
        drop.set_selected(i as u32);
    }
}

fn time_to_hours(time: NaiveTime) -> f64 {
    f64::from(time.hour()) + f64::from(time.minute()) / 60.0 + f64::from(time.second()) / 3600.0
}

fn local_day(at: EpochMillis, zone: Tz) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .with_timezone(&zone)
        .date_naive()
}

fn local_time(at: EpochMillis, zone: Tz) -> NaiveTime {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .with_timezone(&zone)
        .time()
}

/// The key the Calendar row's model holds for a calendar: its account
/// and id, which together name it, since two accounts can share a
/// calendar id.
fn calendar_key(account: AccountId, id: &str) -> String {
    format!("{account}\u{1f}{id}")
}

/// The entry `key` names among the Calendar row's choices.
fn calendar_entry<'a>(
    entries: &'a [(AccountId, String, Calendar)],
    key: &str,
) -> Option<&'a (AccountId, String, Calendar)> {
    entries
        .iter()
        .find(|(account, _, c)| calendar_key(*account, &c.id) == key)
}

/// The Calendar row's per-item widget: a colour swatch and a two-line
/// label, filled in by [`fill_calendar_item`]. Built once per list item
/// and reused as the item scrolls, the way `gtk::SignalListItemFactory`
/// expects.
fn calendar_item_widget() -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();
    // `checkbutton.calendar-check` is the sidebar's own rule for this
    // dot (`app/data/style.css`); a plain `gtk::Box` has no hook for it,
    // so a real (non-interactive) check button reuses that rule rather
    // than adding a second one this task's file list has no room for.
    let dot = gtk::CheckButton::builder()
        .active(true)
        .can_focus(false)
        .can_target(false)
        .build();
    // Decorative: the row's own name already says which calendar it is,
    // so a screen reader gets nothing more from a dot it cannot press.
    dot.set_accessible_role(gtk::AccessibleRole::Presentation);
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    let name = gtk::Label::builder().xalign(0.0).build();
    let address = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    names.append(&name);
    names.append(&address);
    row.append(&dot);
    row.append(&names);
    row
}

fn fill_calendar_item(item: &gtk::Widget, calendar: &Calendar, address: &str) {
    let Some(row) = item.clone().downcast::<gtk::Box>().ok() else {
        return;
    };
    let Some(dot) = row.first_child().and_downcast::<gtk::CheckButton>() else {
        return;
    };
    dot.set_css_classes(&["calendar-check", &tint::css_class(&calendar.color)]);
    if let Some(names) = row
        .first_child()
        .and_then(|d| d.next_sibling())
        .and_downcast::<gtk::Box>()
    {
        if let Some(name) = names.first_child().and_downcast::<gtk::Label>() {
            name.set_label(&calendar.name);
        }
        if let Some(address_label) = names
            .first_child()
            .and_then(|n| n.next_sibling())
            .and_downcast::<gtk::Label>()
        {
            address_label.set_label(address);
        }
    }
    ui::name(
        item,
        &fill(
            &gettext("{name}, {address}"),
            &[("name", &calendar.name), ("address", address)],
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(id: &str, name: &str) -> Calendar {
        Calendar { id: id.into(), name: name.into(), ..Calendar::default() }
    }

    fn entries() -> Vec<(AccountId, String, Calendar)> {
        vec![
            (1, "me@example.com".into(), calendar("me@example.com", "Personal")),
            (2, "me@work.pt".into(), calendar("team", "Design team")),
            (1, "me@example.com".into(), calendar("family", "Family")),
        ]
    }

    #[test]
    fn a_calendar_row_item_finds_its_entry_by_key_not_by_position() {
        let entries = entries();
        let key = calendar_key(1, "family");
        let (account, _, found) = calendar_entry(&entries, &key).expect("the key names an entry");
        assert_eq!((*account, found.name.as_str()), (1, "Family"));
    }

    #[test]
    fn the_same_calendar_id_on_two_accounts_keeps_two_keys() {
        assert_ne!(calendar_key(1, "team"), calendar_key(2, "team"));
        assert!(calendar_entry(&entries(), &calendar_key(1, "team")).is_none());
    }
}
