//! `Agenda`, the flat list of upcoming occurrences: a date heading per
//! day, then a row per occurrence: a colour dot, its start and end time
//! or "All day", the title, and the place and calendar name dimmed
//! below it.
//!
//! A `gtk::ListView` over `AgendaModel`, a `gio::ListStore`-like model
//! that also implements `gtk::SectionModel`, builds a widget only for the
//! rows on screen; a `gtk::ListBox` would build a widget tree for every
//! day loaded, and scrolling up loads as much as a year.
//!
//! The first row of each date draws the date's heading above itself,
//! rather than GTK's own section header. GTK's list keeps its scroll
//! position as a row at the top edge, never a header, so a list set to a
//! day opened on that day's first row with its heading just above the
//! edge ("09:30 Stand-up" with no day over it), and moving the list back
//! by hand drifted again with every resize and late layout.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use chrono::{NaiveDate, TimeZone};
use gtk::pango;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::translate::gettext;
use mailrs_domain::{AccountId, EpochMillis};

use super::tint;
use super::words;

/// One row's data: the occurrence, the local date it groups under, and
/// the heading its section shares, computed once in [`Agenda::show`] or
/// [`Agenda::prepend`] so neither the row factory nor the header
/// factory needs the zone again. `date` is kept on the row, rather than
/// alongside it, so [`AgendaModel::prepend_rows`] can recompute every
/// section boundary after splicing old and new rows together.
#[derive(Debug, Clone)]
struct Row {
    occurrence: Occurrence,
    date: NaiveDate,
    heading: String,
}

/// Turns `occurrences` into rows in the order the agenda always shows
/// them: earliest date first, an all-day occurrence before a timed one
/// on the same date, then by start time. Shared by [`Agenda::show`],
/// which replaces every row, and [`Agenda::prepend`], which adds rows
/// before them.
fn sorted_rows<Z: TimeZone>(occurrences: &[Occurrence], zone: &Z) -> Vec<Row> {
    days_of(occurrences, zone)
        .into_iter()
        .flat_map(|(date, day)| {
            let heading = words::full_date_words(date);
            day.into_iter().map(move |occurrence| Row {
                heading: heading.clone(),
                date,
                occurrence,
            })
        })
        .collect()
}

/// `occurrences` grouped into days, earliest first; within a day an
/// all-day occurrence comes before a timed one, then by start time.
fn days_of<Z: TimeZone>(
    occurrences: &[Occurrence],
    zone: &Z,
) -> Vec<(NaiveDate, Vec<Occurrence>)> {
    let mut dated: Vec<(NaiveDate, Occurrence)> = occurrences
        .iter()
        .map(|o| (agenda_date(o, zone), o.clone()))
        .collect();
    dated.sort_by_key(|(date, o)| (*date, !o.event.all_day, o.start));
    let mut days: Vec<(NaiveDate, Vec<Occurrence>)> = Vec::new();
    for (date, o) in dated {
        match days.last_mut() {
            Some((last, day)) if *last == date => day.push(o),
            _ => days.push((date, vec![o])),
        }
    }
    days
}

/// The occurrences that group under `first` or a later day. A read of
/// later days also returns an event that began before them and runs
/// into them, which the list already shows under its own first day.
fn starting_from<Z: TimeZone>(
    occurrences: &[Occurrence],
    first: NaiveDate,
    zone: &Z,
) -> Vec<Occurrence> {
    occurrences
        .iter()
        .filter(|o| agenda_date(o, zone) >= first)
        .cloned()
        .collect()
}

/// The local date a row groups under: an all-day occurrence by its own
/// UTC date, a timed one by local wall time. An all-day event's
/// midnights are UTC's, and read in local time west of UTC they would
/// land on the day before.
fn agenda_date<Z: TimeZone>(o: &Occurrence, zone: &Z) -> NaiveDate {
    let start: EpochMillis = o.start;
    let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(start).unwrap_or_default();
    if o.event.all_day {
        utc.date_naive()
    } else {
        utc.with_timezone(zone).date_naive()
    }
}

/// The section boundaries [`model::AgendaModel`] answers with: one run
/// per stretch of consecutive equal dates in `dates`. [`Agenda::show`]
/// sorts occurrences by date first, so each date's rows are already
/// contiguous.
fn sections_of(dates: &[NaiveDate]) -> Vec<(u32, u32)> {
    let mut sections = Vec::new();
    let mut start = 0usize;
    for i in 1..=dates.len() {
        if i == dates.len() || dates[i] != dates[start] {
            sections.push((start as u32, i as u32));
            start = i;
        }
    }
    sections
}

/// Whether the row at `position` is the first of its date in `sections`,
/// and so draws the date's heading above itself.
fn opens_section(sections: &[(u32, u32)], position: u32) -> bool {
    sections.iter().any(|&(start, _)| start == position)
}

mod model {
    use super::*;

    mod imp {
        use super::*;

        #[derive(Default)]
        pub struct AgendaModel {
            pub(super) items: RefCell<Vec<Row>>,
            pub sections: RefCell<Vec<(u32, u32)>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for AgendaModel {
            const NAME: &'static str = "MailrsAgendaModel";
            type Type = super::AgendaModel;
            type Interfaces = (gio::ListModel, gtk::SectionModel);
        }

        impl ObjectImpl for AgendaModel {}

        impl gio::subclass::prelude::ListModelImpl for AgendaModel {
            fn item_type(&self) -> glib::Type {
                glib::BoxedAnyObject::static_type()
            }

            fn n_items(&self) -> u32 {
                self.items.borrow().len() as u32
            }

            fn item(&self, position: u32) -> Option<glib::Object> {
                self.items
                    .borrow()
                    .get(position as usize)
                    .cloned()
                    .map(|row| glib::BoxedAnyObject::new(row).upcast())
            }
        }

        impl gtk::subclass::prelude::SectionModelImpl for AgendaModel {
            fn section(&self, position: u32) -> (u32, u32) {
                let len = self.items.borrow().len() as u32;
                self.sections
                    .borrow()
                    .iter()
                    .find(|&&(start, end)| position >= start && position < end)
                    .copied()
                    .unwrap_or((0, len))
            }
        }
    }

    glib::wrapper! {
        pub struct AgendaModel(ObjectSubclass<imp::AgendaModel>)
            @implements gio::ListModel, gtk::SectionModel;
    }

    impl Default for AgendaModel {
        fn default() -> Self {
            glib::Object::new()
        }
    }

    impl AgendaModel {
        /// Replaces every row and its section boundaries in one go, the
        /// same full-rebuild `show` gives every other calendar widget.
        pub(super) fn set_rows(&self, items: Vec<Row>, sections: Vec<(u32, u32)>) {
            let old = self.imp().items.borrow().len() as u32;
            self.imp().items.replace(items);
            let new = self.imp().items.borrow().len() as u32;
            self.imp().sections.replace(sections);
            self.items_changed(0, old, new);
            self.sections_changed(0, new);
        }

        /// Adds `items` (already sorted) after the last row and
        /// recomputes the section boundaries, since the first new row can
        /// share the last section's date. Nothing scrolls.
        pub(super) fn append_rows(&self, items: Vec<Row>) {
            let added = items.len() as u32;
            if added == 0 {
                return;
            }
            let old = self.imp().items.borrow().len() as u32;
            self.imp().items.borrow_mut().extend(items);
            let dates: Vec<NaiveDate> =
                self.imp().items.borrow().iter().map(|row| row.date).collect();
            self.imp().sections.replace(sections_of(&dates));
            self.items_changed(old, 0, added);
            self.sections_changed(0, old + added);
        }

        /// Inserts `items` (already sorted, oldest first) before the
        /// model's own first row and recomputes every section boundary,
        /// since the new rows' last date could be the same as what was
        /// the first section's. Answers how many rows it
        /// inserted, 0 for an empty `items`, which leaves the model
        /// untouched rather than firing a no-op change.
        pub(super) fn prepend_rows(&self, mut items: Vec<Row>) -> u32 {
            let inserted = items.len() as u32;
            if inserted == 0 {
                return 0;
            }
            let mut rest = self.imp().items.take();
            items.append(&mut rest);
            let dates: Vec<NaiveDate> = items.iter().map(|row| row.date).collect();
            let sections = sections_of(&dates);
            let new_len = items.len() as u32;
            self.imp().items.replace(items);
            self.imp().sections.replace(sections);
            self.items_changed(0, 0, inserted);
            self.sections_changed(0, new_len);
            inserted
        }
    }
}

/// The widget one occurrence's row draws: a coloured dot (striped for out
/// of office, or the target of focus time and the cake of a birthday in
/// its place), the time (or "All day"), the title, and the place dimmed. A `gtk::Box` subclass so
/// the row factory's bind step can reach its own labels back out, the
/// same reason `ThreadRow` is one (`app/src/ui/mod.rs`).
mod row {
    use super::*;

    mod imp {
        use std::cell::OnceCell;

        use super::*;

        #[derive(Default)]
        pub struct AgendaRow {
            /// The date's heading, shown on the date's first row only.
            pub heading: OnceCell<gtk::Label>,
            pub dot: OnceCell<gtk::Box>,
            /// Focus time's target or a birthday's cake, shown instead of
            /// the dot, as the week and month blocks show them.
            pub kind: OnceCell<gtk::Image>,
            pub time: OnceCell<gtk::Label>,
            pub title: OnceCell<gtk::Label>,
            pub place: OnceCell<gtk::Label>,
            /// The occurrence the row draws now, for the Delete key and N
            /// while the row's list item holds the focus.
            pub occurrence: RefCell<Option<Occurrence>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for AgendaRow {
            const NAME: &'static str = "MailrsAgendaRow";
            type Type = super::AgendaRow;
            type ParentType = gtk::Box;
        }

        impl ObjectImpl for AgendaRow {
            fn constructed(&self) {
                self.parent_constructed();
                let outer = self.obj();
                outer.set_orientation(gtk::Orientation::Vertical);
                outer.set_accessible_role(gtk::AccessibleRole::ListItem);
                let heading = gtk::Label::builder()
                    .css_classes(["agenda-heading", "heading"])
                    .xalign(0.0)
                    // The space GTK's own section header kept around it.
                    .margin_top(20)
                    .margin_bottom(8)
                    .accessible_role(gtk::AccessibleRole::Heading)
                    .visible(false)
                    .build();
                // A press on the date is not a press on the event under it.
                let still = gtk::GestureClick::new();
                still.connect_pressed(|gesture, _, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                });
                heading.add_controller(still);
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .margin_top(4)
                    .margin_bottom(4)
                    .css_classes(["agenda-row"])
                    .build();
                outer.append(&heading);
                outer.append(&row);
                let _ = self.heading.set(heading);

                let dot = gtk::Box::builder()
                    .valign(gtk::Align::Center)
                    .css_classes(["agenda-dot"])
                    .build();
                let kind = gtk::Image::builder()
                    .pixel_size(12)
                    .valign(gtk::Align::Center)
                    .css_classes(["agenda-kind"])
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .visible(false)
                    .build();
                let time = gtk::Label::builder()
                    .css_classes(["dim-label", "caption"])
                    // Wide enough for "10:00–11:30" without wrapping;
                    // "All day" and a single "10:00" both fall short of it.
                    .width_chars(11)
                    .xalign(0.0)
                    .valign(gtk::Align::Center)
                    .build();
                let title = gtk::Label::builder()
                    .hexpand(true)
                    .xalign(0.0)
                    .ellipsize(pango::EllipsizeMode::End)
                    .single_line_mode(true)
                    .build();
                let place = gtk::Label::builder()
                    .css_classes(["dim-label", "caption"])
                    .xalign(0.0)
                    .ellipsize(pango::EllipsizeMode::End)
                    .single_line_mode(true)
                    .visible(false)
                    .build();
                for widget in [
                    dot.upcast_ref::<gtk::Widget>(),
                    kind.upcast_ref(),
                    time.upcast_ref(),
                    title.upcast_ref(),
                    place.upcast_ref(),
                ] {
                    row.append(widget);
                }
                let _ = self.dot.set(dot);
                let _ = self.kind.set(kind);
                let _ = self.time.set(time);
                let _ = self.title.set(title);
                let _ = self.place.set(place);
            }
        }

        impl WidgetImpl for AgendaRow {}
        impl BoxImpl for AgendaRow {}
    }

    glib::wrapper! {
        pub struct AgendaRow(ObjectSubclass<imp::AgendaRow>)
            @extends gtk::Box, gtk::Widget,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
    }

    impl Default for AgendaRow {
        fn default() -> Self {
            glib::Object::new()
        }
    }

    impl AgendaRow {
        /// The occurrence the row draws now.
        pub(super) fn occurrence(&self) -> Option<Occurrence> {
            self.imp().occurrence.borrow().clone()
        }

        /// Fills every label from `row`'s occurrence, using `calendars`
        /// for the dot's colour and the calendar name shown below the
        /// title and read out in the accessible name. `opens` says the
        /// row is its date's first, which draws the date's heading.
        pub(super) fn fill(
            &self,
            row: &Row,
            opens: bool,
            calendars: &HashMap<(AccountId, String), Calendar>,
        ) {
            let imp = self.imp();
            let heading = imp.heading.get().expect("built in constructed");
            heading.set_visible(opens);
            heading.set_label(&row.heading);
            let o = &row.occurrence;
            imp.occurrence.replace(Some(o.clone()));
            let (colour, calendar_name) = calendar_of(o, calendars);

            let dot = imp.dot.get().expect("built in constructed");
            let tinted = tint::css_class(colour);
            let look = crate::ui::calendar::kinds::look(&o.event.kind);
            let mut classes = vec!["agenda-dot", tinted.as_str()];
            classes.extend(look.css_class());
            dot.set_css_classes(&classes);
            let kind = imp.kind.get().expect("built in constructed");
            kind.set_css_classes(&["agenda-kind", &tinted]);
            kind.set_icon_name(look.icon());
            kind.set_visible(look.icon().is_some());
            dot.set_visible(look.icon().is_none());

            imp.time
                .get()
                .expect("built in constructed")
                .set_label(&words::agenda_span_words(o, &chrono::Local));
            imp.title
                .get()
                .expect("built in constructed")
                .set_label(&o.event.title);

            let subtitle = words::agenda_subtitle_words(&o.event.place, calendar_name);
            let place = imp.place.get().expect("built in constructed");
            place.set_visible(!subtitle.is_empty());
            place.set_label(&subtitle);

            // Out of office and focus time say so, since the stripes and
            // the icon that show it are not spoken.
            let titled = match crate::ui::calendar::kinds::kind_words(&o.event.kind) {
                Some(kind) if !kind.eq_ignore_ascii_case(o.event.title.trim()) => mailrs_domain::translate::fill(
                    &gettext("{title}, {kind}"),
                    &[("title", &o.event.title), ("kind", &kind)],
                ),
                _ => o.event.title.clone(),
            };
            let name = mailrs_domain::translate::fill(
                &gettext("{title}, {when}, {calendar}"),
                &[
                    ("title", &titled),
                    ("when", &words::when_words(o, &chrono::Local)),
                    ("calendar", calendar_name),
                ],
            );
            crate::ui::name(self, &name);
        }
    }
}

use model::AgendaModel;
use row::AgendaRow;

/// The calendar an occurrence's event names: its own colour and name.
/// Missing from `calendars` only when a caller passes an incomplete map;
/// an empty pair still draws a usable row.
fn calendar_of<'a>(
    o: &Occurrence,
    calendars: &'a HashMap<(AccountId, String), Calendar>,
) -> (&'a str, &'a str) {
    match calendars.get(&(o.account_id, o.event.calendar.clone())) {
        Some(calendar) => (calendar.color.as_str(), calendar.name.as_str()),
        None => ("", ""),
    }
}

/// The widest the agenda's column grows in a wide window, in pixels.
const COLUMN_WIDTH: i32 = 720;

type Activated = dyn Fn(&Occurrence);
type ScrolledToTop = dyn Fn();

pub struct Agenda {
    /// What callers place: the column around the scrolled list.
    pub widget: gtk::Widget,
    /// The scrolled list itself, for sizing it in a popover.
    pub scrolled: gtk::ScrolledWindow,
    model: AgendaModel,
    /// Kept to scroll it after [`Agenda::prepend`]: the row
    /// that was first before the insert is asked to stay first.
    list_view: gtk::ListView,
    /// The dim line [`Agenda::show_no_earlier`] reveals once loading has
    /// reached the earliest day the agenda loads, above the list's own
    /// first row so it reads as part of the same scrolling content.
    no_earlier: gtk::Label,
    /// Shared with the row factory, which reads each dot's colour from it
    /// as a row scrolls into view.
    calendars: Rc<RefCell<HashMap<(AccountId, String), Calendar>>>,
    activated: Rc<RefCell<Option<Box<Activated>>>>,
    scrolled_to_top: Rc<RefCell<Option<Box<ScrolledToTop>>>>,
}

impl Agenda {
    pub fn new() -> Rc<Agenda> {
        let model = AgendaModel::default();
        let selection = gtk::NoSelection::new(Some(model.clone()));

        let calendars: Rc<RefCell<HashMap<(AccountId, String), Calendar>>> =
            Rc::new(RefCell::new(HashMap::new()));

        let row_factory = gtk::SignalListItemFactory::new();
        row_factory.connect_setup(move |_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("list items are ListItems");
            item.set_child(Some(&AgendaRow::default()));
        });
        let bind_calendars = Rc::clone(&calendars);
        let bind_model = model.clone();
        row_factory.connect_bind(move |_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("list items are ListItems");
            let Some(widget) = item.child().and_downcast::<AgendaRow>() else {
                return;
            };
            let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let row = boxed.borrow::<Row>();
            let opens = opens_section(&bind_model.imp().sections.borrow(), item.position());
            widget.fill(&row, opens, &bind_calendars.borrow());
        });

        let list_view = gtk::ListView::builder()
            .model(&selection)
            .factory(&row_factory)
            .single_click_activate(true)
            .build();
        list_view.add_css_class("agenda-list");

        let activated: Rc<RefCell<Option<Box<Activated>>>> = Rc::new(RefCell::new(None));
        let on_activate = Rc::clone(&activated);
        let activate_model = model.clone();
        list_view.connect_activate(move |_, position| {
            if let Some(item) = activate_model
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
            {
                let row = item.borrow::<Row>();
                if let Some(f) = on_activate.borrow().as_ref() {
                    f(&row.occurrence);
                }
            }
        });

        // The list view is the scrolled window's own child: inside a box
        // or a viewport GTK gives it the height of every row and builds a
        // widget for each, which is what "virtualised" exists to avoid.
        // The dim line therefore sits above the scrolled window, and
        // shows once the top has been reached.
        let no_earlier = gtk::Label::builder()
            .label(gettext("Nothing earlier on this computer"))
            .css_classes(["dim-label", "caption"])
            .margin_top(10)
            .margin_bottom(10)
            .visible(false)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list_view)
            .build();
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        content.append(&no_earlier);
        content.append(&scrolled);
        // A wide window reads the agenda as a centered column; a narrow
        // one is below the clamp's size and fills its width as before.
        let widget = adw::Clamp::builder()
            .maximum_size(COLUMN_WIDTH)
            .tightening_threshold(COLUMN_WIDTH)
            .child(&content)
            .build();
        let scrolled_to_top: Rc<RefCell<Option<Box<ScrolledToTop>>>> = Rc::new(RefCell::new(None));
        let top_slot = Rc::clone(&scrolled_to_top);
        scrolled.connect_edge_reached(move |_, position| {
            if position == gtk::PositionType::Top
                && let Some(f) = top_slot.borrow().as_ref()
            {
                f();
            }
        });

        Rc::new(Agenda {
            widget: widget.upcast(),
            scrolled,
            model,
            list_view,
            no_earlier,
            calendars,
            activated,
            scrolled_to_top,
        })
    }

    /// Rebuilds every row from `occurrences`, sorted and grouped into one
    /// section per local date (an all-day occurrence groups by its own
    /// UTC date). `calendars` gives each dot its colour.
    pub fn show(
        &self,
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
        zone: &chrono::Local,
    ) {
        *self.calendars.borrow_mut() = calendars.clone();
        self.no_earlier.set_visible(false);
        let rows = sorted_rows(occurrences, zone);
        let dates: Vec<NaiveDate> = rows.iter().map(|row| row.date).collect();
        let sections = sections_of(&dates);
        self.model.set_rows(rows, sections);
        // A new list starts at its first heading, not wherever the last
        // one was scrolled to.
        self.scrolled.vadjustment().set_value(0.0);
    }

    /// Inserts `occurrences` before the agenda's earliest row and
    /// scrolls so the row that was first stays first: `ListView::scroll_to`
    /// with its new index, once GTK has laid the inserted rows out.
    /// `occurrences` must run entirely before the earliest date already
    /// shown, which `shown::not_yet_listed` makes true. `calendars` is merged in
    /// rather than replacing what `show` set, since a widened window can
    /// meet a calendar the first read never had to draw.
    pub fn prepend(
        &self,
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
        zone: &chrono::Local,
    ) {
        self.calendars
            .borrow_mut()
            .extend(calendars.iter().map(|(k, v)| (k.clone(), v.clone())));
        let rows = sorted_rows(occurrences, zone);
        let inserted = self.model.prepend_rows(rows);
        if inserted > 0 {
            self.list_view.scroll_to(inserted, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// Adds `occurrences` after the agenda's last row. Only those that
    /// group under `first` or later are kept, so an event that runs
    /// across the seam is not listed twice.
    pub fn append(
        &self,
        occurrences: &[Occurrence],
        first: NaiveDate,
        calendars: &HashMap<(AccountId, String), Calendar>,
        zone: &chrono::Local,
    ) {
        self.calendars
            .borrow_mut()
            .extend(calendars.iter().map(|(k, v)| (k.clone(), v.clone())));
        let rows = sorted_rows(&starting_from(occurrences, first, zone), zone);
        self.model.append_rows(rows);
    }

    /// Runs `f` when the scroll position comes within `margin` pixels of
    /// the bottom, and again when the content grows and leaves it there,
    /// so the caller loads later days.
    pub fn connect_near_end(&self, margin: f64, f: impl Fn() + 'static) {
        let f = Rc::new(f);
        let adjustment = self.scrolled.vadjustment();
        let check = move |a: &gtk::Adjustment| {
            if super::shown::near_end(a.value(), a.page_size(), a.upper(), margin) {
                f();
            }
        };
        let on_value = check.clone();
        adjustment.connect_value_changed(move |a| on_value(a));
        adjustment.connect_changed(move |a| check(a));
    }

    /// Reveals the dim line saying the copy holds nothing earlier, once
    /// loading has reached `range::earliest_agenda_day`. `show` hides
    /// it again, for the range change that follows leaving List mode
    /// and coming back.
    pub fn show_no_earlier(&self) {
        self.no_earlier.set_visible(true);
    }

    /// Runs `f` with the occurrence a row was activated for.
    pub fn connect_event_activated(&self, f: impl Fn(&Occurrence) + 'static) {
        self.activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` when the agenda is scrolled to its top, so the caller
    /// loads earlier days.
    pub fn connect_scrolled_to_top(&self, f: impl Fn() + 'static) {
        self.scrolled_to_top.replace(Some(Box::new(f)));
    }

    /// The occurrence of the row with the keyboard focus. GTK's
    /// `ListView` gives the focus to the list item it wraps each row in,
    /// so the row is that widget's child rather than the focus itself.
    pub fn focused(&self) -> Option<Occurrence> {
        let focus = self.list_view.root()?.focus()?;
        if !focus.is_ancestor(&self.list_view) {
            return None;
        }
        let mut current = Some(focus);
        while let Some(widget) = current {
            if widget == *self.list_view.upcast_ref::<gtk::Widget>() {
                return None;
            }
            let row = widget
                .downcast_ref::<AgendaRow>()
                .cloned()
                .or_else(|| widget.first_child().and_downcast::<AgendaRow>());
            if let Some(row) = row {
                return row.occurrence();
            }
            current = widget.parent();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn sections_of_groups_consecutive_equal_dates() {
        let dates = [
            d(2026, 9, 23),
            d(2026, 9, 23),
            d(2026, 9, 24),
            d(2026, 9, 25),
            d(2026, 9, 25),
        ];
        assert_eq!(sections_of(&dates), vec![(0, 2), (2, 3), (3, 5)]);
    }

    fn timed(start: i64) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: std::sync::Arc::new(mailrs_domain::calendar::Event {
                start,
                end: start + 3_600_000,
                ..Default::default()
            }),
            start,
            end: start + 3_600_000,
        }
    }

    #[test]
    fn events_group_into_days_earliest_first_with_all_day_ahead_of_timed() {
        let zone = chrono_tz::Europe::Lisbon;
        let at = |day, hour| {
            zone.with_ymd_and_hms(2026, 9, day, hour, 0, 0).unwrap().timestamp_millis()
        };
        let mut all_day = timed(chrono::Utc.with_ymd_and_hms(2026, 9, 24, 0, 0, 0).unwrap().timestamp_millis());
        std::sync::Arc::make_mut(&mut all_day.event).all_day = true;
        let found = [timed(at(25, 9)), timed(at(24, 15)), all_day, timed(at(24, 8))];
        let days = days_of(&found, &zone);
        let shape: Vec<(NaiveDate, Vec<bool>)> = days
            .iter()
            .map(|(d, os)| (*d, os.iter().map(|o| o.event.all_day).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![(d(2026, 9, 24), vec![true, false, false]), (d(2026, 9, 25), vec![false])]
        );
        assert!(days[0].1[1].start < days[0].1[2].start);
    }

    #[test]
    fn a_later_read_drops_what_an_earlier_day_already_listed() {
        let zone = chrono_tz::Europe::Lisbon;
        let at = |day, hour| {
            zone.with_ymd_and_hms(2026, 9, day, hour, 0, 0).unwrap().timestamp_millis()
        };
        // The store returns a multi-day event again when a later read
        // overlaps it; its own start still sits before that read.
        let found = [timed(at(23, 22)), timed(at(24, 9))];
        let kept = starting_from(&found, d(2026, 9, 24), &zone);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].start, at(24, 9));
    }

    #[test]
    fn rows_appended_on_the_last_date_join_its_section() {
        // The model recomputes every boundary from all its dates after an
        // append, so a later read that starts on the day the list ended
        // on adds to that day's section rather than opening a new one.
        let mut dates = vec![d(2026, 9, 23), d(2026, 9, 24)];
        dates.extend([d(2026, 9, 24), d(2026, 9, 25)]);
        assert_eq!(sections_of(&dates), vec![(0, 1), (1, 3), (3, 4)]);
    }

    #[test]
    fn the_first_row_of_each_day_carries_its_heading() {
        let sections = [(0, 2), (2, 3), (3, 5)];
        let heads: Vec<u32> = (0..5).filter(|&p| opens_section(&sections, p)).collect();
        assert_eq!(heads, vec![0, 2, 3]);
    }

    #[test]
    fn sections_of_an_empty_list_is_empty() {
        assert_eq!(sections_of(&[]), Vec::new());
    }

    #[test]
    fn an_all_day_occurrence_groups_by_its_utc_date() {
        // Lisbon is UTC-1 in winter, so 23:30 UTC on the 30th is already
        // the 31st in local time; an all-day occurrence must not follow
        // it there.
        let start = chrono::Utc
            .with_ymd_and_hms(2026, 1, 31, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let o = Occurrence {
            account_id: 1,
            event: std::sync::Arc::new(mailrs_domain::calendar::Event {
                all_day: true,
                start,
                end: start + 86_400_000,
                ..Default::default()
            }),
            start,
            end: start + 86_400_000,
        };
        assert_eq!(agenda_date(&o, &chrono_tz::Europe::Lisbon), d(2026, 1, 31));
    }
}
