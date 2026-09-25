//! `TimeGrid`, the day and week view's 24-hour grid: hour lines, a
//! focusable [`EventBlock`] per placed timed occurrence, a "+N" card
//! where a cluster overflows its lanes, and the now-line while today is
//! on screen. `AllDayStrip`, the row above it a window parents outside
//! the `gtk::ScrolledWindow` that holds this grid, draws the all-day
//! occurrences the same days cover; both share [`GUTTER`] so their
//! columns line up.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use adw::prelude::*;
use chrono::{Days, NaiveDate, TimeZone};
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::translate::{date_locale, fill_plural, gettext};
use mailrs_domain::{AccountId, EpochMillis};

use super::block::{self, EventBlock, EventKey};
use super::layout;
use super::range::{Range, ViewKind};

/// Width of the hour-label gutter down the left edge, shared with
/// `AllDayStrip` so the day columns of both widgets line up. The values
/// here are the approved mockup's (`calendar-mockup/mockups.py`).
pub const GUTTER: f32 = 60.0;
/// Height of one hour's row: 08:00 to 20:00 fill a 900-pixel window.
pub const HOUR: f32 = 62.0;
/// How far one all-day lane sits below the one above it.
pub const ALL_DAY_ROW: f32 = 28.0;
/// Height of an all-day card, and the space above the first lane.
const ALL_DAY_CARD: f32 = 24.0;
const ALL_DAY_PAD: f32 = 5.0;
/// Space a card keeps from the hour lines and from its neighbours; also
/// the gap between two cards sharing a lane split.
const CARD_INSET: f32 = 3.0;
const LANE_GAP: f32 = 3.0;

/// Where one card sits in pixels, from `column`'s share of `width`
/// (`GUTTER` plus `columns` equal shares), split into `lanes` at `lane`,
/// running from `top_hours` to `bottom_hours` down the column. Returns a
/// plain tuple rather than `graphene::Rect` so the test below needs no
/// display; `size_allocate` builds the `Rect` from it.
pub fn rect(
    column: usize,
    columns: usize,
    lane: usize,
    lanes: usize,
    top_hours: f64,
    bottom_hours: f64,
    width: f32,
) -> (f32, f32, f32, f32) {
    let columns = columns.max(1) as f32;
    let lanes = lanes.max(1) as f32;
    let column_width = (width - GUTTER) / columns;
    let lane_width = (column_width - 2.0 * CARD_INSET - (lanes - 1.0) * LANE_GAP) / lanes;
    let x =
        GUTTER + column as f32 * column_width + CARD_INSET + lane as f32 * (lane_width + LANE_GAP);
    let y = top_hours as f32 * HOUR + CARD_INSET / 2.0;
    let height = (bottom_hours - top_hours) as f32 * HOUR - CARD_INSET;
    (x, y, lane_width, height)
}

/// Where one all-day card sits: `start_day` to `end_day` (exclusive) of
/// `days` equal shares of `width`, stacked at `lane` when more than one
/// event covers the same day.
fn all_day_rect(
    start_day: usize,
    end_day: usize,
    lane: usize,
    days: usize,
    width: f32,
) -> (f32, f32, f32, f32) {
    let column_width = (width - GUTTER) / days.max(1) as f32;
    let span = (end_day.saturating_sub(start_day)).max(1) as f32;
    let x = GUTTER + start_day as f32 * column_width + CARD_INSET;
    let width = span * column_width - 2.0 * CARD_INSET;
    let y = ALL_DAY_PAD + lane as f32 * ALL_DAY_ROW;
    (x, y, width, ALL_DAY_CARD)
}

/// The strip's height for `rows` lanes: one lane makes the mockup's
/// 34-pixel row.
fn all_day_height(rows: usize) -> f32 {
    let rows = rows.max(1) as f32;
    2.0 * ALL_DAY_PAD + ALL_DAY_CARD + (rows - 1.0) * ALL_DAY_ROW
}

/// `days`' first and last index an all-day occurrence covers, clipped to
/// the days on screen; `None` when it falls entirely outside them.
/// `event.end` is UTC midnight after the last day, so the last day shown
/// is the one before it; both come from the event's own UTC date, never
/// converted to local time (reconcile.md, "Every task" item 8).
fn all_day_span(o: &Occurrence, days: &[NaiveDate]) -> Option<(usize, usize)> {
    let (&first, &last) = (days.first()?, days.last()?);
    let start_date = utc_date(o.start)?;
    let end_date = utc_date(o.end)?.checked_sub_days(Days::new(1))?;
    if end_date < first || start_date > last {
        return None;
    }
    let clipped_start = start_date.max(first);
    let clipped_end = end_date.min(last);
    let start_day = (clipped_start - first).num_days() as usize;
    let end_day = (clipped_end - first).num_days() as usize + 1;
    Some((start_day, end_day))
}

fn utc_date(at: EpochMillis) -> Option<NaiveDate> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at).map(|d| d.date_naive())
}

/// The words `fill_plural`'s "{count} more event(s)" pattern makes of
/// `count`.
fn more_label(count: usize) -> String {
    fill_plural(
        "{count} more event",
        "{count} more events",
        count,
        &[("count", &count.to_string())],
    )
}

/// A plain button for a "+N" overflow card. It carries its own name,
/// since it is a button rather than a labelled card.
fn more_button(count: usize) -> gtk::Button {
    let label = more_label(count);
    let button = gtk::Button::builder()
        .css_classes(["flat"])
        .label(&label)
        .build();
    crate::ui::name(&button, &label);
    button
}

/// The colour of the hour and day lines: the text colour at the strength
/// that gives the mockup's `#ececef` on white and `#2c2c31` on its dark
/// view.
fn hairline(text: &gtk::gdk::RGBA) -> gtk::gdk::RGBA {
    let mut line = *text;
    line.set_alpha(text.alpha() * 0.08);
    line
}

/// "09:00" at `hour`, in the pattern the rest of the app clocks a moment
/// with; the date is a placeholder, only the hour and minute are read.
fn hour_text(hour: u32) -> String {
    chrono::Utc
        .with_ymd_and_hms(1970, 1, 1, hour, 0, 0)
        .single()
        .map(|at| {
            at.format_localized(&gettext("%H:%M"), date_locale())
                .to_string()
        })
        .unwrap_or_default()
}

/// The calendar an occurrence's event names, as its own colour (the
/// event's own colour wins in [`EventBlock`]) and its name for the
/// accessible label. Missing from `calendars` only when a caller passes
/// an incomplete map; an empty pair still draws a usable card.
fn calendar_of<'a>(
    o: &Occurrence,
    calendars: &'a HashMap<(AccountId, String), Calendar>,
) -> (&'a str, &'a str) {
    match calendars.get(&(o.account_id, o.event.calendar.clone())) {
        Some(calendar) => (calendar.color.as_str(), calendar.name.as_str()),
        None => ("", ""),
    }
}

mod imp {
    use super::*;

    /// Where one child of [`super::TimeGrid`] sits, before [`rect`]
    /// turns it into pixels.
    #[derive(Debug, Clone, Copy)]
    pub enum Placement {
        Card {
            column: usize,
            columns: usize,
            lane: usize,
            lanes: usize,
            top: f64,
            bottom: f64,
        },
        Hour(u32),
    }

    type Activated = dyn Fn(&super::TimeGrid, &Occurrence, &gtk::Widget);
    type MoreClicked = dyn Fn(&super::TimeGrid, &[Occurrence], &gtk::Widget);
    type StripActivated = dyn Fn(&super::AllDayStrip, &Occurrence, &gtk::Widget);
    type StripMoreClicked = dyn Fn(&super::AllDayStrip, &[Occurrence], &gtk::Widget);
    /// A strip card's start day, end day (exclusive) and lane.
    type StripPlacement = (usize, usize, usize);

    #[derive(Default)]
    pub struct TimeGrid {
        pub children: RefCell<Vec<(gtk::Widget, Placement)>>,
        pub days: RefCell<Vec<NaiveDate>>,
        pub now: Cell<EpochMillis>,
        pub now_timer: RefCell<Option<glib::SourceId>>,
        /// How far the parent scrolled window has scrolled the grid.
        pub scroll_top: Cell<f64>,
        pub activated: RefCell<Option<Box<Activated>>>,
        pub more_clicked: RefCell<Option<Box<MoreClicked>>>,
        /// Each block by the event it draws, cleared on every `show`.
        pub blocks: RefCell<Vec<(EventKey, gtk::Widget)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TimeGrid {
        const NAME: &'static str = "MailrsTimeGrid";
        type Type = super::TimeGrid;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TimeGrid {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_accessible_role(gtk::AccessibleRole::Group);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_hexpand(true);
        }

        fn dispose(&self) {
            if let Some(source) = self.now_timer.take() {
                source.remove();
            }
            self.blocks.borrow_mut().clear();
            for (child, _) in self.children.borrow_mut().drain(..) {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TimeGrid {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Vertical => {
                    let height = (24.0 * super::HOUR).round() as i32;
                    (height, height, -1, -1)
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, baseline: i32) {
            for (child, placement) in self.children.borrow().iter() {
                let (x, y, w, h) = match *placement {
                    Placement::Card {
                        column,
                        columns,
                        lane,
                        lanes,
                        top,
                        bottom,
                    } => super::rect(column, columns, lane, lanes, top, bottom, width as f32),
                    // Right-aligned 10 pixels short of the gutter's edge and
                    // centred on its hairline, as the mockup sets them.
                    Placement::Hour(hour) => (
                        0.0,
                        hour as f32 * super::HOUR - 8.0,
                        super::GUTTER - 10.0,
                        16.0,
                    ),
                };
                child.allocate(
                    w.round().max(0.0) as i32,
                    h.round().max(0.0) as i32,
                    baseline,
                    Some(gsk::Transform::new().translate(&graphene::Point::new(x, y))),
                );
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let width = widget.width() as f32;
            let height = widget.height() as f32;
            let days = self.days.borrow();
            let columns = days.len().max(1);
            let column_width = (width - super::GUTTER) / columns as f32;

            let hairline = super::hairline(&widget.color());
            let top = self.scroll_top.get() as f32;
            for hour in 0..=24 {
                let y = hour as f32 * super::HOUR;
                // The line at the scrolled window's top edge would double
                // the border above the grid.
                if (y - top).abs() < 1.0 {
                    continue;
                }
                snapshot.append_color(
                    &hairline,
                    &graphene::Rect::new(super::GUTTER, y, width - super::GUTTER, 1.0),
                );
            }
            for day in 1..columns {
                let x = super::GUTTER + day as f32 * column_width;
                snapshot.append_color(&hairline, &graphene::Rect::new(x, 0.0, 1.0, height));
            }

            // Every parented child draws itself, in the order `show`
            // added it.
            self.parent_snapshot(snapshot);

            if let Some((column, y)) = self.now_line(&days) {
                let accent = adw::StyleManager::default().accent_color_rgba();
                let x = super::GUTTER + column as f32 * column_width;
                snapshot.append_color(&accent, &graphene::Rect::new(x, y - 1.0, column_width, 2.0));
                let dot_bounds = graphene::Rect::new(x - 4.5, y - 4.5, 9.0, 9.0);
                let dot = gsk::RoundedRect::from_rect(dot_bounds, 4.5);
                snapshot.push_rounded_clip(&dot);
                snapshot.append_color(&accent, &dot_bounds);
                snapshot.pop();
            }
        }

        fn focus(&self, direction_type: gtk::DirectionType) -> bool {
            let forward = match direction_type {
                gtk::DirectionType::TabForward => true,
                gtk::DirectionType::TabBackward => false,
                other => return self.parent_focus(other),
            };
            step_focus(&self.children.borrow(), forward)
        }

        fn map(&self) {
            self.parent_map();
            let weak = self.obj().downgrade();
            let source = glib::timeout_add_seconds_local(60, move || {
                let Some(grid) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                grid.imp().now.set(chrono::Local::now().timestamp_millis());
                grid.queue_draw();
                glib::ControlFlow::Continue
            });
            self.now_timer.replace(Some(source));
        }

        fn unmap(&self) {
            if let Some(source) = self.now_timer.take() {
                source.remove();
            }
            self.parent_unmap();
        }
    }

    impl TimeGrid {
        /// Today's column and the now-line's y in pixels, when today is
        /// one of `days`.
        fn now_line(&self, days: &[NaiveDate]) -> Option<(usize, f32)> {
            let today = super::utc_date(self.now.get())?
                .and_hms_opt(0, 0, 0)?
                .and_utc()
                .with_timezone(&chrono::Local)
                .date_naive();
            let column = days.iter().position(|&d| d == today)?;
            let midnight = today.and_hms_opt(0, 0, 0)?;
            let hours = layout::wall_offset(self.now.get(), midnight, &chrono::Local);
            Some((column, hours as f32 * super::HOUR))
        }
    }

    #[derive(Default)]
    pub struct AllDayStrip {
        pub children: RefCell<Vec<(gtk::Widget, StripPlacement)>>,
        pub days: Cell<usize>,
        pub rows: Cell<usize>,
        pub activated: RefCell<Option<Box<StripActivated>>>,
        pub more_clicked: RefCell<Option<Box<StripMoreClicked>>>,
        pub blocks: RefCell<Vec<(EventKey, gtk::Widget)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AllDayStrip {
        const NAME: &'static str = "MailrsAllDayStrip";
        type Type = super::AllDayStrip;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for AllDayStrip {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_accessible_role(gtk::AccessibleRole::Group);
            obj.set_hexpand(true);
            obj.add_css_class("all-day-strip");
        }

        fn dispose(&self) {
            self.blocks.borrow_mut().clear();
            for (child, _) in self.children.borrow_mut().drain(..) {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for AllDayStrip {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Vertical => {
                    let height = super::all_day_height(self.rows.get()).round() as i32;
                    (height, height, -1, -1)
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, baseline: i32) {
            let days = self.days.get();
            for (child, placement) in self.children.borrow().iter() {
                let (start_day, end_day, lane) = *placement;
                let (x, y, w, h) =
                    super::all_day_rect(start_day, end_day, lane, days, width as f32);
                child.allocate(
                    w.round().max(0.0) as i32,
                    h.round().max(0.0) as i32,
                    baseline,
                    Some(gsk::Transform::new().translate(&graphene::Point::new(x, y))),
                );
            }
        }

        /// The day lines run on through the all-day row, as in the
        /// mockup, under the cards.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let days = self.days.get().max(1);
            let width = widget.width() as f32;
            let height = widget.height() as f32;
            let column_width = (width - super::GUTTER) / days as f32;
            let hairline = super::hairline(&widget.color());
            for day in 1..days {
                let x = super::GUTTER + day as f32 * column_width;
                snapshot.append_color(&hairline, &graphene::Rect::new(x, 0.0, 1.0, height));
            }
            self.parent_snapshot(snapshot);
        }
    }
}

/// Steps focus by one among `children`'s focusable widgets, in the order
/// `show` added them, which is `(day, start)` order. Returns whether a
/// child took focus; `false` lets Tab leave the grid the way it would
/// leave any single control.
fn step_focus(children: &[(gtk::Widget, imp::Placement)], forward: bool) -> bool {
    let widgets: Vec<&gtk::Widget> = children
        .iter()
        .map(|(w, _)| w)
        .filter(|w| w.can_focus() && w.is_visible())
        .collect();
    if widgets.is_empty() {
        return false;
    }
    let current = widgets.iter().position(|w| w.has_focus());
    let next = match current {
        Some(i) if forward => (i + 1 < widgets.len()).then_some(i + 1),
        Some(i) => i.checked_sub(1),
        None => Some(if forward { 0 } else { widgets.len() - 1 }),
    };
    next.is_some_and(|i| widgets[i].grab_focus())
}

glib::wrapper! {
    pub struct TimeGrid(ObjectSubclass<imp::TimeGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TimeGrid {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TimeGrid {
    pub fn new() -> TimeGrid {
        TimeGrid::default()
    }

    /// Rebuilds every child from the timed occurrences of `occurrences`
    /// clipped to `days`: one [`EventBlock`] per placed occurrence, a
    /// "+N" card per overflow, and the hour labels down the gutter. An
    /// all-day occurrence is left for [`AllDayStrip`].
    pub fn show(
        &self,
        days: &[NaiveDate],
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
        now: EpochMillis,
        zone: &chrono::Local,
    ) {
        let imp = self.imp();
        for (child, _) in imp.children.borrow_mut().drain(..) {
            child.unparent();
        }
        imp.days.replace(days.to_vec());
        imp.now.set(now);

        let bounds: Vec<(EpochMillis, EpochMillis)> = days
            .iter()
            .map(|&day| Range::around(ViewKind::Day, day).span(zone))
            .collect();
        let mut by_day: Vec<Vec<(usize, EpochMillis, EpochMillis)>> = vec![Vec::new(); days.len()];
        for (index, o) in occurrences.iter().enumerate() {
            if o.event.all_day {
                continue;
            }
            for (column, start, end) in layout::clip_to_days(o.start, o.end, &bounds) {
                by_day[column].push((index, start, end));
            }
        }

        let mut children = Vec::new();
        let mut blocks = Vec::new();
        for (column, (&day, pieces)) in days.iter().zip(by_day.iter()).enumerate() {
            let Some(midnight) = day.and_hms_opt(0, 0, 0) else {
                continue;
            };
            let spans: Vec<(EpochMillis, EpochMillis)> =
                pieces.iter().map(|&(_, s, e)| (s, e)).collect();
            let (placed, more) = layout::lanes(&spans);

            let mut in_day: Vec<(gtk::Widget, imp::Placement, f64)> = Vec::new();
            for p in placed {
                let (occ_index, start, end) = pieces[p.index];
                let o = &occurrences[occ_index];
                let (colour, name) = calendar_of(o, calendars);
                let compact = block::is_compact(start, end);
                let card = EventBlock::new(o, colour, name, compact, zone).widget;
                connect_activated(self, &card, o.clone());
                card.set_parent(self);
                blocks.push((block::key_of(o), card.clone().upcast()));
                let top = layout::wall_offset(start, midnight, zone);
                let bottom = layout::wall_offset(end, midnight, zone);
                in_day.push((
                    card.upcast(),
                    imp::Placement::Card {
                        column,
                        columns: days.len(),
                        lane: p.lane,
                        lanes: p.lanes,
                        top,
                        bottom,
                    },
                    top,
                ));
            }
            for group in more {
                let hidden: Vec<Occurrence> = group
                    .hidden
                    .iter()
                    .map(|&i| occurrences[pieces[i].0].clone())
                    .collect();
                let button = more_button(hidden.len());
                connect_more_clicked(self, &button, hidden);
                button.set_parent(self);
                let top = layout::wall_offset(group.from, midnight, zone);
                let bottom = layout::wall_offset(group.to, midnight, zone);
                in_day.push((
                    button.upcast(),
                    imp::Placement::Card {
                        column,
                        columns: days.len(),
                        lane: layout::MOST_LANES - 1,
                        lanes: layout::MOST_LANES,
                        top,
                        bottom,
                    },
                    top,
                ));
            }
            in_day.sort_by(|a, b| a.2.total_cmp(&b.2));
            children.extend(in_day.into_iter().map(|(w, p, _)| (w, p)));
        }

        for hour in 0..24 {
            let label = gtk::Label::builder()
                .label(hour_text(hour))
                .css_classes(["hour-label"])
                .xalign(1.0)
                .build();
            label.set_parent(self);
            children.push((label.upcast(), imp::Placement::Hour(hour)));
        }

        imp.children.replace(children);
        imp.blocks.replace(blocks);
        self.queue_resize();
    }

    /// Runs `f` when a card's own button is clicked, with the widget to
    /// anchor a popover on.
    pub fn connect_event_activated(
        &self,
        f: impl Fn(&TimeGrid, &Occurrence, &gtk::Widget) + 'static,
    ) {
        self.imp().activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` when a "+N" card is clicked, with the occurrences it hid
    /// and the card to point a popover at.
    pub fn connect_more_clicked(
        &self,
        f: impl Fn(&TimeGrid, &[Occurrence], &gtk::Widget) + 'static,
    ) {
        self.imp().more_clicked.replace(Some(Box::new(f)));
    }

    /// The block drawing `key`, when the grid shows it.
    pub fn block_of(&self, key: &EventKey) -> Option<gtk::Widget> {
        find_block(&self.imp().blocks.borrow(), key)
    }

    /// Hides the hour label the scrolled window's top edge would cut in
    /// half, and the hour line along that edge, `top` being how far the
    /// grid is scrolled: at 08:00 the line meets the all-day row's border
    /// and the mockup draws one line and names no hour there.
    pub fn set_scroll_top(&self, top: f64) {
        self.imp().scroll_top.set(top);
        self.queue_draw();
        for (child, placement) in self.imp().children.borrow().iter() {
            if let imp::Placement::Hour(hour) = placement {
                let y = f64::from(*hour) * f64::from(HOUR);
                child.set_child_visible(y - top >= 8.0 || y < top - 8.0);
            }
        }
    }

    /// The y an hour sits at, for the parent `gtk::ScrolledWindow` to
    /// scroll its adjustment to.
    pub fn scroll_to_hour(&self, hour: f64) -> f64 {
        hour * f64::from(HOUR)
    }
}

fn connect_activated(grid: &TimeGrid, card: &gtk::Button, occurrence: Occurrence) {
    let weak = grid.downgrade();
    card.connect_clicked(move |button| {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.imp().activated.borrow().as_ref() {
            f(&grid, &occurrence, button.upcast_ref());
        }
    });
}

fn connect_more_clicked(grid: &TimeGrid, button: &gtk::Button, hidden: Vec<Occurrence>) {
    let weak = grid.downgrade();
    button.connect_clicked(move |button| {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.imp().more_clicked.borrow().as_ref() {
            f(&grid, &hidden, button.upcast_ref());
        }
    });
}

fn find_block(blocks: &[(EventKey, gtk::Widget)], key: &EventKey) -> Option<gtk::Widget> {
    blocks
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, widget)| widget.clone())
}

glib::wrapper! {
    pub struct AllDayStrip(ObjectSubclass<imp::AllDayStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AllDayStrip {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AllDayStrip {
    pub fn new() -> AllDayStrip {
        AllDayStrip::default()
    }

    /// Rebuilds every card from the all-day occurrences of `occurrences`
    /// that fall within `days`, stacked in lanes the way overlapping
    /// timed events are; `TimeGrid` draws the rest.
    pub fn show(
        &self,
        days: &[NaiveDate],
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
    ) {
        let imp = self.imp();
        for (child, _) in imp.children.borrow_mut().drain(..) {
            child.unparent();
        }

        let mut spanning: Vec<(usize, usize, usize)> = Vec::new(); // (occurrence index, start day, end day)
        for (index, o) in occurrences.iter().enumerate() {
            if !o.event.all_day {
                continue;
            }
            if let Some((start, end)) = all_day_span(o, days) {
                spanning.push((index, start, end));
            }
        }
        let spans: Vec<(EpochMillis, EpochMillis)> = spanning
            .iter()
            .map(|&(_, s, e)| (s as EpochMillis, e as EpochMillis))
            .collect();
        let (placed, more) = layout::lanes(&spans);

        let mut children = Vec::new();
        let mut blocks = Vec::new();
        let mut rows = 0usize;
        for p in placed {
            let (occ_index, start, end) = spanning[p.index];
            let o = &occurrences[occ_index];
            let (colour, name) = calendar_of(o, calendars);
            let card = EventBlock::new(o, colour, name, true, &chrono::Local).widget;
            connect_activated_strip(self, &card, o.clone());
            card.set_parent(self);
            blocks.push((block::key_of(o), card.clone().upcast()));
            children.push((card.upcast(), (start, end, p.lane)));
            rows = rows.max(p.lane + 1);
        }
        for group in more {
            let hidden: Vec<Occurrence> = group
                .hidden
                .iter()
                .map(|&i| occurrences[spanning[i].0].clone())
                .collect();
            let start = group.from as usize;
            let end = group.to as usize;
            let lane = layout::MOST_LANES - 1;
            let button = more_button(hidden.len());
            connect_more_clicked_strip(self, &button, hidden);
            button.set_parent(self);
            children.push((button.upcast(), (start, end, lane)));
            rows = rows.max(lane + 1);
        }

        imp.days.set(days.len());
        imp.rows.set(rows);
        imp.children.replace(children);
        imp.blocks.replace(blocks);
        self.queue_resize();
    }

    /// The block drawing `key`, when the strip shows it.
    pub fn block_of(&self, key: &EventKey) -> Option<gtk::Widget> {
        find_block(&self.imp().blocks.borrow(), key)
    }

    pub fn connect_event_activated(
        &self,
        f: impl Fn(&AllDayStrip, &Occurrence, &gtk::Widget) + 'static,
    ) {
        self.imp().activated.replace(Some(Box::new(f)));
    }

    pub fn connect_more_clicked(
        &self,
        f: impl Fn(&AllDayStrip, &[Occurrence], &gtk::Widget) + 'static,
    ) {
        self.imp().more_clicked.replace(Some(Box::new(f)));
    }
}

fn connect_activated_strip(strip: &AllDayStrip, card: &gtk::Button, occurrence: Occurrence) {
    let weak = strip.downgrade();
    card.connect_clicked(move |button| {
        let Some(strip) = weak.upgrade() else { return };
        if let Some(f) = strip.imp().activated.borrow().as_ref() {
            f(&strip, &occurrence, button.upcast_ref());
        }
    });
}

fn connect_more_clicked_strip(strip: &AllDayStrip, button: &gtk::Button, hidden: Vec<Occurrence>) {
    let weak = strip.downgrade();
    button.connect_clicked(move |button| {
        let Some(strip) = weak.upgrade() else { return };
        if let Some(f) = strip.imp().more_clicked.borrow().as_ref() {
            f(&strip, &hidden, button.upcast_ref());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_in_the_second_of_two_lanes_takes_the_right_half_of_its_column() {
        let r = rect(1, 7, 1, 2, 10.0, 11.5, 60.0 + 7.0 * 100.0);
        assert_eq!((r.0, r.2), (211.5, 45.5));
        assert_eq!((r.1, r.3), (621.5, 90.0));
    }

    #[test]
    fn an_all_day_card_spans_its_days_minus_the_inset() {
        let (x, y, w, h) = all_day_rect(1, 3, 0, 7, 60.0 + 7.0 * 100.0);
        assert_eq!((x, w), (163.0, 194.0));
        assert_eq!((y, h), (5.0, 24.0));
    }

    #[test]
    fn a_second_all_day_lane_sits_four_pixels_under_the_first() {
        let (_, y, _, h) = all_day_rect(0, 1, 1, 7, 760.0);
        assert_eq!((y, h), (33.0, 24.0));
        assert_eq!(all_day_height(2), 62.0);
        assert_eq!(all_day_height(1), 34.0);
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn all_day_event(start: EpochMillis, end: EpochMillis) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: std::sync::Arc::new(mailrs_domain::calendar::Event {
                all_day: true,
                start,
                end,
                ..Default::default()
            }),
            start,
            end,
        }
    }

    #[test]
    fn a_one_day_event_spans_only_its_own_column() {
        let days = [d(2026, 9, 21), d(2026, 9, 22), d(2026, 9, 23)];
        let start = Range::around(ViewKind::Day, d(2026, 9, 22))
            .span(&chrono::Utc)
            .0;
        let end = Range::around(ViewKind::Day, d(2026, 9, 22))
            .span(&chrono::Utc)
            .1;
        let o = all_day_event(start, end);
        assert_eq!(all_day_span(&o, &days), Some((1, 2)));
    }

    #[test]
    fn a_two_day_event_spans_both_columns() {
        let days = [d(2026, 9, 21), d(2026, 9, 22), d(2026, 9, 23)];
        let start = Range::around(ViewKind::Day, d(2026, 9, 22))
            .span(&chrono::Utc)
            .0;
        let end = Range::around(ViewKind::Day, d(2026, 9, 23))
            .span(&chrono::Utc)
            .1;
        let o = all_day_event(start, end);
        assert_eq!(all_day_span(&o, &days), Some((1, 3)));
    }

    #[test]
    fn an_event_outside_the_days_shown_spans_none() {
        let days = [d(2026, 9, 21), d(2026, 9, 22)];
        let start = Range::around(ViewKind::Day, d(2026, 9, 30))
            .span(&chrono::Utc)
            .0;
        let end = Range::around(ViewKind::Day, d(2026, 9, 30))
            .span(&chrono::Utc)
            .1;
        let o = all_day_event(start, end);
        assert_eq!(all_day_span(&o, &days), None);
    }

    #[test]
    fn an_hour_label_reads_the_clock() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(hour_text(9), "09:00");
    }

    #[test]
    fn more_events_take_the_plural_they_need() {
        assert_eq!(more_label(1), "1 more event");
        assert_eq!(more_label(3), "3 more events");
    }
}
