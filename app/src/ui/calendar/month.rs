//! `MonthGrid`, the month view: six week rows of seven date cells. An
//! event that runs over several days draws as one bar across the days
//! it covers in each week row, squared where it carries on into the
//! next row; the rest are chips in their day. A busy week takes more
//! height than a quiet one, and a week folds a crowded day into "N
//! more", which lists the whole day in a popover, only once it cannot
//! grow further. The day's own number opens that day in Day view; a
//! click on the rest of the cell opens quick create there.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Days, NaiveDate};
use gtk::{gdk, glib, graphene, gsk};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};

use super::block::{self, EventBlock, EventKey, key_of};
use super::layout;
use super::range::{Range, ViewKind};
use super::time_grid::{focused_key, focused_occurrence};
use super::words;

/// Week rows in a month: enough for any month on any first weekday.
const WEEKS: usize = 6;
/// Space a bar or chip keeps from the day lines, on each end it does
/// not carry on past; the week view's all-day cards keep the same.
const INSET: f32 = 3.0;
/// Space between two lines of events in a week row.
const LINE_GAP: i32 = 2;
/// A line's height before any event has been measured.
const FIRST_LINE: i32 = 20;

/// What `show` was last called with, kept so the grid can redraw once
/// its allocation says how many lines each week row holds.
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
type RowsChanged = dyn Fn(Vec<usize>);

/// Where a bar, a chip or an "N more" button sits: its week row, its
/// columns (`end` exclusive), its line, and which ends carry on.
#[derive(Debug, Clone, Copy)]
struct Place {
    week: usize,
    start: usize,
    end: usize,
    line: usize,
    squared_start: bool,
    squared_end: bool,
}

mod imp {
    use super::*;

    /// The widget [`super::MonthGrid`] draws in: the 42 date cells in
    /// six week rows whose heights [`layout::week_heights`] shares out,
    /// and every bar, chip and "N more" button placed over them.
    #[derive(Default)]
    pub struct MonthBody {
        pub(super) cells: RefCell<Vec<gtk::Widget>>,
        pub(super) items: RefCell<Vec<(gtk::Widget, Place)>>,
        /// Lines of events each week row wants, from the last build.
        pub(super) needs: RefCell<Vec<usize>>,
        /// Lines each week row was built to hold, `None` for a week built
        /// with room for all its events.
        pub(super) built: RefCell<Vec<Option<usize>>>,
        /// The tallest line and date heading measured so far. Kept at
        /// their highest so a rebuild that swaps a chip for an "N more"
        /// button cannot change the heights and ask for another rebuild.
        pub(super) line: Cell<i32>,
        pub(super) heading: Cell<i32>,
        pub(super) rows_changed: RefCell<Option<Box<RowsChanged>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MonthBody {
        const NAME: &'static str = "MailrsMonthBody";
        type Type = super::MonthBody;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MonthBody {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().add_css_class("month-grid");
        }

        fn dispose(&self) {
            for (item, _) in self.items.borrow_mut().drain(..) {
                item.unparent();
            }
            for cell in self.cells.borrow_mut().drain(..) {
                cell.unparent();
            }
        }
    }

    impl WidgetImpl for MonthBody {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (heading, line) = self.metrics();
            match orientation {
                gtk::Orientation::Vertical => {
                    let least = WEEKS as i32 * (heading + line);
                    let wanted: i32 =
                        self.needs.borrow().iter().map(|&n| heading + n.max(1) as i32 * line).sum();
                    (least, wanted.max(least), -1, -1)
                }
                _ => {
                    let mut column = 0;
                    for cell in self.cells.borrow().iter() {
                        column = column.max(cell.measure(gtk::Orientation::Horizontal, -1).0);
                    }
                    for (item, place) in self.items.borrow().iter() {
                        let span = (place.end - place.start).max(1) as i32;
                        let least = item.measure(gtk::Orientation::Horizontal, -1).0 + 2 * INSET as i32;
                        column = column.max((least + span - 1) / span);
                    }
                    (7 * column, 7 * column, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let column_x = |column: usize| (width as f32 * column as f32 / 7.0).round();
            let span_of = |place: &Place| {
                let left = column_x(place.start) + if place.squared_start { 0.0 } else { INSET };
                let right = column_x(place.end) - if place.squared_end { 0.0 } else { INSET };
                (left, (right - left).max(0.0))
            };
            // A block's height depends on its width once its title can
            // wrap, so each is measured at the width it is about to get.
            for (item, place) in self.items.borrow().iter() {
                let (_, w) = span_of(place);
                let least = item.measure(gtk::Orientation::Vertical, w.round() as i32).0;
                self.line.set(self.line.get().max(least + LINE_GAP));
            }
            let (heading, line) = self.metrics();
            let mut needs = self.needs.borrow().clone();
            needs.resize(WEEKS, 0);
            let heights = layout::week_heights(&needs, height, heading, line);
            let tops: Vec<i32> = heights
                .iter()
                .scan(0, |top, &h| {
                    let this = *top;
                    *top += h;
                    Some(this)
                })
                .collect();
            let rtl = self.obj().direction() == gtk::TextDirection::Rtl;
            let mirrored = |x: f32, w: f32| if rtl { width as f32 - x - w } else { x };

            for (index, cell) in self.cells.borrow().iter().enumerate() {
                let (week, column) = (index / 7, index % 7);
                let x = column_x(column);
                let w = column_x(column + 1) - x;
                allocate_at(cell, mirrored(x, w), tops[week] as f32, w, heights[week] as f32, baseline);
            }
            for (item, place) in self.items.borrow().iter() {
                let (left, w) = span_of(place);
                let y = tops[place.week] + heading + place.line as i32 * line;
                allocate_at(item, mirrored(left, w), y as f32, w, (line - LINE_GAP) as f32, baseline);
            }

            let rows: Vec<usize> = heights.iter().map(|&h| layout::week_rows(h, heading, line)).collect();
            let folds: Vec<Option<usize>> =
                rows.iter().zip(&needs).map(|(&r, &n)| (r < n).then_some(r)).collect();
            let changed = folds != *self.built.borrow();
            if changed && let Some(f) = self.rows_changed.borrow().as_ref() {
                f(rows);
            }
        }
    }

    impl MonthBody {
        /// The date heading's height and a line's, each the most measured
        /// so far.
        fn metrics(&self) -> (i32, i32) {
            let mut heading = self.heading.get();
            for cell in self.cells.borrow().iter() {
                heading = heading.max(cell.measure(gtk::Orientation::Vertical, -1).0);
            }
            let mut line = self.line.get().max(FIRST_LINE);
            for (item, _) in self.items.borrow().iter() {
                line = line.max(item.measure(gtk::Orientation::Vertical, -1).0 + LINE_GAP);
            }
            self.heading.set(heading);
            self.line.set(line);
            (heading, line)
        }
    }

    fn allocate_at(child: &gtk::Widget, x: f32, y: f32, w: f32, h: f32, baseline: i32) {
        child.allocate(
            w.round().max(0.0) as i32,
            h.round().max(0.0) as i32,
            baseline,
            Some(gsk::Transform::new().translate(&graphene::Point::new(x, y))),
        );
    }
}

glib::wrapper! {
    pub struct MonthBody(ObjectSubclass<imp::MonthBody>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MonthBody {
    fn new(cells: &[gtk::Box]) -> MonthBody {
        let body: MonthBody = glib::Object::new();
        for cell in cells {
            cell.set_parent(&body);
        }
        body.imp()
            .cells
            .replace(cells.iter().map(|c| c.clone().upcast()).collect());
        body
    }

    fn clear_items(&self) {
        let items: Vec<(gtk::Widget, Place)> = self.imp().items.borrow_mut().drain(..).collect();
        for (item, _) in items {
            item.unparent();
        }
    }

    /// Adds `item` after its own week's cells, so it draws over them and
    /// a screen reader reaches it with that week.
    fn add_item(&self, item: &impl IsA<gtk::Widget>, place: Place) {
        let next_week = self.imp().cells.borrow().get((place.week + 1) * 7).cloned();
        item.as_ref().insert_before(self, next_week.as_ref());
        self.imp().items.borrow_mut().push((item.as_ref().clone(), place));
    }

    fn set_lines(&self, needs: Vec<usize>, built: Vec<Option<usize>>) {
        self.imp().needs.replace(needs);
        self.imp().built.replace(built);
        self.queue_resize();
    }

    /// Runs `f` with the lines each week row holds, whenever an
    /// allocation gives a week fewer or more lines than its last build
    /// folded to.
    fn connect_rows_changed(&self, f: impl Fn(Vec<usize>) + 'static) {
        self.imp().rows_changed.replace(Some(Box::new(f)));
    }
}

pub struct MonthGrid {
    pub widget: MonthBody,
    cells: Vec<gtk::Box>,
    /// Lines each week row held at the last allocation; `None` until the
    /// first, when every week builds with room for all its events.
    rows: RefCell<Option<Vec<usize>>>,
    rebuild_queued: Cell<bool>,
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
    /// a popover at one it opens by name. Cleared on every rebuild. A bar
    /// over two week rows is here twice, the first row first.
    blocks: RefCell<Vec<(EventKey, Occurrence, gtk::Widget)>>,
    /// Each bar, chip and "N more" button with the index of the first
    /// cell it covers, for [`MonthGrid::focused_day`].
    item_days: RefCell<Vec<(gtk::Widget, usize)>>,
    /// The range last shown, kept so [`MonthGrid::day_rect`] and
    /// [`MonthGrid::focused_day`] can find a day among its cells.
    days: RefCell<Vec<NaiveDate>>,
    /// The cell index a press landed on empty background of, from
    /// `press_cell` to `release_cell`.
    pressed_cell: Cell<Option<usize>>,
}

impl MonthGrid {
    pub fn new() -> Rc<MonthGrid> {
        let cells: Vec<gtk::Box> = (0..WEEKS * 7)
            .map(|_| {
                gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .css_classes(["month-cell"])
                    .build()
            })
            .collect();
        let widget = MonthBody::new(&cells);
        let this = Rc::new(MonthGrid {
            widget,
            cells,
            rows: RefCell::new(None),
            rebuild_queued: Cell::new(false),
            shown: RefCell::new(None),
            day_activated: RefCell::new(None),
            day_clicked: RefCell::new(None),
            event_activated: RefCell::new(None),
            event_edited: RefCell::new(None),
            more_clicked: RefCell::new(None),
            blocks: RefCell::new(Vec::new()),
            item_days: RefCell::new(Vec::new()),
            days: RefCell::new(Vec::new()),
            pressed_cell: Cell::new(None),
        });

        // An allocation runs inside GTK's layout pass, where adding and
        // removing children is not allowed, so the grid rebuilds on the
        // next idle instead, once per burst of allocations.
        let weak = Rc::downgrade(&this);
        this.widget.connect_rows_changed(move |rows| {
            let Some(grid) = weak.upgrade() else { return };
            grid.rows.replace(Some(rows));
            if grid.rebuild_queued.replace(true) {
                return;
            }
            let weak = Rc::downgrade(&grid);
            glib::idle_add_local_once(move || {
                if let Some(grid) = weak.upgrade() {
                    grid.rebuild_queued.set(false);
                    grid.rebuild();
                }
            });
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
    /// range around the month shown, starting on the first weekday the
    /// range was built with; a day outside that month is dimmed. An
    /// all-day occurrence places by its own UTC date, a timed one by
    /// local wall time, matching every other calendar view.
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
        let in_cell = self.cells.iter().position(|cell| focus.is_ancestor(cell));
        let index = in_cell.or_else(|| {
            self.item_days
                .borrow()
                .iter()
                .find(|(item, _)| &focus == item || focus.is_ancestor(item))
                .map(|&(_, index)| index)
        })?;
        self.days.borrow().get(index).copied()
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
        let days: Vec<NaiveDate> = (0..(WEEKS * 7) as u64).map(|i| range.first + Days::new(i)).collect();
        self.days.replace(days.clone());

        for (index, &day) in days.iter().enumerate() {
            let cell = &self.cells[index];
            while let Some(child) = cell.first_child() {
                cell.remove(&child);
            }
            cell.set_css_classes(&["month-cell"]);
            if index % 7 == 6 {
                cell.add_css_class("last-column");
            }
            if !working_hours.is_working_day(day.weekday()) {
                cell.add_css_class("shaded");
            }
            let day_button = day_heading(day, today, month);
            connect_day(self, &day_button, day);
            cell.append(&day_button);
        }

        let mut sorted: Vec<&Occurrence> = occurrences.iter().collect();
        sorted.sort_by_key(|o| (!o.event.all_day, o.start));
        let mut weeks: Vec<Vec<(&Occurrence, bool, layout::Segment)>> = vec![Vec::new(); WEEKS];
        for o in sorted {
            let (first, end) = grid_days(o, range.first, &chrono::Local);
            let is_bar = end - first > 1;
            for segment in layout::week_segments(first, end, WEEKS) {
                weeks[segment.week].push((o, is_bar, segment));
            }
        }

        self.widget.clear_items();
        let rows = self.rows.borrow().clone();
        let mut needs = Vec::with_capacity(WEEKS);
        let mut built = Vec::with_capacity(WEEKS);
        let mut blocks = Vec::new();
        let mut item_days = Vec::new();
        for (week, entries) in weeks.iter().enumerate() {
            let spans: Vec<(EpochMillis, EpochMillis)> = entries
                .iter()
                .map(|(_, _, s)| (s.start as EpochMillis, s.end as EpochMillis))
                .collect();
            let (placed, _) = layout::lanes_within(&spans, usize::MAX);
            let lines: Vec<usize> = placed.iter().map(|p| p.lane).collect();
            let need = lines.iter().max().map_or(0, |l| l + 1);
            let fits = rows.as_ref().map_or(need, |rows| rows[week]).max(1);
            needs.push(need);
            built.push((fits < need).then_some(fits));

            let columns: Vec<(usize, usize)> = entries.iter().map(|(_, _, s)| (s.start, s.end)).collect();
            let (shown, more) = layout::fold_week(&columns, &lines, fits);
            for (((o, is_bar, segment), line), shown) in entries.iter().zip(&lines).zip(shown) {
                if !shown {
                    continue;
                }
                let first_cell = week * 7 + segment.start;
                let (colour, name) = calendar_of(o, &calendars);
                let on_edit = edit_closure(self, (*o).clone());
                let block = EventBlock::new(o, colour, name, true, Some(days[first_cell]), &chrono::Local, on_edit);
                if *is_bar {
                    name_bar(&block.widget, o, name);
                    block.widget.add_css_class("month-bar");
                    if segment.squared_start {
                        block.widget.add_css_class("continues-before");
                    }
                    if segment.squared_end {
                        block.widget.add_css_class("continues-after");
                    }
                }
                connect_event(self, &block.widget, (*o).clone());
                let place = Place {
                    week,
                    start: segment.start,
                    end: segment.end,
                    line: *line,
                    squared_start: segment.squared_start,
                    squared_end: segment.squared_end,
                };
                self.widget.add_item(&block.widget, place);
                blocks.push((key_of(o), (*o).clone(), block.widget.clone().upcast()));
                item_days.push((block.widget.upcast(), first_cell));
            }
            for (column, &hidden) in more.iter().enumerate().filter(|(_, hidden)| **hidden > 0) {
                let day = days[week * 7 + column];
                let more = gtk::Button::builder()
                    .css_classes(["flat", "month-more"])
                    // A child label rather than the button's own: GTK names
                    // a button after its own label, over the name below.
                    .child(&gtk::Label::new(Some(&words::more_count_words(hidden))))
                    .halign(gtk::Align::Start)
                    .build();
                crate::ui::name(&more, &words::month_more_words(hidden, day));
                let mut in_day = day_occurrences(&occurrences, day);
                in_day.sort_by_key(|o| (!o.event.all_day, o.start));
                connect_more(self, &more, in_day.into_iter().cloned().collect());
                let place = Place {
                    week,
                    start: column,
                    end: column + 1,
                    line: fits - 1,
                    squared_start: false,
                    squared_end: false,
                };
                self.widget.add_item(&more, place);
                item_days.push((more.upcast(), week * 7 + column));
            }
        }
        self.blocks.replace(blocks);
        self.item_days.replace(item_days);
        self.widget.set_lines(needs, built);
    }
}

/// Names a bar after the whole span of its event, which a bar cut at a
/// week's edge does not show on its own, keeping the description the
/// block gave it.
fn name_bar(button: &gtk::Button, o: &Occurrence, calendar_name: &str) {
    let name = words::bar_words(o, calendar_name, &chrono::Local);
    crate::ui::describe(button, &name, &block::description(&o.event));
    button.set_tooltip_text(Some(&name));
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

/// The days `o` covers, counted from the grid's `first` day, end
/// exclusive: an all-day occurrence by its own UTC dates, a timed one by
/// the wall dates in `zone` of its start and of its last instant, so a
/// meeting that ends at midnight stays on one day.
fn grid_days<Z: chrono::TimeZone>(o: &Occurrence, first: NaiveDate, zone: &Z) -> (i64, i64) {
    let date = |at: EpochMillis, local: bool| {
        let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at).unwrap_or_default();
        if local {
            utc.with_timezone(zone).date_naive()
        } else {
            utc.date_naive()
        }
    };
    let (start, end) = if o.event.all_day {
        let start = date(o.start, false);
        (start, date(o.end, false).max(start + Days::new(1)))
    } else {
        let start = date(o.start, true);
        (start, date(o.end.saturating_sub(1).max(o.start), true) + Days::new(1))
    };
    ((start - first).num_days(), (end - first).num_days())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use mailrs_domain::calendar::Event;

    use super::*;

    fn at(d: u32, h: u32) -> EpochMillis {
        Utc.with_ymd_and_hms(2026, 10, d, h, 0, 0).unwrap().timestamp_millis()
    }

    fn occurrence(all_day: bool, start: EpochMillis, end: EpochMillis) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: Arc::new(Event { all_day, start, end, ..Event::default() }),
            start,
            end,
        }
    }

    fn first() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
    }

    #[test]
    fn an_all_day_event_covers_its_own_utc_dates() {
        // 2 to 4 October, the grid starting on 28 September.
        assert_eq!(grid_days(&occurrence(true, at(2, 0), at(5, 0)), first(), &Utc), (4, 7));
    }

    #[test]
    fn a_timed_event_past_midnight_covers_both_days() {
        assert_eq!(grid_days(&occurrence(false, at(2, 22), at(3, 2)), first(), &Utc), (4, 6));
    }

    #[test]
    fn a_timed_event_ending_at_midnight_stays_on_its_day() {
        assert_eq!(grid_days(&occurrence(false, at(2, 22), at(3, 0)), first(), &Utc), (4, 5));
    }

    #[test]
    fn a_timed_event_of_no_length_still_takes_its_day() {
        assert_eq!(grid_days(&occurrence(false, at(2, 9), at(2, 9)), first(), &Utc), (4, 5));
    }

    #[test]
    fn an_event_from_before_the_grid_counts_back_from_it() {
        assert_eq!(grid_days(&occurrence(true, at(1, 0), at(3, 0)) , NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(), &Utc), (-1, 1));
    }
}
