//! `TimeGrid`, the day and week view's 24-hour grid: hour lines, a
//! focusable [`EventBlock`] per placed timed occurrence, a "+N" card
//! where a cluster overflows its lanes, and the now-line while today is
//! on screen. `AllDayStrip`, the row above it a window parents outside
//! the `gtk::ScrolledWindow` that holds this grid, draws the all-day
//! occurrences the same days cover; both share [`GUTTER`] so their
//! columns line up.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

use adw::prelude::*;
use chrono::{Days, NaiveDate, TimeZone};
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext};
use mailrs_domain::{AccountId, EpochMillis};

use super::block::{self, EventBlock, EventKey};
use super::drag::{self, Grab, Handle};
use super::layout;
use super::range::{Range, ViewKind};

/// A drag of a card, or across empty time, in progress: the card being
/// dragged (`None` for empty time), where the pointer or the settle
/// spring holds it, and enough of the gesture's own state that `update`
/// and `end` need no widget of their own to read it back from.
pub struct Dragging {
    /// The card being dragged: its widget and the occurrence it draws.
    pub card: Option<(gtk::Widget, Occurrence)>,
    /// `None` for a drag across empty time.
    pub grab: Option<Grab>,
    /// The day column the pointer is over now.
    pub column: usize,
    /// Where the card is drawn, in the grid's own coordinates, while the
    /// pointer or the settle spring holds it.
    pub rect: graphene::Rect,
    /// The press point, for a drag across empty time.
    pub from: EpochMillis,
    /// The gesture's own start point, so `update` and `end`, which the
    /// gesture hands only an offset, can read the pointer's position.
    pub press: (f64, f64),
    /// Pointer samples for the release velocity: `(frame-clock time in
    /// microseconds, y)`, the last four.
    pub samples: VecDeque<(i64, f64)>,
    pub started: bool,
}

/// Width of the hour-label gutter down the left edge, shared with
/// `AllDayStrip` so the day columns of both widgets line up. The values
/// here are the approved mockup's (`calendar-mockup/mockups.py`).
pub const GUTTER: f32 = 60.0;
/// Height of one hour's row: 08:00 to 20:00 fill a 900-pixel window.
pub const HOUR: f32 = 62.0;
/// How far one all-day lane sits below the one above it.
pub const ALL_DAY_ROW: f32 = 28.0;
/// Height of an all-day card, and the space above the first lane and
/// below the last. The mockup puts a card 5 px under the rule above it,
/// which is 4 px into the strip under that 1 px rule, and 34 px from rule
/// to rule, which leaves 5 px below.
const ALL_DAY_CARD: f32 = 24.0;
const ALL_DAY_PAD: f32 = 4.0;
const ALL_DAY_PAD_BELOW: f32 = 5.0;
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

/// How many lines of title fit in a block running from `top` to `bottom`
/// hours: its height less the padding, the title's top margin and the
/// time line, in lines of the 12.5 px title.
fn title_lines(top: f64, bottom: f64) -> i32 {
    const ABOVE_AND_BELOW: f32 = 6.0 + 3.0 + 15.0;
    const TITLE_LINE: f32 = 16.0;
    let height = (bottom - top) as f32 * HOUR - CARD_INSET;
    (((height - ABOVE_AND_BELOW) / TITLE_LINE).floor() as i32).max(1)
}

/// Whether the label of `hour` shows while the grid is scrolled to `top`
/// with `height` of it on screen. The edges of the scrolled window would
/// cut a label on the top line or the bottom line in half, and the
/// mockup names neither, so both go; the bottom one needs its whole
/// height clear, since the card's rounded foot clips it sooner.
fn hour_label_shown(hour: u32, top: f64, height: f64) -> bool {
    let y = f64::from(hour) * f64::from(HOUR);
    let bottom = top + height;
    let clear_of_top = y - top >= 8.0 || y < top - 8.0;
    let clear_of_bottom = bottom - y >= 16.0 || y > bottom + 8.0;
    clear_of_top && clear_of_bottom
}

/// How far to scroll the grid to open at `hour`: one pixel past the
/// hour's line, which the 1 px rule above the grid then stands for, as
/// the mockup draws it.
fn scroll_for_hour(hour: f64) -> f64 {
    hour * f64::from(HOUR) + 1.0
}

/// The strip's height for `rows` lanes: one lane and the rule under it
/// make the mockup's 34-pixel row.
fn all_day_height(rows: usize) -> f32 {
    let rows = rows.max(1) as f32;
    ALL_DAY_PAD + ALL_DAY_CARD + ALL_DAY_PAD_BELOW + (rows - 1.0) * ALL_DAY_ROW
}

/// `days`' first and last index an all-day occurrence covers, clipped to
/// the days on screen; `None` when it falls entirely outside them.
/// `event.end` is UTC midnight after the last day, so the last day shown
/// is the one before it; both come from the event's own UTC date, never
/// converted to local time, which would move them a day west of UTC.
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

/// Today's column among `days` and how many hours past its midnight
/// `now` is, when today is one of them.
fn now_column<Z: TimeZone>(now: EpochMillis, days: &[NaiveDate], zone: &Z) -> Option<(usize, f64)> {
    // Today is the local date of now. The UTC date is a different day
    // for part of every day anywhere east or west of UTC.
    let today = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now)?
        .with_timezone(zone)
        .date_naive();
    let column = days.iter().position(|&d| d == today)?;
    let midnight = today.and_hms_opt(0, 0, 0)?;
    Some((column, layout::wall_offset(now, midnight, zone)))
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

/// Adds the keyboard path a drag has no equivalent for otherwise to a
/// movable card's own accessible description, leaving its name (the
/// day, time and calendar a screen reader needs to tell one card from
/// another) untouched.
fn describe_draggable(card: &impl IsA<gtk::Widget>, o: &Occurrence) {
    let base = block::description(&o.event);
    let hint = gettext(
        "Shift+Up or Shift+Down moves it; Shift+Alt+Up or Shift+Alt+Down changes when it ends.",
    );
    let detail = if base.is_empty() {
        hint
    } else {
        fill(&gettext("{base} {hint}"), &[("base", &base), ("hint", &hint)])
    };
    card.as_ref().update_property(&[gtk::accessible::Property::Description(&detail)]);
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
    type Moved = dyn Fn(&super::TimeGrid, &Occurrence, EpochMillis, EpochMillis);
    type Selected = dyn Fn(EpochMillis, EpochMillis);
    type CanMove = dyn Fn(&Occurrence) -> bool;
    type CarouselInteractive = dyn Fn(bool);
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
        /// The occurrence each block of `blocks` draws, by the same key,
        /// so a drag's press finds the occurrence a `pick` landed on.
        pub occurrences: RefCell<HashMap<EventKey, Occurrence>>,
        /// Each day's span, in the order `days` shows them, from the last
        /// `show`; a drag clamps its card to the one under the pointer.
        pub bounds: RefCell<Vec<(EpochMillis, EpochMillis)>>,
        /// The ghost card of a drag across empty time, one widget per day
        /// it crosses, gone once the drag ends or the ghost is cleared.
        pub ghosts: RefCell<Vec<(gtk::Widget, Placement)>>,
        /// The card being dragged, and where it is drawn now.
        pub dragging: RefCell<Option<super::Dragging>>,
        /// The spring that settles a released card.
        pub settle: RefCell<Option<adw::SpringAnimation>>,
        pub moved: RefCell<Option<Box<Moved>>>,
        pub selected: RefCell<Option<Box<Selected>>>,
        /// Says which occurrences a drag may move; a card whose predicate
        /// answers `false` starts no drag. `None` starts none either,
        /// which is only true before the view has set one.
        pub can_move: RefCell<Option<Box<CanMove>>>,
        /// Turns the ancestor carousel's own swipe off while a drag holds
        /// the grid, so a sideways touch drag does not page the range.
        pub carousel_interactive: RefCell<Option<Box<CarouselInteractive>>>,
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
            // The hour labels never change, so they are built once and
            // outlive every `show`. A label `show` built afresh would come
            // back visible under the all-day row, where `set_scroll_top`
            // had hidden the one before it.
            let mut children = self.children.borrow_mut();
            for hour in 0..24 {
                let label = gtk::Label::builder()
                    .label(super::hour_text(hour))
                    .css_classes(["hour-label"])
                    .xalign(1.0)
                    .build();
                label.set_parent(&*obj);
                children.push((label.upcast(), Placement::Hour(hour)));
            }
        }

        fn dispose(&self) {
            if let Some(source) = self.now_timer.take() {
                source.remove();
            }
            self.blocks.borrow_mut().clear();
            self.occurrences.borrow_mut().clear();
            self.dragging.take();
            self.settle.take();
            for (child, _) in self.ghosts.borrow_mut().drain(..) {
                child.unparent();
            }
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
            let dragged = self
                .dragging
                .borrow()
                .as_ref()
                .and_then(|d| d.card.as_ref().map(|(widget, _)| widget.clone()));
            for (child, placement) in self.children.borrow().iter() {
                if dragged.as_ref() == Some(child) {
                    // Placed below from `dragging.rect`, not its own slot.
                    continue;
                }
                let (x, y, w, h) = placement_pixels(*placement, width as f32);
                allocate_at(child, x, y, w, h, baseline);
            }
            for (child, placement) in self.ghosts.borrow().iter() {
                let (x, y, w, h) = placement_pixels(*placement, width as f32);
                allocate_at(child, x, y, w, h, baseline);
            }
            if let Some(dragging) = self.dragging.borrow().as_ref()
                && let Some((widget, _)) = &dragging.card
            {
                let r = dragging.rect;
                allocate_at(widget, r.x(), r.y(), r.width(), r.height(), baseline);
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
            let (column, hours) = super::now_column(self.now.get(), days, &chrono::Local)?;
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

/// A placement's pixel rect, `size_allocate`'s own match pulled out so
/// the dragged card's allocation and the ghost's can share it.
fn placement_pixels(placement: imp::Placement, width: f32) -> (f32, f32, f32, f32) {
    match placement {
        imp::Placement::Card { column, columns, lane, lanes, top, bottom } => {
            rect(column, columns, lane, lanes, top, bottom, width)
        }
        // Right-aligned 10 pixels short of the gutter's edge and centred
        // on its hairline, as the mockup sets them.
        imp::Placement::Hour(hour) => (0.0, hour as f32 * HOUR - 8.0, GUTTER - 10.0, 16.0),
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
        let grid = TimeGrid::default();
        // `Capture` so this sees a press on a card before the card's own
        // click gesture does; it claims the sequence only once the
        // pointer has travelled past GTK's own drag threshold, so a
        // press that goes no further stays a click for the card.
        let drag = gtk::GestureDrag::builder()
            .button(gdk::BUTTON_PRIMARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak = grid.downgrade();
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(grid) = weak.upgrade() {
                grid.begin(x, y);
            }
        });
        let weak = grid.downgrade();
        drag.connect_drag_update(move |gesture, dx, dy| {
            let Some(grid) = weak.upgrade() else { return };
            let threshold = gtk::Settings::default().map_or(8, |s| s.gtk_dnd_drag_threshold()) as f64;
            if grid.update(dx, dy, threshold) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        let weak = grid.downgrade();
        drag.connect_drag_end(move |_, dx, dy| {
            if let Some(grid) = weak.upgrade() {
                grid.end(dx, dy);
            }
        });
        grid.add_controller(drag);

        // The keyboard path a mouse drag has no equivalent for
        // otherwise: Shift+Up and Shift+Down move the focused card by a
        // quarter hour, Shift+Alt+Up and Shift+Alt+Down change when it
        // ends. Bubble phase, since neither chord is a button's own key
        // binding, so the card's Enter and Space still activate it.
        let nudge_keys = gtk::EventControllerKey::new();
        let weak = grid.downgrade();
        nudge_keys.connect_key_pressed(move |_, keyval, _, state| {
            weak.upgrade().map_or(glib::Propagation::Proceed, |grid| grid.nudge_focused(keyval, state))
        });
        grid.add_controller(nudge_keys);
        grid
    }

    /// Rebuilds the cards from the timed occurrences of `occurrences`
    /// clipped to `days`: one [`EventBlock`] per placed occurrence and a
    /// "+N" card per overflow. The hour labels stay. An all-day
    /// occurrence is left for [`AllDayStrip`].
    pub fn show(
        &self,
        days: &[NaiveDate],
        occurrences: &[Occurrence],
        calendars: &HashMap<(AccountId, String), Calendar>,
        now: EpochMillis,
        zone: &chrono::Local,
    ) {
        let imp = self.imp();
        imp.days.replace(days.to_vec());
        imp.now.set(now);

        let bounds: Vec<(EpochMillis, EpochMillis)> = days
            .iter()
            .map(|&day| Range::around(ViewKind::Day, day).span(zone))
            .collect();
        imp.bounds.replace(bounds.clone());
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
        let mut occurrence_of = HashMap::new();
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
                let top = layout::wall_offset(start, midnight, zone);
                let bottom = layout::wall_offset(end, midnight, zone);
                let named_day = (days.len() > 1).then_some(day);
                let event_block = EventBlock::new(o, colour, name, compact, named_day, zone);
                event_block.set_title_lines(title_lines(top, bottom));
                let card = event_block.widget;
                connect_activated(self, &card, o.clone());
                card.set_parent(self);
                if imp.can_move.borrow().as_ref().is_some_and(|f| f(o)) {
                    describe_draggable(&card, o);
                }
                blocks.push((block::key_of(o), card.clone().upcast()));
                occurrence_of.insert(block::key_of(o), o.clone());
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

        let removed = swap_cards(&mut imp.children.borrow_mut(), children);
        for child in removed {
            child.unparent();
        }
        imp.blocks.replace(blocks);
        imp.occurrences.replace(occurrence_of);
        // A reload rebuilds every card, so a drag or a settle in flight
        // would otherwise hold a widget that just lost its parent.
        imp.settle.take();
        if let Some(dragging) = imp.dragging.take()
            && let Some(f) = imp.carousel_interactive.borrow().as_ref()
            && dragging.card.is_some()
        {
            f(true);
        }
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

    /// The first card Tab reaches, in day and start order.
    pub fn first_block(&self) -> Option<gtk::Widget> {
        self.imp()
            .children
            .borrow()
            .iter()
            .find(|(w, p)| matches!(p, imp::Placement::Card { .. }) && w.can_focus())
            .map(|(w, _)| w.clone())
    }

    /// The event whose block has the keyboard focus.
    pub fn focused_key(&self) -> Option<EventKey> {
        focused_key(&self.imp().blocks.borrow())
    }

    /// Hides the hour labels [`hour_label_shown`] leaves out, and the
    /// hour line along the top edge, `top` being how far the grid is
    /// scrolled and `height` how much of it shows: at 08:00 the line
    /// meets the all-day row's border and the mockup draws one line and
    /// names no hour there.
    pub fn set_view(&self, top: f64, height: f64) {
        self.imp().scroll_top.set(top);
        self.queue_draw();
        for (child, placement) in self.imp().children.borrow().iter() {
            if let imp::Placement::Hour(hour) = placement {
                child.set_child_visible(hour_label_shown(*hour, top, height));
            }
        }
    }

    /// The y an hour sits at, for the parent `gtk::ScrolledWindow` to
    /// scroll its adjustment to.
    pub fn scroll_to_hour(&self, hour: f64) -> f64 {
        scroll_for_hour(hour)
    }

    // ---- Dragging --------------------------------------------------

    /// Runs `f` when a drag of a card ends at a new span, settled: the
    /// grid it happened on (E10; a page holds this weakly across the
    /// repeat question and acts on it only while still on screen), the
    /// occurrence, and its new start and end.
    pub fn connect_moved(
        &self,
        f: impl Fn(&TimeGrid, &Occurrence, EpochMillis, EpochMillis) + 'static,
    ) {
        self.imp().moved.replace(Some(Box::new(f)));
    }

    /// Runs `f` when a drag across empty time ends, with its span.
    pub fn connect_selected(&self, f: impl Fn(EpochMillis, EpochMillis) + 'static) {
        self.imp().selected.replace(Some(Box::new(f)));
    }

    /// Says which occurrences a drag may move: a card whose predicate
    /// answers `false`, such as one on a read-only calendar or a guest's
    /// own event (R9), starts no drag.
    pub fn set_can_move(&self, f: impl Fn(&Occurrence) -> bool + 'static) {
        self.imp().can_move.replace(Some(Box::new(f)));
    }

    /// Runs `f(false)` once a drag of a card holds the grid and `f(true)`
    /// once it lets go, so the view can turn the ancestor carousel's own
    /// swipe off: it allows touch drags, and a sideways one across a
    /// card would otherwise page the range under the drag.
    pub fn connect_carousel_interactive(&self, f: impl Fn(bool) + 'static) {
        self.imp().carousel_interactive.replace(Some(Box::new(f)));
    }

    /// Shows a ghost card for a drag across empty time, replacing any
    /// shown before; `None` clears it. Task 8's quick create keeps it up
    /// until its popover closes.
    pub fn show_ghost(&self, span: Option<(EpochMillis, EpochMillis)>) {
        let imp = self.imp();
        for (widget, _) in imp.ghosts.borrow_mut().drain(..) {
            widget.unparent();
        }
        let Some((start, end)) = span else {
            self.queue_allocate();
            return;
        };
        let days = imp.days.borrow();
        let bounds = imp.bounds.borrow();
        let mut ghosts = Vec::new();
        for (column, day_start, day_end) in layout::clip_to_days(start, end, &bounds) {
            let Some(midnight) = days.get(column).and_then(|d| d.and_hms_opt(0, 0, 0)) else {
                continue;
            };
            let top = layout::wall_offset(day_start, midnight, &chrono::Local);
            let bottom = layout::wall_offset(day_end, midnight, &chrono::Local);
            let widget: gtk::Widget = gtk::Box::builder()
                .css_classes(["event-block", "ghost"])
                .build()
                .upcast();
            widget.set_parent(self);
            ghosts.push((
                widget,
                imp::Placement::Card { column, columns: days.len(), lane: 0, lanes: 1, top, bottom },
            ));
        }
        drop(days);
        drop(bounds);
        imp.ghosts.replace(ghosts);
        self.queue_allocate();
    }

    /// Returns the last dragged card to its own place, with no write, for
    /// a cancelled repeat question.
    pub fn spring_back(&self) {
        let imp = self.imp();
        let Some((widget, from)) = imp
            .dragging
            .borrow()
            .as_ref()
            .and_then(|d| d.card.as_ref().map(|(w, _)| w.clone()).zip(Some(d.rect)))
        else {
            return;
        };
        let Some(to) = self.placement_rect_of(&widget) else {
            self.finish_settle();
            return;
        };
        self.settle_to(from, to, 0.0, {
            let weak = self.downgrade();
            move || {
                if let Some(grid) = weak.upgrade() {
                    grid.finish_settle();
                }
            }
        });
    }

    /// The occurrence a `pick` at `(x, y)` lands on, and its widget:
    /// walks up from the picked widget to the card it belongs to, the
    /// way a click on a label or the bar inside it still finds the card.
    fn pick_card(&self, x: f64, y: f64) -> Option<(gtk::Widget, Occurrence)> {
        let root: gtk::Widget = self.clone().upcast();
        let mut current = self.pick(x, y, gtk::PickFlags::DEFAULT)?;
        loop {
            let found = self
                .imp()
                .blocks
                .borrow()
                .iter()
                .find(|(_, w)| *w == current)
                .map(|(key, w)| (key.clone(), w.clone()));
            if let Some((key, widget)) = found {
                let o = self.imp().occurrences.borrow().get(&key)?.clone();
                return Some((widget, o));
            }
            if current == root {
                return None;
            }
            current = current.parent()?;
        }
    }

    /// The day column under `x`, clamped to the days shown.
    fn column_at(&self, x: f64) -> usize {
        let days = self.imp().days.borrow().len().max(1);
        let column_width = (f64::from(self.width() as f32) - f64::from(GUTTER)) / days as f64;
        let column = if column_width > 0.0 { ((x - f64::from(GUTTER)) / column_width).floor() } else { 0.0 };
        column.clamp(0.0, days as f64 - 1.0) as usize
    }

    /// The instant `column`'s day and `y` (hours down its midnight) name.
    fn time_at(&self, column: usize, y: f64) -> EpochMillis {
        let days = self.imp().days.borrow();
        match days.get(column) {
            Some(&day) => layout::instant_at(day, y / f64::from(HOUR), &chrono::Local),
            None => 0,
        }
    }

    /// `column`'s own span, for `Grab::settle` to keep a card inside it.
    fn day_span(&self, column: usize) -> (EpochMillis, EpochMillis) {
        const DAY: EpochMillis = 24 * 3_600_000;
        self.imp().bounds.borrow().get(column).copied().unwrap_or((0, DAY))
    }

    /// `widget`'s own rect from its last `show`, ignoring a drag: where a
    /// card belongs when nothing is holding it.
    fn placement_rect_of(&self, widget: &gtk::Widget) -> Option<graphene::Rect> {
        let width = self.width() as f32;
        self.imp().children.borrow().iter().find(|(w, _)| w == widget).map(|(_, placement)| {
            let (x, y, w, h) = placement_pixels(*placement, width);
            graphene::Rect::new(x, y, w, h)
        })
    }

    /// `widget`'s own column, lane and the lanes it shares, from its last
    /// `show`, so a drag keeps its width as it moves.
    fn lane_of(&self, widget: &gtk::Widget) -> (usize, usize, usize) {
        self.imp()
            .children
            .borrow()
            .iter()
            .find_map(|(w, p)| match (w == widget, p) {
                (true, imp::Placement::Card { columns, lane, lanes, .. }) => {
                    Some((*columns, *lane, *lanes))
                }
                _ => None,
            })
            .unwrap_or((1, 0, 1))
    }

    /// The press: picks the card under `(x, y)`, when the view allows a
    /// drag of it, or records the point for a drag across empty time.
    fn begin(&self, x: f64, y: f64) {
        let imp = self.imp();
        if let Some(spring) = imp.settle.borrow_mut().take() {
            spring.pause();
        }
        let column = self.column_at(x);
        if let Some((widget, o)) = self.pick_card(x, y) {
            let allowed = imp.can_move.borrow().as_ref().is_some_and(|f| f(&o));
            if !allowed {
                imp.dragging.replace(None);
                return;
            }
            let rect = widget
                .compute_bounds(self)
                .unwrap_or_else(|| graphene::Rect::new(0.0, 0.0, 0.0, 0.0));
            let handle = drag::handle_at(y - f64::from(rect.y()), f64::from(rect.height()));
            let grab = Grab::new(handle, o.start, o.end, self.time_at(column, y));
            imp.dragging.replace(Some(Dragging {
                card: Some((widget, o)),
                grab: Some(grab),
                column,
                rect,
                from: 0,
                press: (x, y),
                samples: VecDeque::new(),
                started: false,
            }));
        } else if x >= f64::from(GUTTER) && y >= 0.0 {
            imp.dragging.replace(Some(Dragging {
                card: None,
                grab: None,
                column,
                rect: graphene::Rect::new(0.0, 0.0, 0.0, 0.0),
                from: self.time_at(column, y),
                press: (x, y),
                samples: VecDeque::new(),
                started: false,
            }));
        } else {
            imp.dragging.replace(None);
        }
    }

    /// The pointer moved by `(dx, dy)` from the press. Returns whether
    /// the drag has started, past GTK's own `threshold`, so the gesture
    /// can claim the sequence once it has.
    fn update(&self, dx: f64, dy: f64, threshold: f64) -> bool {
        let imp = self.imp();
        let already_started = match imp.dragging.borrow().as_ref() {
            None => return false,
            Some(state) => state.started,
        };
        if !already_started && dx.hypot(dy) < threshold {
            return false;
        }
        if !already_started {
            let (card, is_end) = {
                let mut dragging = imp.dragging.borrow_mut();
                let state = dragging.as_mut().expect("checked above");
                state.started = true;
                (state.card.clone(), state.grab.map(|g| g.handle) == Some(Handle::End))
            };
            if let Some((widget, _)) = &card {
                widget.add_css_class("dragging");
            }
            self.set_cursor_from_name(Some(if is_end { "ns-resize" } else { "grabbing" }));
            if card.is_some()
                && let Some(f) = imp.carousel_interactive.borrow().as_ref()
            {
                f(false);
            }
        }

        let (press, card, grab, from) = {
            let dragging = imp.dragging.borrow();
            let state = dragging.as_ref().expect("checked above");
            (state.press, state.card.clone(), state.grab, state.from)
        };
        let (x, y) = (press.0 + dx, press.1 + dy);
        let column = self.column_at(x);
        let now = self.frame_clock().map_or(0, |c| c.frame_time());

        match (&card, grab) {
            (Some((widget, _)), Some(grab)) => {
                let (start, end) = grab.follow(self.time_at(column, y));
                let (columns, lane, lanes) = self.lane_of(widget);
                let days = imp.days.borrow();
                let Some(&day) = days.get(column) else { return true };
                let Some(midnight) = day.and_hms_opt(0, 0, 0) else { return true };
                drop(days);
                let top = layout::wall_offset(start, midnight, &chrono::Local);
                let bottom = layout::wall_offset(end, midnight, &chrono::Local);
                let width = self.width() as f32;
                let (rx, ry, rw, rh) = rect(column, columns, lane, lanes, top, bottom, width);
                let day_height = 24.0 * f64::from(HOUR);
                let banded_y = drag::banded(f64::from(ry), f64::from(rh), day_height) as f32;
                if let Some(state) = imp.dragging.borrow_mut().as_mut() {
                    state.rect = graphene::Rect::new(rx, banded_y, rw, rh);
                    state.column = column;
                    state.samples.push_back((now, y));
                    if state.samples.len() > 4 {
                        state.samples.pop_front();
                    }
                }
            }
            _ => {
                self.show_ghost(Some(drag::selection(from, self.time_at(column, y))));
                if let Some(state) = imp.dragging.borrow_mut().as_mut() {
                    state.column = column;
                    state.samples.push_back((now, y));
                    if state.samples.len() > 4 {
                        state.samples.pop_front();
                    }
                }
            }
        }
        self.queue_allocate();
        true
    }

    /// The release. A drag that never started leaves the card's own
    /// click to open the popover, as before; one that did settles the
    /// card on the spring, or emits `selected` for empty time.
    fn end(&self, dx: f64, dy: f64) {
        let imp = self.imp();
        let taken = imp.dragging.borrow().as_ref().map(|d| {
            (d.started, d.card.clone(), d.grab, d.from, d.press, d.rect, d.samples.clone())
        });
        let Some((started, card, grab, from, press, current_rect, samples)) = taken else {
            return;
        };
        if !started {
            imp.dragging.replace(None);
            return;
        }
        let (x, y) = (press.0 + dx, press.1 + dy);
        let column = self.column_at(x);
        let velocity_y = drag::velocity(&samples);
        match card {
            Some((widget, occurrence)) => {
                let Some(grab) = grab else {
                    self.finish_settle();
                    return;
                };
                let day = self.day_span(column);
                let (start, end) = grab.settle(self.time_at(column, y), day);
                let Some(target) = self.card_target_rect(&widget, column, start, end) else {
                    self.finish_settle();
                    return;
                };
                let unchanged = start == occurrence.start && end == occurrence.end;
                let weak = self.downgrade();
                self.settle_to(current_rect, target, velocity_y, move || {
                    let Some(grid) = weak.upgrade() else { return };
                    grid.finish_settle();
                    if !unchanged
                        && let Some(f) = grid.imp().moved.borrow().as_ref()
                    {
                        f(&grid, &occurrence, start, end);
                    }
                });
            }
            None => {
                imp.dragging.replace(None);
                let selection = drag::selection(from, self.time_at(column, y));
                if let Some(f) = imp.selected.borrow().as_ref() {
                    f(selection.0, selection.1);
                }
            }
        }
    }

    /// The rect a card settles to at `start` to `end` in `column`,
    /// keeping the lane it drew in before the drag.
    fn card_target_rect(
        &self,
        widget: &gtk::Widget,
        column: usize,
        start: EpochMillis,
        end: EpochMillis,
    ) -> Option<graphene::Rect> {
        let (columns, lane, lanes) = self.lane_of(widget);
        let day = *self.imp().days.borrow().get(column)?;
        let midnight = day.and_hms_opt(0, 0, 0)?;
        let top = layout::wall_offset(start, midnight, &chrono::Local);
        let bottom = layout::wall_offset(end, midnight, &chrono::Local);
        let width = self.width() as f32;
        let (x, y, w, h) = rect(column, columns, lane, lanes, top, bottom, width);
        Some(graphene::Rect::new(x, y, w, h))
    }

    /// Runs the settle spring from `from` to `to`, `velocity_y` (pixels
    /// per second) carrying a flick's release into it; `done` runs once
    /// it lands, or at once when animations are off.
    fn settle_to(&self, from: graphene::Rect, to: graphene::Rect, velocity_y: f64, done: impl Fn() + 'static) {
        let distance = f64::from(to.y() - from.y());
        let initial = if distance.abs() > 0.5 { velocity_y / distance } else { 0.0 };
        let weak = self.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |progress| {
            let Some(grid) = weak.upgrade() else { return };
            let t = progress as f32;
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            if let Some(dragging) = grid.imp().dragging.borrow_mut().as_mut() {
                dragging.rect = graphene::Rect::new(
                    lerp(from.x(), to.x()),
                    lerp(from.y(), to.y()),
                    lerp(from.width(), to.width()),
                    lerp(from.height(), to.height()),
                );
            }
            grid.queue_allocate();
        });
        // Damping ratio 1.0: the card lands without passing its slot.
        // `AdwAnimation` follows GNOME's animations setting on its own
        // (R10): with it off, `play` ends the spring at once and `done`
        // below runs straight away, no cross-fade shown.
        let spring = adw::SpringAnimation::builder()
            .widget(self)
            .value_from(0.0)
            .value_to(1.0)
            .spring_params(&adw::SpringParams::new(1.0, 1.0, 400.0))
            .initial_velocity(initial)
            .clamp(false)
            .target(&target)
            .build();
        spring.connect_done(move |_| done());
        spring.play();
        self.imp().settle.replace(Some(spring));
    }

    /// Clears the drag once its settle has landed: the class, the
    /// cursor, and the carousel's own swipe.
    fn finish_settle(&self) {
        let imp = self.imp();
        let had_card = imp
            .dragging
            .take()
            .inspect(|d| {
                if let Some((widget, _)) = &d.card {
                    widget.remove_css_class("dragging");
                }
            })
            .is_some_and(|d| d.card.is_some());
        imp.settle.take();
        self.set_cursor_from_name(None);
        if had_card
            && let Some(f) = imp.carousel_interactive.borrow().as_ref()
        {
            f(true);
        }
    }

    /// The keyboard path beside the drag: Shift+Up or Shift+Down moves
    /// the focused card by a quarter hour, Shift+Alt+Up or
    /// Shift+Alt+Down changes when it ends. Neither writes anything the
    /// card's own predicate refuses.
    fn nudge_focused(&self, keyval: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if !state.contains(gdk::ModifierType::SHIFT_MASK) {
            return glib::Propagation::Proceed;
        }
        let steps = match keyval {
            gdk::Key::Up => -1,
            gdk::Key::Down => 1,
            _ => return glib::Propagation::Proceed,
        };
        let imp = self.imp();
        let Some(o) = self
            .focused_key()
            .and_then(|key| imp.occurrences.borrow().get(&key).cloned())
        else {
            return glib::Propagation::Proceed;
        };
        if !imp.can_move.borrow().as_ref().is_some_and(|f| f(&o)) {
            return glib::Propagation::Proceed;
        }
        let (start, end) = if state.contains(gdk::ModifierType::ALT_MASK) {
            drag::stretch(o.start, o.end, steps)
        } else {
            drag::nudge(o.start, o.end, steps)
        };
        if let Some(f) = imp.moved.borrow().as_ref() {
            f(self, &o, start, end);
        }
        glib::Propagation::Stop
    }
}

/// Puts `cards` in place of the cards among `children`, ahead of the
/// hour labels, which stay as they are. Returns the cards it took out,
/// for the caller to unparent.
fn swap_cards<W>(children: &mut Vec<(W, imp::Placement)>, cards: Vec<(W, imp::Placement)>) -> Vec<W> {
    let (hours, old): (Vec<_>, Vec<_>) = std::mem::take(children)
        .into_iter()
        .partition(|(_, placement)| matches!(placement, imp::Placement::Hour(_)));
    children.extend(cards);
    children.extend(hours);
    old.into_iter().map(|(widget, _)| widget).collect()
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

/// The event whose block among `blocks` has the keyboard focus.
pub fn focused_key(blocks: &[(EventKey, gtk::Widget)]) -> Option<EventKey> {
    blocks
        .iter()
        .find(|(_, widget)| widget.has_focus())
        .map(|(key, _)| key.clone())
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
            let named_day = (days.len() > 1).then(|| days[start]);
            let card = EventBlock::new(o, colour, name, true, named_day, &chrono::Local).widget;
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

    /// The first card Tab reaches.
    pub fn first_block(&self) -> Option<gtk::Widget> {
        self.imp().children.borrow().first().map(|(w, _)| w.clone())
    }

    /// The event whose block has the keyboard focus.
    pub fn focused_key(&self) -> Option<EventKey> {
        focused_key(&self.imp().blocks.borrow())
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
        assert_eq!((y, h), (4.0, 24.0));
    }

    #[test]
    fn a_second_all_day_lane_sits_four_pixels_under_the_first() {
        let (_, y, _, h) = all_day_rect(0, 1, 1, 7, 760.0);
        assert_eq!((y, h), (32.0, 24.0));
        assert_eq!(all_day_height(2), 61.0);
    }

    #[test]
    fn one_all_day_lane_and_its_rule_make_the_mockup_s_34_pixel_row() {
        // The rule under the row is a separate 1 px widget.
        assert_eq!(all_day_height(1) + 1.0, 34.0);
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

    fn at<Z: TimeZone>(zone: &Z, y: i32, m: u32, day: u32, h: u32, min: u32) -> EpochMillis {
        zone.with_ymd_and_hms(y, m, day, h, min, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn the_now_line_stays_in_today_s_column_before_utc_reaches_today() {
        // 08:00 in Tokyo is 23:00 the day before in UTC.
        let tokyo = chrono_tz::Asia::Tokyo;
        let days = [d(2026, 9, 24), d(2026, 9, 25), d(2026, 9, 26)];
        let now = at(&tokyo, 2026, 9, 25, 8, 0);
        assert_eq!(now_column(now, &days, &tokyo), Some((1, 8.0)));
    }

    #[test]
    fn the_now_line_in_lisbon_just_after_midnight_is_near_the_top() {
        let lisbon = chrono_tz::Europe::Lisbon;
        let days = [d(2026, 9, 24), d(2026, 9, 25)];
        let now = at(&lisbon, 2026, 9, 25, 0, 30);
        assert_eq!(now_column(now, &days, &lisbon), Some((1, 0.5)));
    }

    fn card(column: usize) -> imp::Placement {
        imp::Placement::Card {
            column,
            columns: 7,
            lane: 0,
            lanes: 1,
            top: 9.0,
            bottom: 10.0,
        }
    }

    #[test]
    fn a_new_show_replaces_the_cards_and_keeps_the_hour_labels() {
        let mut children = vec![("old", card(0)), ("08:00", imp::Placement::Hour(8))];
        let removed = swap_cards(&mut children, vec![("new", card(1))]);
        assert_eq!(removed, vec!["old"]);
        let names: Vec<&str> = children.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, vec!["new", "08:00"]);
    }

    #[test]
    fn an_hour_long_block_has_room_for_two_lines_of_title() {
        assert_eq!(title_lines(10.0, 11.0), 2);
    }

    #[test]
    fn a_ninety_minute_block_lets_sprint_planning_wrap() {
        assert!(title_lines(10.0, 11.5) >= 2);
    }

    #[test]
    fn a_short_block_keeps_its_title_on_one_line() {
        assert_eq!(title_lines(9.0, 9.75), 1);
    }

    #[test]
    fn the_first_hour_line_sits_under_the_all_day_rule() {
        // The mockup draws 08:00 on the rule under the all-day row, so
        // 09:00 comes one hour, not one hour and a pixel, below it.
        assert_eq!(scroll_for_hour(8.0), 8.0 * f64::from(HOUR) + 1.0);
    }

    #[test]
    fn no_hour_is_named_on_the_line_under_the_all_day_row() {
        let top = scroll_for_hour(8.0);
        assert!(!hour_label_shown(8, top, 751.0));
        assert!(hour_label_shown(9, top, 751.0));
    }

    #[test]
    fn no_hour_is_named_on_the_line_the_card_ends_on() {
        // 751 px from 08:00 shows down to 20:00, whose line meets the
        // card's foot; the mockup names 19:00 last.
        let top = scroll_for_hour(8.0);
        assert!(hour_label_shown(19, top, 751.0));
        assert!(!hour_label_shown(20, top, 751.0));
    }

    #[test]
    fn more_events_take_the_plural_they_need() {
        assert_eq!(more_label(1), "1 more event");
        assert_eq!(more_label(3), "3 more events");
    }
}
