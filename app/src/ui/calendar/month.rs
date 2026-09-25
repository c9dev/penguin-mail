//! `MonthGrid`, the month view: 42 date cells in a fixed 6×7 grid, each
//! holding a few compact [`EventBlock`]s and, once it is crowded, an "N
//! more" button that lists the whole day in a popover, as the spec's
//! Month section asks. The day's own number opens that day in Day view.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Days, NaiveDate};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};

use super::block::{EventBlock, EventKey, key_of};
use super::layout;
use super::range::{Range, ViewKind};
use super::words;

/// Rows a cell keeps for events before folding the rest into "N more",
/// until [`MonthGrid::set_rows`] answers a narrower window's breakpoint.
/// Reconcile.md Task 5 item 8: a fixed rule rather than measuring the
/// grid's own allocation.
const DEFAULT_ROWS: usize = 4;

/// What `show` was last called with, kept so [`MonthGrid::set_rows`] can
/// redraw without the caller handing the same range back.
type Shown = (
    Range,
    Vec<Occurrence>,
    HashMap<(AccountId, String), Calendar>,
);

type DayActivated = dyn Fn(NaiveDate);
type EventActivated = dyn Fn(&MonthGrid, &Occurrence, &gtk::Widget);
type MoreClicked = dyn Fn(&MonthGrid, &[Occurrence], &gtk::Widget);

pub struct MonthGrid {
    pub widget: gtk::Grid,
    cells: Vec<gtk::Box>,
    rows_that_fit: Cell<usize>,
    shown: RefCell<Option<Shown>>,
    day_activated: RefCell<Option<Box<DayActivated>>>,
    event_activated: RefCell<Option<Box<EventActivated>>>,
    more_clicked: RefCell<Option<Box<MoreClicked>>>,
    /// Each block on screen by the event it draws, so the view can point
    /// a popover at one it opens by name. Cleared on every rebuild.
    blocks: RefCell<Vec<(EventKey, gtk::Widget)>>,
}

impl MonthGrid {
    pub fn new() -> Rc<MonthGrid> {
        let widget = gtk::Grid::builder()
            .row_homogeneous(true)
            .column_homogeneous(true)
            .css_classes(["month-grid"])
            .build();
        let mut cells = Vec::with_capacity(42);
        for row in 0..6i32 {
            for column in 0..7i32 {
                let cell = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(2)
                    .css_classes(["month-cell"])
                    .build();
                widget.attach(&cell, column, row, 1, 1);
                cells.push(cell);
            }
        }
        Rc::new(MonthGrid {
            widget,
            cells,
            rows_that_fit: Cell::new(DEFAULT_ROWS),
            shown: RefCell::new(None),
            day_activated: RefCell::new(None),
            event_activated: RefCell::new(None),
            more_clicked: RefCell::new(None),
            blocks: RefCell::new(Vec::new()),
        })
    }

    /// Rebuilds every cell: `range` is the six-week `ViewKind::Month`
    /// range around the month shown; a day outside that month is
    /// dimmed. An all-day occurrence places by its own UTC date, a timed
    /// one by local wall time, matching every other calendar view
    /// (reconcile.md, "Every task" item 8).
    pub fn show(
        self: &Rc<Self>,
        range: Range,
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
    ) {
        self.shown
            .replace(Some((range, occurrences.to_vec(), calendars.clone())));
        self.rebuild();
    }

    /// Answers the window's breakpoint: 3 rows below 720sp window height,
    /// 4 at or above it (reconcile.md Task 5 item 8). Redraws at once
    /// when a month is already shown.
    pub fn set_rows(self: &Rc<Self>, rows: usize) {
        self.rows_that_fit.set(rows.max(2));
        if self.shown.borrow().is_some() {
            self.rebuild();
        }
    }

    /// Runs `f` with the date a day's own number was activated for.
    pub fn connect_day_activated(&self, f: impl Fn(NaiveDate) + 'static) {
        self.day_activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` when a block's own button is clicked, with the widget to
    /// anchor a popover on.
    pub fn connect_event_activated(
        &self,
        f: impl Fn(&MonthGrid, &Occurrence, &gtk::Widget) + 'static,
    ) {
        self.event_activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` when a crowded day's "N more" button is clicked, with
    /// every occurrence of that day and the button to point a popover at.
    pub fn connect_more_clicked(
        &self,
        f: impl Fn(&MonthGrid, &[Occurrence], &gtk::Widget) + 'static,
    ) {
        self.more_clicked.replace(Some(Box::new(f)));
    }

    /// The block drawing `key`, when the month shows it.
    pub fn block_of(&self, key: &EventKey) -> Option<gtk::Widget> {
        self.blocks
            .borrow()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, widget)| widget.clone())
    }

    fn rebuild(self: &Rc<Self>) {
        let Some((range, occurrences, calendars)) = self.shown.borrow().clone() else {
            return;
        };
        let today = chrono::Local::now().date_naive();
        let month = range.month();
        let rows_that_fit = self.rows_that_fit.get();
        let days: Vec<NaiveDate> = (0..42u64).map(|i| range.first + Days::new(i)).collect();
        let mut blocks = Vec::new();

        for (index, &day) in days.iter().enumerate() {
            let cell = &self.cells[index];
            while let Some(child) = cell.first_child() {
                cell.remove(&child);
            }

            let day_button = day_heading(day, today, month);
            connect_day(self, &day_button, day);
            cell.append(&day_button);

            let mut in_day: Vec<&Occurrence> = day_occurrences(&occurrences, day);
            in_day.sort_by_key(|o| (!o.event.all_day, o.start));

            let (shown, hidden) = layout::month_fit(in_day.len(), rows_that_fit);
            for o in &in_day[..shown] {
                let (colour, name) = calendar_of(o, &calendars);
                let block = EventBlock::new(o, colour, name, true, &chrono::Local);
                connect_event(self, &block.widget, (*o).clone());
                cell.append(&block.widget);
                blocks.push((key_of(o), block.widget.upcast()));
            }
            if hidden > 0 {
                let more = gtk::Button::builder()
                    .css_classes(["flat", "month-more"])
                    .label(words::more_count_words(hidden))
                    .halign(gtk::Align::Start)
                    .build();
                crate::ui::name(&more, &words::month_more_words(hidden, day));
                let whole_day: Vec<Occurrence> = in_day.iter().map(|o| (*o).clone()).collect();
                connect_more(self, &more, whole_day);
                cell.append(&more);
            }
        }
        self.blocks.replace(blocks);
    }
}

/// The occurrences that touch `day`: an all-day one by its own UTC date,
/// a timed one by local wall time.
fn day_occurrences(occurrences: &[Occurrence], day: NaiveDate) -> Vec<&Occurrence> {
    let (local_start, local_end) = Range::around(ViewKind::Day, day).span(&chrono::Local);
    let (utc_start, utc_end) = Range::around(ViewKind::Day, day).span(&chrono::Utc);
    occurrences
        .iter()
        .filter(|o| {
            let (start, end): (EpochMillis, EpochMillis) = if o.event.all_day {
                (utc_start, utc_end)
            } else {
                (local_start, local_end)
            };
            o.start < end && o.end > start
        })
        .collect()
}

/// The date button a cell opens its day with: the number visible, the
/// full date spoken (reconcile.md Task 5 item 9), today in the accent
/// pill and a day outside `month` dimmed.
fn day_heading(day: NaiveDate, today: NaiveDate, month: NaiveDate) -> gtk::Button {
    let label = gtk::Label::builder()
        .label(day.day().to_string())
        .css_classes(["date"])
        .build();
    let button = gtk::Button::builder()
        .css_classes(["flat", "day-heading"])
        .halign(gtk::Align::Start)
        .child(&label)
        .build();
    if day == today {
        button.add_css_class("today");
    }
    if day.month() != month.month() || day.year() != month.year() {
        button.add_css_class("dimmed");
    }
    crate::ui::name(&button, &words::full_date_words(day));
    button
}

/// The calendar an occurrence's event names: its own colour and name for
/// the accessible label. Missing from `calendars` only when a caller
/// passes an incomplete map; an empty pair still draws a usable block.
fn calendar_of<'a>(
    o: &Occurrence,
    calendars: &'a HashMap<(AccountId, String), Calendar>,
) -> (&'a str, &'a str) {
    match calendars.get(&(o.account_id, o.event.calendar.clone())) {
        Some(calendar) => (calendar.color.as_str(), calendar.name.as_str()),
        None => ("", ""),
    }
}

fn connect_day(grid: &Rc<MonthGrid>, button: &gtk::Button, day: NaiveDate) {
    let weak = Rc::downgrade(grid);
    button.connect_clicked(move |_| {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.day_activated.borrow().as_ref() {
            f(day);
        }
    });
}

fn connect_more(grid: &Rc<MonthGrid>, button: &gtk::Button, day: Vec<Occurrence>) {
    let weak = Rc::downgrade(grid);
    button.connect_clicked(move |button| {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.more_clicked.borrow().as_ref() {
            f(&grid, &day, button.upcast_ref());
        }
    });
}

fn connect_event(grid: &Rc<MonthGrid>, button: &gtk::Button, occurrence: Occurrence) {
    let weak = Rc::downgrade(grid);
    button.connect_clicked(move |button| {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.event_activated.borrow().as_ref() {
            f(&grid, &occurrence, button.upcast_ref());
        }
    });
}
