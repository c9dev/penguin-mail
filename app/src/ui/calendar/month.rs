//! `MonthGrid`, the month view: 42 date cells in a fixed 6×7 grid, each
//! holding a few compact [`EventBlock`]s and, once it is crowded, an "N
//! more" button that lists the whole day in a popover. The day's own
//! number opens that day in Day view; a click on the rest of the cell
//! opens quick create there.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Days, NaiveDate};
use gtk::gdk;
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};

use super::block::{EventBlock, EventKey, key_of};
use super::layout;
use super::range::{Range, ViewKind};
use super::time_grid::{focused_key, focused_occurrence};
use super::words;

/// Rows a cell keeps for events before folding the rest into "N more",
/// until [`MonthGrid::set_rows`] answers a shorter window's breakpoint:
/// a fixed rule, since measuring the grid's own allocation to decide
/// what it holds would lay it out twice.
const DEFAULT_ROWS: usize = 4;

/// What `show` was last called with, kept so [`MonthGrid::set_rows`] can
/// redraw without the caller handing the same range back.
type Shown = (
    Range,
    Vec<Occurrence>,
    HashMap<(AccountId, String), Calendar>,
    mailrs_domain::calendar::hours::WorkingHours,
);

type DayActivated = dyn Fn(NaiveDate);
type DayClicked = dyn Fn(NaiveDate);
type EventActivated = dyn Fn(&MonthGrid, &Occurrence, &gtk::Widget);
type EventEdited = dyn Fn(&MonthGrid, &Occurrence);
type MoreClicked = dyn Fn(&MonthGrid, &[Occurrence], &gtk::Widget);

pub struct MonthGrid {
    pub widget: gtk::Grid,
    cells: Vec<gtk::Box>,
    rows_that_fit: Cell<usize>,
    shown: RefCell<Option<Shown>>,
    day_activated: RefCell<Option<Box<DayActivated>>>,
    /// Runs when a press on a cell's own empty background releases
    /// without becoming a drag: quick create opens on its day, the way
    /// N opens it on the focused one.
    day_clicked: RefCell<Option<Box<DayClicked>>>,
    event_activated: RefCell<Option<Box<EventActivated>>>,
    /// Runs on a block's double click or Enter, over the popover a
    /// single click or Space opens.
    event_edited: RefCell<Option<Box<EventEdited>>>,
    more_clicked: RefCell<Option<Box<MoreClicked>>>,
    /// Each block on screen by the event it draws, so the view can point
    /// a popover at one it opens by name. Cleared on every rebuild.
    blocks: RefCell<Vec<(EventKey, Occurrence, gtk::Widget)>>,
    /// The range last shown, kept so [`MonthGrid::day_rect`] and
    /// [`MonthGrid::focused_day`] can find a day among its cells.
    days: RefCell<Vec<NaiveDate>>,
    /// The cell index a press landed on empty background of, from
    /// `press_cell` to `release_cell`.
    pressed_cell: Cell<Option<usize>>,
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
        let this = Rc::new(MonthGrid {
            widget,
            cells,
            rows_that_fit: Cell::new(DEFAULT_ROWS),
            shown: RefCell::new(None),
            day_activated: RefCell::new(None),
            day_clicked: RefCell::new(None),
            event_activated: RefCell::new(None),
            event_edited: RefCell::new(None),
            more_clicked: RefCell::new(None),
            blocks: RefCell::new(Vec::new()),
            days: RefCell::new(Vec::new()),
            pressed_cell: Cell::new(None),
        });

        // A press on a cell's own background remembers the cell, for a
        // release that never became a drag to open quick create on; a
        // press on the day heading, an event or "N more" leaves it
        // alone, since each already answers its own click. Capture
        // phase, the same reason the time grid's own drag takes it: this
        // never claims the sequence, so the swipe between months is
        // still free to.
        let drag = gtk::GestureDrag::builder()
            .button(gdk::BUTTON_PRIMARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak = Rc::downgrade(&this);
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(grid) = weak.upgrade() {
                grid.press_cell(x, y);
            }
        });
        let weak = Rc::downgrade(&this);
        drag.connect_drag_end(move |_, dx, dy| {
            let Some(grid) = weak.upgrade() else { return };
            let threshold = gtk::Settings::default().map_or(8, |s| s.gtk_dnd_drag_threshold()) as f64;
            grid.release_cell(dx, dy, threshold);
        });
        this.widget.add_controller(drag);
        this
    }

    /// Rebuilds every cell: `range` is the six-week `ViewKind::Month`
    /// range around the month shown; a day outside that month is
    /// dimmed. An all-day occurrence places by its own UTC date, a timed
    /// one by local wall time, matching every other calendar view.
    pub fn show(
        self: &Rc<Self>,
        range: Range,
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
        working_hours: mailrs_domain::calendar::hours::WorkingHours,
    ) {
        self.shown
            .replace(Some((range, occurrences.to_vec(), calendars.clone(), working_hours)));
        self.rebuild();
    }

    /// Answers the window's breakpoint: 3 rows below 720sp window height,
    /// 4 at or above it. Redraws at once
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

    /// Runs `f` with the date of a click on a cell's own empty
    /// background: not its heading, an event, or "N more", each of
    /// which answers its own click instead.
    pub fn connect_day_clicked(&self, f: impl Fn(NaiveDate) + 'static) {
        self.day_clicked.replace(Some(Box::new(f)));
    }

    /// Remembers which cell, if any, a press landed on its own empty
    /// background, for `release_cell` to open quick create on if the
    /// press never becomes a drag.
    fn press_cell(&self, x: f64, y: f64) {
        let picked = self.widget.pick(x, y, gtk::PickFlags::DEFAULT);
        self.pressed_cell.set(
            self.cells
                .iter()
                .position(|cell| picked.as_ref().is_some_and(|w| w == cell.upcast_ref::<gtk::Widget>())),
        );
    }

    /// The release after `press_cell`. Under `threshold`, the press
    /// counts as a click and opens quick create on its cell's day; past
    /// it, the press was a drag, such as the swipe between months, which
    /// this leaves alone.
    fn release_cell(&self, dx: f64, dy: f64, threshold: f64) {
        let Some(index) = self.pressed_cell.take() else { return };
        if !super::drag::is_click(dx, dy, threshold) {
            return;
        }
        let Some(&day) = self.days.borrow().get(index) else { return };
        if let Some(f) = self.day_clicked.borrow().as_ref() {
            f(day);
        }
    }

    /// Runs `f` when a block's own button is clicked, with the widget to
    /// anchor a popover on.
    pub fn connect_event_activated(
        &self,
        f: impl Fn(&MonthGrid, &Occurrence, &gtk::Widget) + 'static,
    ) {
        self.event_activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` on a block's double click or Enter, which opens the
    /// editor over the popover a single click or Space opens.
    pub fn connect_event_edited(&self, f: impl Fn(&MonthGrid, &Occurrence) + 'static) {
        self.event_edited.replace(Some(Box::new(f)));
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
            .find(|(k, _, _)| k == key)
            .map(|(_, _, widget)| widget.clone())
    }

    /// The block drawing the occurrence of `key` that starts at `start`.
    pub fn block_at(&self, key: &EventKey, start: EpochMillis) -> Option<gtk::Widget> {
        super::time_grid::block_at(&self.blocks.borrow(), key, start)
    }

    /// The first event block in day order, and the event of the block
    /// that has the keyboard focus, for the view to put the focus back
    /// after it rebuilds the month.
    pub fn first_block(&self) -> Option<gtk::Widget> {
        self.blocks.borrow().first().map(|(_, _, widget)| widget.clone())
    }

    pub fn focused_key(&self) -> Option<EventKey> {
        focused_key(&self.blocks.borrow())
    }

    /// The occurrence whose block has the keyboard focus.
    pub fn focused(&self) -> Option<Occurrence> {
        focused_occurrence(&self.blocks.borrow())
    }

    /// The date of the day heading or the card that has the keyboard
    /// focus, for N to start a new event on.
    pub fn focused_day(&self) -> Option<NaiveDate> {
        let focus = self.widget.root().and_then(|root| root.focus())?;
        self.cells
            .iter()
            .position(|cell| focus.is_ancestor(cell))
            .and_then(|index| self.days.borrow().get(index).copied())
    }

    /// The widget itself, for quick create to point its popover at.
    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone().upcast()
    }

    /// The cell's own bounds in the grid's coordinates, for quick
    /// create's popover to point at. `None` when `day` is not among the
    /// six weeks the month last showed.
    pub fn day_rect(&self, day: NaiveDate) -> Option<gdk::Rectangle> {
        let index = self.days.borrow().iter().position(|&d| d == day)?;
        let bounds = self.cells.get(index)?.compute_bounds(&self.widget)?;
        Some(gdk::Rectangle::new(
            bounds.x().round() as i32,
            bounds.y().round() as i32,
            bounds.width().round().max(1.0) as i32,
            bounds.height().round().max(1.0) as i32,
        ))
    }

    fn rebuild(self: &Rc<Self>) {
        let Some((range, occurrences, calendars, working_hours)) = self.shown.borrow().clone() else {
            return;
        };
        let today = chrono::Local::now().date_naive();
        let month = range.month();
        let rows_that_fit = self.rows_that_fit.get();
        let days: Vec<NaiveDate> = (0..42u64).map(|i| range.first + Days::new(i)).collect();
        self.days.replace(days.clone());
        let mut blocks = Vec::new();

        for (index, &day) in days.iter().enumerate() {
            let cell = &self.cells[index];
            while let Some(child) = cell.first_child() {
                cell.remove(&child);
            }
            cell.set_css_classes(&["month-cell"]);
            if !working_hours.is_working_day(day.weekday()) {
                cell.add_css_class("shaded");
            }

            let day_button = day_heading(day, today, month);
            connect_day(self, &day_button, day);
            cell.append(&day_button);

            let mut in_day: Vec<&Occurrence> = day_occurrences(&occurrences, day);
            in_day.sort_by_key(|o| (!o.event.all_day, o.start));

            let (shown, hidden) = layout::month_fit(in_day.len(), rows_that_fit);
            for o in &in_day[..shown] {
                let (colour, name) = calendar_of(o, &calendars);
                let on_edit = edit_closure(self, (*o).clone());
                let block = EventBlock::new(o, colour, name, true, Some(day), &chrono::Local, on_edit);
                connect_event(self, &block.widget, (*o).clone());
                cell.append(&block.widget);
                blocks.push((key_of(o), (*o).clone(), block.widget.clone().upcast()));
            }
            if hidden > 0 {
                let more = gtk::Button::builder()
                    .css_classes(["flat", "month-more"])
                    // A child label rather than the button's own: GTK names
                    // a button after its own label, over the name below.
                    .child(&gtk::Label::new(Some(&words::more_count_words(hidden))))
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
/// full date spoken, since "24" alone says nothing, today in the accent
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

/// The closure a block's double click or Enter runs, which reports
/// `occurrence` through [`MonthGrid::connect_event_edited`].
fn edit_closure(grid: &Rc<MonthGrid>, occurrence: Occurrence) -> Rc<dyn Fn()> {
    let weak = Rc::downgrade(grid);
    Rc::new(move || {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.event_edited.borrow().as_ref() {
            f(&grid, &occurrence);
        }
    })
}
