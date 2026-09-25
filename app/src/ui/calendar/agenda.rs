//! `Agenda`, the flat list of upcoming occurrences: a date heading per
//! day, then a row per occurrence: a colour dot, the time or "All day",
//! the title, and the place dimmed.
//!
//! A `gtk::ListView` over `AgendaModel`, a `gio::ListStore`-like model
//! that also implements `gtk::SectionModel`, gives GTK the date headings
//! with a widget only for the rows on screen; a `gtk::ListBox` would
//! build a widget tree for every day loaded, and Task 7 loads up to a
//! year (reconcile.md Task 5 item 10, Memory item 6).

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
/// before them (Task 7).
fn sorted_rows(occurrences: &[Occurrence], zone: &chrono::Local) -> Vec<Row> {
    let mut dated: Vec<(NaiveDate, Occurrence)> = occurrences
        .iter()
        .map(|o| (agenda_date(o, zone), o.clone()))
        .collect();
    dated.sort_by_key(|(date, o)| (*date, !o.event.all_day, o.start));
    dated
        .into_iter()
        .map(|(date, occurrence)| Row {
            heading: words::full_date_words(date),
            date,
            occurrence,
        })
        .collect()
}

/// The local date a row groups under: an all-day occurrence by its own
/// UTC date, a timed one by local wall time (reconcile.md, "Every task"
/// item 8).
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

        /// Inserts `items` (already sorted, oldest first) before the
        /// model's own first row and recomputes every section boundary,
        /// since the new rows' last date could be the same as what was
        /// the first section's (Task 7). Answers how many rows it
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

/// The widget one occurrence's row draws: a coloured dot, the time (or
/// "All day"), the title, and the place dimmed. A `gtk::Box` subclass so
/// the row factory's bind step can reach its own labels back out, the
/// same reason `ThreadRow` is one (`app/src/ui/mod.rs`).
mod row {
    use super::*;

    mod imp {
        use std::cell::OnceCell;

        use super::*;

        #[derive(Default)]
        pub struct AgendaRow {
            pub dot: OnceCell<gtk::Box>,
            pub time: OnceCell<gtk::Label>,
            pub title: OnceCell<gtk::Label>,
            pub place: OnceCell<gtk::Label>,
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
                let row = self.obj();
                row.set_orientation(gtk::Orientation::Horizontal);
                row.set_spacing(8);
                row.set_margin_top(4);
                row.set_margin_bottom(4);
                row.add_css_class("agenda-row");
                row.set_accessible_role(gtk::AccessibleRole::ListItem);

                let dot = gtk::Box::builder()
                    .valign(gtk::Align::Center)
                    .css_classes(["agenda-dot"])
                    .build();
                let time = gtk::Label::builder()
                    .css_classes(["dim-label", "caption"])
                    .width_chars(5)
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
                    time.upcast_ref(),
                    title.upcast_ref(),
                    place.upcast_ref(),
                ] {
                    row.append(widget);
                }
                let _ = self.dot.set(dot);
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
        /// Fills every label from `row`'s occurrence, using `calendars`
        /// for the dot's colour and the accessible name's calendar name.
        pub(super) fn fill(&self, row: &Row, calendars: &HashMap<(AccountId, String), Calendar>) {
            let imp = self.imp();
            let o = &row.occurrence;
            let (colour, calendar_name) = calendar_of(o, calendars);

            let dot = imp.dot.get().expect("built in constructed");
            dot.set_css_classes(&["agenda-dot", &tint::css_class(colour)]);

            let when = if o.event.all_day {
                gettext("All day")
            } else {
                words::clock_words(o.start, &chrono::Local)
            };
            imp.time
                .get()
                .expect("built in constructed")
                .set_label(&when);
            imp.title
                .get()
                .expect("built in constructed")
                .set_label(&o.event.title);

            let place = imp.place.get().expect("built in constructed");
            place.set_visible(!o.event.place.is_empty());
            place.set_label(&o.event.place);

            let name = mailrs_domain::translate::fill(
                &gettext("{title}, {when}, {calendar}"),
                &[
                    ("title", &o.event.title),
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

type Activated = dyn Fn(&Occurrence);
type ScrolledToTop = dyn Fn();

pub struct Agenda {
    pub widget: gtk::ScrolledWindow,
    model: AgendaModel,
    /// Kept to scroll it after [`Agenda::prepend`] (Task 7): the row
    /// that was first before the insert is asked to stay first.
    list_view: gtk::ListView,
    /// The dim line [`Agenda::show_no_earlier`] reveals once loading has
    /// reached `FIRST_READ_BACK` before today, above the list's own
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
            widget.fill(&row, &bind_calendars.borrow());
        });

        let header_factory = gtk::SignalListItemFactory::new();
        header_factory.connect_setup(move |_, item| {
            let item = item
                .downcast_ref::<gtk::ListHeader>()
                .expect("headers are ListHeaders");
            let label = gtk::Label::builder()
                .css_classes(["agenda-heading", "heading"])
                .xalign(0.0)
                .margin_top(10)
                .margin_bottom(4)
                .build();
            item.set_child(Some(&label));
        });
        header_factory.connect_bind(move |_, item| {
            let item = item
                .downcast_ref::<gtk::ListHeader>()
                .expect("headers are ListHeaders");
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let row = boxed.borrow::<Row>();
            label.set_label(&row.heading);
        });

        let list_view = gtk::ListView::builder()
            .model(&selection)
            .factory(&row_factory)
            .header_factory(&header_factory)
            .single_click_activate(true)
            .build();

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

        // The dim line sits above the list inside the same scrolled
        // content, so it reads as the true top of the agenda rather than
        // a banner that stays on screen once shown.
        let no_earlier = gtk::Label::builder()
            .label(gettext("Nothing earlier on this computer"))
            .css_classes(["dim-label", "caption"])
            .margin_top(10)
            .margin_bottom(10)
            .visible(false)
            .build();
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        content.append(&no_earlier);
        content.append(&list_view);

        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build();
        let scrolled_to_top: Rc<RefCell<Option<Box<ScrolledToTop>>>> = Rc::new(RefCell::new(None));
        let top_slot = Rc::clone(&scrolled_to_top);
        widget.connect_edge_reached(move |_, position| {
            if position == gtk::PositionType::Top
                && let Some(f) = top_slot.borrow().as_ref()
            {
                f();
            }
        });

        Rc::new(Agenda {
            widget,
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
        self.widget.vadjustment().set_value(0.0);
    }

    /// Inserts `occurrences` before the agenda's earliest row and
    /// scrolls so the row that was first stays first: `ListView::scroll_to`
    /// with its new index, once GTK has laid the inserted rows out
    /// (reconcile.md Task 7 item 2). `occurrences` must run entirely
    /// before the earliest date already shown. `calendars` is merged in
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

    /// Reveals the dim line saying the copy holds nothing earlier, once
    /// loading has reached `FIRST_READ_BACK` before today. `show` hides
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
    /// loads earlier days (Task 7).
    pub fn connect_scrolled_to_top(&self, f: impl Fn() + 'static) {
        self.scrolled_to_top.replace(Some(Box::new(f)));
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
