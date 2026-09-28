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
use chrono::{Datelike, Days, NaiveDate, TimeZone};
use chrono_tz::Tz;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::translate::{fill, fill_plural, gettext};
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
    /// The day column the pointer is over now, for a card, which may
    /// move to another day. Fixed at the column the press landed in for
    /// a drag across empty time, which never leaves it.
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

/// A run of keyboard nudges on one card, moved on screen and not yet
/// asked about.
pub struct Nudging {
    pub card: gtk::Widget,
    /// The occurrence as it was before the first press.
    pub occurrence: Occurrence,
    pub run: drag::Nudges,
    /// Where the card sat before the first press, for a Cancel.
    pub placement: imp::Placement,
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
/// The shortest a timed event's card ever draws, however briefly the
/// event itself runs, so a five-minute meeting still keeps its title
/// readable: the same height stage 2 gave an all-day card
/// ([`ALL_DAY_CARD`]), which the mockup already treats as legible.
const MIN_HEIGHT: f32 = ALL_DAY_CARD;

/// Where one card sits in pixels, from `column`'s share of `width`
/// (`GUTTER` plus `columns` equal shares), split into `lanes` at `lane`,
/// running from `top_hours` to `bottom_hours` down the column. The
/// height never draws under [`MIN_HEIGHT`]: the card's top stays at its
/// event's real start, so a short event only runs further down than it
/// truly does, never up past it. Returns a plain tuple rather than
/// `graphene::Rect` so the test below needs no display; `size_allocate`
/// builds the `Rect` from it.
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
    let height = ((bottom_hours - top_hours) as f32 * HOUR - CARD_INSET).max(MIN_HEIGHT);
    (x, y, lane_width, height)
}

/// The milliseconds [`MIN_HEIGHT`] pixels are at `hour_px` pixels an
/// hour, with `card_inset` trimmed off each card: how long an event
/// must run before its card is tall enough to draw at its own length
/// rather than the minimum. Two events shorter than this, close enough
/// together, would draw over each other once both stretch to the
/// minimum; [`stretch_for_lanes`] uses it to give them separate lanes
/// instead.
fn min_duration(min_height: f32, card_inset: f32, hour_px: f32) -> EpochMillis {
    let hours = f64::from((min_height + card_inset) / hour_px);
    (hours * 3_600_000.0).round() as EpochMillis
}

/// `spans`, each stretched to run at least `min` from its own start:
/// not what the cards draw (their real length still decides that; see
/// [`rect`]'s own floor), but what lane assignment reasons about, so two
/// short events close enough together that their minimum-height cards
/// would touch take separate lanes rather than sharing one and drawing
/// on top of each other.
fn stretch_for_lanes(
    spans: &[(EpochMillis, EpochMillis)],
    min: EpochMillis,
) -> Vec<(EpochMillis, EpochMillis)> {
    spans.iter().map(|&(start, end)| (start, end.max(start + min))).collect()
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

/// The wash over an hour outside working hours: the same text colour a
/// wash elsewhere in the app uses (`.assistant-step title:hover` in
/// `style.css`), light enough on both a light and a dark surface that it
/// reads as a shade rather than a fill, since the mockup itself shades
/// nothing here.
fn shade(text: &gtk::gdk::RGBA) -> gtk::gdk::RGBA {
    let mut wash = *text;
    wash.set_alpha(text.alpha() * 0.05);
    wash
}

/// "09:00" or "9:00 AM" at `hour`, in the clock
/// [`crate::clock_format::current`] names.
fn hour_text(hour: u32) -> String {
    chrono::NaiveTime::from_hms_opt(hour, 0, 0)
        .map(crate::clock_format::time_text)
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
    type Edited = dyn Fn(&super::TimeGrid, &Occurrence);
    type MoreClicked = dyn Fn(&super::TimeGrid, &[Occurrence], &gtk::Widget);
    type StripActivated = dyn Fn(&super::AllDayStrip, &Occurrence, &gtk::Widget);
    type StripEdited = dyn Fn(&super::AllDayStrip, &Occurrence);
    type StripMoreClicked = dyn Fn(&super::AllDayStrip, &[Occurrence], &gtk::Widget);
    type Moved = dyn Fn(&super::TimeGrid, &Occurrence, EpochMillis, EpochMillis);
    type Selected = dyn Fn(EpochMillis, EpochMillis);
    type CanMove = dyn Fn(&Occurrence) -> bool;
    type CanSelect = dyn Fn() -> bool;
    type CarouselInteractive = dyn Fn(bool);
    /// A strip card's start day, end day (exclusive) and lane.
    type StripPlacement = (usize, usize, usize);

    #[derive(Default)]
    pub struct TimeGrid {
        pub children: RefCell<Vec<(gtk::Widget, Placement)>>,
        pub days: RefCell<Vec<NaiveDate>>,
        /// The hours to shade outside of, from the last `show`.
        pub working_hours: Cell<mailrs_domain::calendar::hours::WorkingHours>,
        pub now: Cell<EpochMillis>,
        pub now_timer: RefCell<Option<glib::SourceId>>,
        /// How far the parent scrolled window has scrolled the grid.
        pub scroll_top: Cell<f64>,
        pub activated: RefCell<Option<Box<Activated>>>,
        /// Runs on a double click or Enter on a card, over the popover a
        /// single click or Space opens.
        pub edited: RefCell<Option<Box<Edited>>>,
        pub more_clicked: RefCell<Option<Box<MoreClicked>>>,
        /// Each block by the event it draws, cleared on every `show`.
        pub blocks: RefCell<Vec<(EventKey, Occurrence, gtk::Widget)>>,
        /// The time last clicked on empty grid, snapped to a quarter
        /// hour, for the slot the New Event button and N start from.
        pub cursor: Cell<Option<EpochMillis>>,
        /// The span a drag across empty time or N marks while quick
        /// create's popover is open, and the widget drawing it.
        pub ghost: RefCell<Option<(gtk::Widget, (EpochMillis, EpochMillis))>>,
        /// Each day's span, in the order `days` shows them, from the last
        /// `show`; a drag clamps its card to the one under the pointer.
        pub bounds: RefCell<Vec<(EpochMillis, EpochMillis)>>,
        /// The card being dragged, and where it is drawn now.
        pub dragging: RefCell<Option<super::Dragging>>,
        /// The spring that settles a released card.
        pub settle: RefCell<Option<adw::SpringAnimation>>,
        pub moved: RefCell<Option<Box<Moved>>>,
        /// Keyboard nudges waiting for the keyboard to rest.
        pub nudging: RefCell<Option<super::Nudging>>,
        pub nudge_timer: RefCell<Option<glib::SourceId>>,
        /// A nudged card the view is asking about, and where
        /// [`super::TimeGrid::spring_back`] returns it on a Cancel.
        pub nudged: RefCell<Option<(gtk::Widget, Placement)>>,
        pub selected: RefCell<Option<Box<Selected>>>,
        /// Says which occurrences a drag may move; a card whose predicate
        /// answers `false` starts no drag. `None` starts none either,
        /// which is only true before the view has set one.
        pub can_move: RefCell<Option<Box<CanMove>>>,
        /// Says whether a drag across empty time may start: with no
        /// calendar to take a new event, it would mark a span nothing
        /// opens for.
        pub can_select: RefCell<Option<Box<CanSelect>>>,
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
            drop(children);
            // A press on empty time remembers where, for the New Event
            // button and N; a press on a card leaves it alone, since the
            // grid is not the widget the pick lands on then.
            let click = gtk::GestureClick::new();
            click.set_button(gdk::BUTTON_PRIMARY);
            let weak = obj.downgrade();
            click.connect_pressed(move |_, _, x, y| {
                if let Some(grid) = weak.upgrade() {
                    grid.note_cursor(x, y);
                }
            });
            obj.add_controller(click);
        }

        fn dispose(&self) {
            if let Some(source) = self.now_timer.take() {
                source.remove();
            }
            if let Some(source) = self.nudge_timer.take() {
                source.remove();
            }
            self.nudging.take();
            self.nudged.take();
            self.blocks.borrow_mut().clear();
            self.dragging.take();
            self.settle.take();
            self.ghost.take();
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

            let shade = super::shade(&widget.color());
            let working_hours = self.working_hours.get();
            for (column, &day) in days.iter().enumerate() {
                let x = super::GUTTER + column as f32 * column_width;
                for (start, end) in working_hours.shaded_hour_ranges(day.weekday()) {
                    let y = start as f32 * super::HOUR;
                    let range_height = (end - start) as f32 * super::HOUR;
                    snapshot.append_color(&shade, &graphene::Rect::new(x, y, column_width, range_height));
                }
            }

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

            // Every child draws in the order `show` added it, but a card
            // being dragged draws last, so it passes over the cards it
            // crosses rather than under the ones added after it.
            let raised = self
                .dragging
                .borrow()
                .as_ref()
                .and_then(|d| d.card.as_ref().map(|(w, _)| w.clone()));
            let mut children = Vec::new();
            let mut child = widget.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                children.push(c);
            }
            for child in super::draw_order(&children, raised.as_ref()) {
                widget.snapshot_child(&child, snapshot);
            }

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

        /// The column holding `start`'s local day, and `start` and `end`
        /// as wall-clock hours from that day's midnight. `None` when
        /// `start` falls on a day the grid does not show.
        pub(super) fn span_placement(&self, start: EpochMillis, end: EpochMillis) -> Option<(usize, f64, f64)> {
            let days = self.days.borrow();
            let day = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(start)?
                .with_timezone(&chrono::Local)
                .date_naive();
            let column = days.iter().position(|&d| d == day)?;
            let midnight = day.and_hms_opt(0, 0, 0)?;
            let top = super::layout::wall_offset(start, midnight, &chrono::Local);
            let bottom = super::layout::wall_offset(end, midnight, &chrono::Local);
            Some((column, top, bottom))
        }
    }

    #[derive(Default)]
    pub struct AllDayStrip {
        pub children: RefCell<Vec<(gtk::Widget, StripPlacement)>>,
        pub days: Cell<usize>,
        pub rows: Cell<usize>,
        pub activated: RefCell<Option<Box<StripActivated>>>,
        pub edited: RefCell<Option<Box<StripEdited>>>,
        pub more_clicked: RefCell<Option<Box<StripMoreClicked>>>,
        pub blocks: RefCell<Vec<(EventKey, Occurrence, gtk::Widget)>>,
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
        working_hours: mailrs_domain::calendar::hours::WorkingHours,
    ) {
        let imp = self.imp();
        imp.days.replace(days.to_vec());
        imp.now.set(now);
        imp.working_hours.set(working_hours);

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
        for (column, (&day, pieces)) in days.iter().zip(by_day.iter()).enumerate() {
            let Some(midnight) = day.and_hms_opt(0, 0, 0) else {
                continue;
            };
            let spans: Vec<(EpochMillis, EpochMillis)> =
                pieces.iter().map(|&(_, s, e)| (s, e)).collect();
            let min = min_duration(MIN_HEIGHT, CARD_INSET, HOUR);
            let (placed, more) = layout::lanes(&stretch_for_lanes(&spans, min));

            let mut in_day: Vec<(gtk::Widget, imp::Placement, f64)> = Vec::new();
            for p in placed {
                let (occ_index, start, end) = pieces[p.index];
                let o = &occurrences[occ_index];
                let (colour, name) = calendar_of(o, calendars);
                let compact = block::is_compact(start, end);
                let top = layout::wall_offset(start, midnight, zone);
                let bottom = layout::wall_offset(end, midnight, zone);
                let named_day = (days.len() > 1).then_some(day);
                let on_edit = edit_closure(self, o.clone());
                let event_block = EventBlock::new(o, colour, name, compact, named_day, zone, on_edit);
                event_block.set_title_lines(title_lines(top, bottom));
                let card = event_block.widget;
                connect_activated(self, &card, o.clone());
                card.set_parent(self);
                if imp.can_move.borrow().as_ref().is_some_and(|f| f(o)) {
                    describe_draggable(&card, o);
                }
                blocks.push((block::key_of(o), o.clone(), card.clone().upcast()));
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
        // A reload rebuilds every card, so a drag or a settle in flight
        // would otherwise hold a widget that just lost its parent.
        imp.settle.take();
        if let Some(dragging) = imp.dragging.take()
            && let Some(f) = imp.carousel_interactive.borrow().as_ref()
            && dragging.card.is_some()
        {
            f(true);
        }
        // A refill silently dropped the ghost among the removed cards;
        // put it back in the same span, unless the span has left the
        // days now shown.
        if let Some((_, span)) = imp.ghost.borrow().clone() {
            self.show_ghost(Some(span));
        }
        // The old cards are gone. A card being asked about comes back from
        // the copy; one still being nudged moves to its new widget, so a
        // sync landing between two presses does not undo them.
        imp.nudged.take();
        self.carry_nudges();
        self.queue_resize();
    }

    /// The time last clicked on empty grid, for the New Event button and
    /// N to start a new event at.
    pub fn cursor(&self) -> Option<EpochMillis> {
        self.imp().cursor.get()
    }

    /// The occurrence whose block has the keyboard focus.
    pub fn focused(&self) -> Option<Occurrence> {
        focused_occurrence(&self.imp().blocks.borrow())
    }

    /// Where `start` to `end` sits in the grid's own coordinates, for
    /// quick create's popover to point at: the column holding `start`,
    /// as a card in lane 0 of 1. `None` when `start` falls outside the
    /// days shown.
    pub fn slot_rect(&self, start: EpochMillis, end: EpochMillis) -> Option<gdk::Rectangle> {
        let width = self.width();
        if width <= 0 {
            return None;
        }
        let (column, top, bottom) = self.imp().span_placement(start, end)?;
        let days = self.imp().days.borrow().len();
        let (x, y, w, h) = rect(column, days, 0, 1, top, bottom, width as f32);
        Some(gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            w.round().max(1.0) as i32,
            h.round().max(1.0) as i32,
        ))
    }

    /// Marks (or clears) the span a drag across empty time or N covers
    /// with a dashed ghost card, while quick create's popover is open.
    pub fn show_ghost(&self, span: Option<(EpochMillis, EpochMillis)>) {
        let imp = self.imp();
        if let Some((widget, _)) = imp.ghost.take() {
            imp.children.borrow_mut().retain(|(w, _)| w != &widget);
            // `show`'s own refill may already have unparented it among
            // the cards it swapped out.
            if widget.parent().is_some() {
                widget.unparent();
            }
        }
        let Some(span) = span else {
            self.queue_resize();
            return;
        };
        let Some((column, top, bottom)) = imp.span_placement(span.0, span.1) else {
            return;
        };
        let ghost = gtk::Box::builder()
            .css_classes(["event-block", "ghost"])
            .can_target(false)
            .build();
        let widget: gtk::Widget = ghost.upcast();
        widget.set_parent(self);
        imp.children.borrow_mut().push((
            widget.clone(),
            imp::Placement::Card {
                column,
                columns: imp.days.borrow().len(),
                lane: 0,
                lanes: 1,
                top,
                bottom,
            },
        ));
        imp.ghost.replace(Some((widget, span)));
        self.queue_resize();
    }

    /// A press at `x`, `y` on empty time, outside every card: the time it
    /// falls on, snapped to a whole quarter hour. Left alone when the
    /// press landed on a card, which the pick then answers with rather
    /// than the grid itself.
    fn note_cursor(&self, x: f64, y: f64) {
        if self
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .is_some_and(|picked| picked.upcast_ref::<gtk::Widget>() != self.upcast_ref::<gtk::Widget>())
        {
            return;
        }
        let imp = self.imp();
        let days = imp.days.borrow();
        let width = self.width() as f32;
        let columns = days.len().max(1);
        let column_width = (width - GUTTER) / columns as f32;
        if column_width <= 0.0 || days.is_empty() {
            return;
        }
        let column = (((x as f32 - GUTTER) / column_width).floor() as i64)
            .clamp(0, days.len() as i64 - 1) as usize;
        let day = days[column];
        drop(days);
        let hours = y / f64::from(HOUR);
        let at = layout::instant_at(day, hours, &chrono::Local);
        imp.cursor.set(Some(drag::selection(at, at).0));
    }

    /// Runs `f` when a card's own button is clicked, with the widget to
    /// anchor a popover on.
    pub fn connect_event_activated(
        &self,
        f: impl Fn(&TimeGrid, &Occurrence, &gtk::Widget) + 'static,
    ) {
        self.imp().activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` on a card's double click or Enter, which opens the
    /// editor over the popover a single click or Space opens.
    pub fn connect_event_edited(&self, f: impl Fn(&TimeGrid, &Occurrence) + 'static) {
        self.imp().edited.replace(Some(Box::new(f)));
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

    /// The block drawing the occurrence of `key` that starts at `start`.
    pub fn block_at(&self, key: &EventKey, start: EpochMillis) -> Option<gtk::Widget> {
        block_at(&self.imp().blocks.borrow(), key, start)
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
    /// grid it happened on (a page holds it weakly across the repeat
    /// question and acts on it only while it is still on screen), the
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
    /// own event, starts no drag.
    pub fn set_can_move(&self, f: impl Fn(&Occurrence) -> bool + 'static) {
        self.imp().can_move.replace(Some(Box::new(f)));
    }

    /// Says whether a drag across empty time may start.
    pub fn set_can_select(&self, f: impl Fn() -> bool + 'static) {
        self.imp().can_select.replace(Some(Box::new(f)));
    }

    /// Runs `f(false)` once a drag of a card holds the grid and `f(true)`
    /// once it lets go, so the view can turn the ancestor carousel's own
    /// swipe off: it allows touch drags, and a sideways one across a
    /// card would otherwise page the range under the drag.
    pub fn connect_carousel_interactive(&self, f: impl Fn(bool) + 'static) {
        self.imp().carousel_interactive.replace(Some(Box::new(f)));
    }

    /// Returns the card that landed on a new span to its own place, with
    /// no write, for a cancelled repeat question or a failed write.
    pub fn spring_back(&self) {
        let imp = self.imp();
        let nudged = imp.nudged.take();
        if let Some((card, placement)) = nudged {
            self.set_placement(&card, placement);
            return;
        }
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
                .find(|(_, _, w)| *w == current)
                .map(|(_, o, w)| (w.clone(), o.clone()));
            if let Some(found) = found {
                return Some(found);
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
        } else if x >= f64::from(GUTTER)
            && y >= 0.0
            && imp.can_select.borrow().as_ref().is_some_and(|f| f())
        {
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
        if !already_started && drag::is_click(dx, dy, threshold) {
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

        let (press, card, grab, from, origin_column) = {
            let dragging = imp.dragging.borrow();
            let state = dragging.as_ref().expect("checked above");
            (state.press, state.card.clone(), state.grab, state.from, state.column)
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
                // The column stays the one the drag began in: a drag
                // across empty time never crosses into another day's
                // column, only the day it started in, clamped to it, so
                // the ghost and the span `end` reports later always
                // agree.
                let to = drag::clamp_to_day(self.time_at(origin_column, y), self.day_span(origin_column));
                self.show_ghost(Some(drag::selection(from, to)));
                if let Some(state) = imp.dragging.borrow_mut().as_mut() {
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

    /// The release. A drag on a card that never started leaves its own
    /// click to open the popover, as before. On empty time, a press that
    /// never started emits `selected` for the half hour it landed on,
    /// the same click quick create opens from; one that did emits it for
    /// the span dragged. A card drag past the threshold settles it on
    /// the spring instead.
    fn end(&self, dx: f64, dy: f64) {
        let imp = self.imp();
        let taken = imp.dragging.borrow().as_ref().map(|d| {
            (d.started, d.card.clone(), d.grab, d.column, d.from, d.press, d.rect, d.samples.clone())
        });
        let Some((started, card, grab, origin_column, from, press, current_rect, samples)) = taken else {
            return;
        };
        if !started {
            imp.dragging.replace(None);
            if card.is_none() {
                let click = drag::click_slot(from);
                if let Some(f) = imp.selected.borrow().as_ref() {
                    f(click.0, click.1);
                }
            }
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
                    if unchanged {
                        grid.finish_settle();
                        return;
                    }
                    grid.land();
                    if let Some(f) = grid.imp().moved.borrow().as_ref() {
                        f(&grid, &occurrence, start, end);
                    }
                });
            }
            None => {
                imp.dragging.replace(None);
                // Same column and clamp as `update`, so the span this
                // reports matches the ghost the person watched settle.
                let to = drag::clamp_to_day(self.time_at(origin_column, y), self.day_span(origin_column));
                let selection = drag::selection(from, to);
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
        // `AdwAnimation` follows GNOME's animations setting on its own:
        // with it off, `play` ends the spring at once and `done`
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

    /// Lets go of a card that settled on a new span while the view asks
    /// the repeat question and writes the change. The card stays drawn
    /// where it landed: the next `show`, after the write, places it
    /// from the copy, and [`Self::spring_back`] returns it on a Cancel
    /// or a failed write.
    fn land(&self) {
        let imp = self.imp();
        let card = imp
            .dragging
            .borrow()
            .as_ref()
            .and_then(|d| d.card.as_ref().map(|(w, _)| w.clone()));
        let Some(card) = card else {
            self.finish_settle();
            return;
        };
        card.remove_css_class("dragging");
        imp.settle.take();
        self.set_cursor_from_name(None);
        if let Some(f) = imp.carousel_interactive.borrow().as_ref() {
            f(true);
        }
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
    /// Shift+Alt+Down changes when it ends, and Shift+Left or Shift+Right
    /// moves it a whole day, in the event's own zone
    /// (`drag::nudge_days`), for a move between days the grid has no
    /// drag gesture for. Neither writes anything the card's own
    /// predicate refuses. The card moves at once; the view hears of the
    /// total move once the keyboard rests (`drag::Nudges`), so a run of
    /// presses asks one question.
    fn nudge_focused(&self, keyval: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if !state.contains(gdk::ModifierType::SHIFT_MASK) {
            return glib::Propagation::Proceed;
        }
        enum Axis {
            Time(i64),
            Day(i64),
        }
        let axis = match keyval {
            gdk::Key::Up => Axis::Time(-1),
            gdk::Key::Down => Axis::Time(1),
            gdk::Key::Left => Axis::Day(-1),
            gdk::Key::Right => Axis::Day(1),
            _ => return glib::Propagation::Proceed,
        };
        let imp = self.imp();
        let Some(o) = self.focused() else {
            return glib::Propagation::Proceed;
        };
        if !imp.can_move.borrow().as_ref().is_some_and(|f| f(&o)) {
            return glib::Propagation::Proceed;
        }
        let Some(card) = self.block_at(&block::key_of(&o), o.start) else {
            return glib::Propagation::Proceed;
        };
        // A run on another card is asked about before this one starts.
        let other = imp.nudging.borrow().as_ref().is_some_and(|n| n.card != card);
        if other {
            if let Some(source) = imp.nudge_timer.take() {
                source.remove();
            }
            self.ask_nudges();
        }
        let now = glib::monotonic_time() / 1_000;
        let span = imp.nudging.borrow().as_ref().map_or((o.start, o.end), |n| n.run.to);
        let to = match axis {
            Axis::Time(steps) if state.contains(gdk::ModifierType::ALT_MASK) => {
                drag::stretch(span.0, span.1, steps)
            }
            Axis::Time(steps) => drag::nudge(span.0, span.1, steps),
            Axis::Day(steps) => {
                let zone: Tz = o.event.zone.parse().unwrap_or(Tz::UTC);
                drag::nudge_days(span.0, span.1, steps, zone)
            }
        };
        let placement = self.placement_of(&card);
        {
            let mut nudging = imp.nudging.borrow_mut();
            match nudging.as_mut() {
                Some(n) => n.run.press(to, now),
                None => {
                    let Some(placement) = placement else {
                        return glib::Propagation::Proceed;
                    };
                    let mut run = drag::Nudges::start((o.start, o.end), now);
                    run.press(to, now);
                    *nudging = Some(Nudging { card: card.clone(), occurrence: o, run, placement });
                }
            }
        }
        self.place_nudged(&card, to);
        if imp.nudge_timer.borrow().is_none() {
            self.wait_for_nudges(drag::NUDGE_QUIET);
        }
        glib::Propagation::Stop
    }

    /// Checks the run again after `wait` milliseconds. One timer serves
    /// the whole run: when it fires early because another press came, it
    /// waits out what [`drag::Nudges::wait`] says is left.
    fn wait_for_nudges(&self, wait: i64) {
        let weak = self.downgrade();
        let millis = u64::try_from(wait).unwrap_or(0);
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(millis), move || {
            let Some(grid) = weak.upgrade() else { return };
            // The source has fired and is gone; forget it without removing it.
            grid.imp().nudge_timer.take();
            let now = glib::monotonic_time() / 1_000;
            let left = grid.imp().nudging.borrow().as_ref().map(|n| n.run.wait(now));
            match left {
                Some(0) => grid.ask_nudges(),
                Some(left) => grid.wait_for_nudges(left),
                None => {}
            }
        });
        self.imp().nudge_timer.replace(Some(source));
    }

    /// Hands the finished run to the view as one move. A run that ended
    /// where it began asks nothing.
    fn ask_nudges(&self) {
        let imp = self.imp();
        let taken = imp.nudging.take();
        let Some(n) = taken else { return };
        if !n.run.moved() {
            self.set_placement(&n.card, n.placement);
            return;
        }
        imp.nudged.replace(Some((n.card.clone(), n.placement)));
        let (start, end) = n.run.to;
        if let Some(f) = imp.moved.borrow().as_ref() {
            f(self, &n.occurrence, start, end);
        }
    }

    /// Moves a run of nudges from a card a `show` removed to the card
    /// that draws the same occurrence now, or drops the run when the
    /// occurrence has left the grid.
    fn carry_nudges(&self) {
        let imp = self.imp();
        let pending = imp
            .nudging
            .borrow()
            .as_ref()
            .map(|n| (block::key_of(&n.occurrence), n.occurrence.start, n.run.to));
        let Some((key, start, to)) = pending else { return };
        let card = self.block_at(&key, start);
        let placement = card.as_ref().and_then(|c| self.placement_of(c));
        match card.zip(placement) {
            Some((card, placement)) => {
                if let Some(n) = imp.nudging.borrow_mut().as_mut() {
                    n.card = card.clone();
                    n.placement = placement;
                }
                self.place_nudged(&card, to);
            }
            None => {
                imp.nudging.take();
                if let Some(source) = imp.nudge_timer.take() {
                    source.remove();
                }
            }
        }
    }

    /// `card`'s placement from the last `show`, or after a nudge.
    fn placement_of(&self, card: &gtk::Widget) -> Option<imp::Placement> {
        self.imp().children.borrow().iter().find(|(w, _)| w == card).map(|(_, p)| *p)
    }

    fn set_placement(&self, card: &gtk::Widget, placement: imp::Placement) {
        if let Some((_, p)) = self.imp().children.borrow_mut().iter_mut().find(|(w, _)| w == card) {
            *p = placement;
        }
        self.queue_allocate();
    }

    /// Draws `card` at `start` to `end`, in its own lane. A span that
    /// leaves the days shown keeps the card where it last was.
    fn place_nudged(&self, card: &gtk::Widget, (start, end): (EpochMillis, EpochMillis)) {
        let Some((column, top, bottom)) = self.imp().span_placement(start, end) else { return };
        let Some(imp::Placement::Card { columns, lane, lanes, .. }) = self.placement_of(card) else { return };
        let placement = imp::Placement::Card { column, columns, lane, lanes, top, bottom };
        self.set_placement(card, placement);
    }
}

/// The order `children` draw in: their own, with `raised` moved to the
/// end so it draws over the rest.
fn draw_order<W: Clone + PartialEq>(children: &[W], raised: Option<&W>) -> Vec<W> {
    children
        .iter()
        .filter(|c| Some(*c) != raised)
        .chain(raised.filter(|r| children.contains(r)))
        .cloned()
        .collect()
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

/// The closure a card's double click or Enter runs, which reports
/// `occurrence` through [`TimeGrid::connect_event_edited`].
fn edit_closure(grid: &TimeGrid, occurrence: Occurrence) -> std::rc::Rc<dyn Fn()> {
    let weak = grid.downgrade();
    std::rc::Rc::new(move || {
        let Some(grid) = weak.upgrade() else { return };
        if let Some(f) = grid.imp().edited.borrow().as_ref() {
            f(&grid, &occurrence);
        }
    })
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
pub fn focused_key(blocks: &[(EventKey, Occurrence, gtk::Widget)]) -> Option<EventKey> {
    blocks
        .iter()
        .find(|(_, _, widget)| widget.has_focus())
        .map(|(key, _, _)| key.clone())
}

/// The occurrence whose block among `blocks` has the keyboard focus.
pub fn focused_occurrence(blocks: &[(EventKey, Occurrence, gtk::Widget)]) -> Option<Occurrence> {
    blocks
        .iter()
        .find(|(_, _, widget)| widget.has_focus())
        .map(|(_, o, _)| o.clone())
}

fn find_block(blocks: &[(EventKey, Occurrence, gtk::Widget)], key: &EventKey) -> Option<gtk::Widget> {
    blocks
        .iter()
        .find(|(k, _, _)| k == key)
        .map(|(_, _, widget)| widget.clone())
}

/// The block among `blocks` drawing the occurrence of `key` that starts
/// at `start`. Every occurrence of an unsplit series shares one key, and
/// a page can show several of them, so the key alone may find a sibling.
pub fn block_at<W: Clone>(
    blocks: &[(EventKey, Occurrence, W)],
    key: &EventKey,
    start: EpochMillis,
) -> Option<W> {
    blocks
        .iter()
        .find(|(k, o, _)| k == key && o.start == start)
        .map(|(_, _, widget)| widget.clone())
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
            let on_edit = edit_closure_strip(self, o.clone());
            let card = EventBlock::new(o, colour, name, true, named_day, &chrono::Local, on_edit).widget;
            connect_activated_strip(self, &card, o.clone());
            card.set_parent(self);
            blocks.push((block::key_of(o), o.clone(), card.clone().upcast()));
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

    /// The block drawing the occurrence of `key` that starts at `start`.
    pub fn block_at(&self, key: &EventKey, start: EpochMillis) -> Option<gtk::Widget> {
        block_at(&self.imp().blocks.borrow(), key, start)
    }

    /// The first card Tab reaches.
    pub fn first_block(&self) -> Option<gtk::Widget> {
        self.imp().children.borrow().first().map(|(w, _)| w.clone())
    }

    /// The event whose block has the keyboard focus.
    pub fn focused_key(&self) -> Option<EventKey> {
        focused_key(&self.imp().blocks.borrow())
    }

    /// The occurrence whose block has the keyboard focus.
    pub fn focused(&self) -> Option<Occurrence> {
        focused_occurrence(&self.imp().blocks.borrow())
    }

    pub fn connect_event_activated(
        &self,
        f: impl Fn(&AllDayStrip, &Occurrence, &gtk::Widget) + 'static,
    ) {
        self.imp().activated.replace(Some(Box::new(f)));
    }

    /// Runs `f` on a card's double click or Enter, which opens the
    /// editor over the popover a single click or Space opens.
    pub fn connect_event_edited(&self, f: impl Fn(&AllDayStrip, &Occurrence) + 'static) {
        self.imp().edited.replace(Some(Box::new(f)));
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

/// The closure a strip card's double click or Enter runs, which reports
/// `occurrence` through [`AllDayStrip::connect_event_edited`].
fn edit_closure_strip(strip: &AllDayStrip, occurrence: Occurrence) -> std::rc::Rc<dyn Fn()> {
    let weak = strip.downgrade();
    std::rc::Rc::new(move || {
        let Some(strip) = weak.upgrade() else { return };
        if let Some(f) = strip.imp().edited.borrow().as_ref() {
            f(&strip, &occurrence);
        }
    })
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
    fn a_five_minute_event_still_draws_a_readable_card() {
        let (_, _, _, height) = rect(0, 1, 0, 1, 9.0, 9.0 + 5.0 / 60.0, 800.0);
        assert_eq!(height, MIN_HEIGHT);
    }

    #[test]
    fn an_hour_long_event_is_well_above_the_minimum_and_keeps_its_own_height() {
        let (_, _, _, height) = rect(0, 1, 0, 1, 9.0, 10.0, 800.0);
        assert_eq!(height, HOUR - CARD_INSET);
    }

    #[test]
    fn the_card_s_top_never_moves_to_make_room_for_the_minimum() {
        let (_, top, _, _) = rect(0, 1, 0, 1, 9.0, 9.0 + 5.0 / 60.0, 800.0);
        assert_eq!(top, 9.0 * HOUR + CARD_INSET / 2.0);
    }

    #[test]
    fn min_duration_is_how_long_an_event_runs_before_its_card_clears_the_floor() {
        let min = min_duration(24.0, 3.0, 62.0);
        // 27 px at 62 px an hour is 26 minutes 8 seconds, rounded to the
        // millisecond.
        assert_eq!(min, 1_567_742);
    }

    #[test]
    fn two_short_events_close_together_take_separate_lanes() {
        // Two five-minute events ten minutes apart do not overlap in
        // real time, but once both stretch to the minimum card height
        // they would draw on top of each other in one lane.
        let five_min = 5 * 60_000;
        let ten_min = 10 * 60_000;
        let spans = [(0, five_min), (ten_min, ten_min + five_min)];
        let stretched = stretch_for_lanes(&spans, 20 * 60_000);
        let (placed, _) = layout::lanes(&stretched);
        assert_eq!(placed[0].lane, 0);
        assert_eq!(placed[1].lane, 1);
        assert_eq!(placed[0].lanes, 2);
    }

    #[test]
    fn events_well_apart_share_a_lane_even_after_stretching() {
        let five_min = 5 * 60_000;
        let spans = [(0, five_min), (30 * 60_000, 30 * 60_000 + five_min)];
        let stretched = stretch_for_lanes(&spans, 20 * 60_000);
        let (placed, _) = layout::lanes(&stretched);
        assert_eq!(placed[0].lane, 0);
        assert_eq!(placed[1].lane, 0);
        assert_eq!(placed[0].lanes, 1);
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

    #[test]
    fn a_series_shown_twice_finds_the_block_of_the_asked_occurrence() {
        // A weekly series on a month page: two occurrences share one key.
        let first = all_day_event(1_000, 2_000);
        let second = Occurrence {
            start: 5_000,
            end: 6_000,
            ..first.clone()
        };
        let key = block::key_of(&first);
        let blocks = [
            (key.clone(), first, "first"),
            (key.clone(), second, "second"),
        ];
        assert_eq!(block_at(&blocks, &key, 5_000), Some("second"));
        assert_eq!(block_at(&blocks, &key, 1_000), Some("first"));
        assert_eq!(block_at(&blocks, &key, 9_000), None);
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
    fn a_dragged_card_draws_after_every_other_child() {
        assert_eq!(draw_order(&["a", "b", "c", "d"], Some(&"b")), ["a", "c", "d", "b"]);
    }

    #[test]
    fn with_no_drag_the_children_draw_in_their_own_order() {
        assert_eq!(draw_order(&["a", "b", "c"], None), ["a", "b", "c"]);
    }

    #[test]
    fn more_events_take_the_plural_they_need() {
        assert_eq!(more_label(1), "1 more event");
        assert_eq!(more_label(3), "3 more events");
    }
}
