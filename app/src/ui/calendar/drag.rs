//! The numbers behind dragging an event on the time grid: where it sits
//! while the pointer holds it, where it settles on release, how the card
//! resists past the edges of the day, and where a new event starts. The
//! grid turns pixels into instants with `layout::instant_at` and hands
//! them here.
//!
//! Snapping works on UTC quarter hours. Every zone's offset is a whole
//! number of quarter hours, so they are local quarter hours too.
//!
//! `snap` and `rubber_band` live here rather than in a `motion` module:
//! stage 2 never wrote one (it took `adw::Carousel` instead), so this is
//! their first use.

use std::collections::VecDeque;

use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{Access, Occurrence, Status};

pub const STEP_MINUTES: i64 = 15;
const STEP: EpochMillis = STEP_MINUTES * 60_000;
/// The shortest event a drag makes.
pub const SHORTEST: EpochMillis = STEP;
/// How far in from a card's top or bottom edge a press resizes it.
pub const END_HANDLE: f64 = 8.0;
const HOUR: EpochMillis = 3_600_000;

/// The part of a card the pointer took hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// The body moves the event.
    Body,
    /// The top edge moves its start.
    Start,
    /// The bottom edge moves its end.
    End,
}

/// One drag of an event, from the press on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grab {
    pub handle: Handle,
    pub start: EpochMillis,
    pub end: EpochMillis,
    /// The pointer's time minus the grabbed edge's, at the press.
    pub offset: EpochMillis,
}

impl Grab {
    pub fn new(handle: Handle, start: EpochMillis, end: EpochMillis, pointer: EpochMillis) -> Grab {
        let edge = match handle {
            Handle::Body | Handle::Start => start,
            Handle::End => end,
        };
        Grab { handle, start, end, offset: pointer - edge }
    }

    /// The event's span with the pointer at `pointer`, unsnapped, so the
    /// card moves as far as the hand does.
    pub fn follow(&self, pointer: EpochMillis) -> (EpochMillis, EpochMillis) {
        let edge = pointer - self.offset;
        match self.handle {
            Handle::Body => (edge, edge + (self.end - self.start)),
            Handle::Start => (edge.min(self.end - SHORTEST), self.end),
            Handle::End => (self.start, edge.max(self.start + SHORTEST)),
        }
    }

    /// Where the event lands on release: on the nearest quarter hour, and
    /// inside `day`, which the rubber band only lets the card leave for a
    /// moment.
    pub fn settle(&self, pointer: EpochMillis, day: (EpochMillis, EpochMillis)) -> (EpochMillis, EpochMillis) {
        let (start, end) = self.follow(pointer);
        match self.handle {
            Handle::Body => {
                let length = end - start;
                let latest = (day.1 - length).max(day.0);
                let start = snap(start, STEP_MINUTES).clamp(day.0, latest);
                (start, start + length)
            }
            Handle::Start => {
                let latest = end - SHORTEST;
                (snap(start, STEP_MINUTES).clamp(day.0.min(latest), latest), end)
            }
            Handle::End => (start, snap(end, STEP_MINUTES).clamp(start + SHORTEST, day.1.max(start + SHORTEST))),
        }
    }
}

/// Which part of a card `y` points at, `height` being the card's.
pub fn handle_at(y: f64, height: f64) -> Handle {
    let edge = END_HANDLE.min(height / 3.0);
    if y >= height - edge {
        Handle::End
    } else if y < edge {
        Handle::Start
    } else {
        Handle::Body
    }
}

/// The span a drag across empty time from `a` to `b` covers: whole
/// quarter hours, at least one.
pub fn selection(a: EpochMillis, b: EpochMillis) -> (EpochMillis, EpochMillis) {
    let start = a.min(b).div_euclid(STEP) * STEP;
    let end = (a.max(b) + STEP - 1).div_euclid(STEP) * STEP;
    (start, end.max(start + SHORTEST))
}

/// `at`, kept inside `day`: where a drag across empty time lands when
/// the pointer strays above the grid's first hour or below its last, or
/// sideways into another day's column, which stays the day the drag
/// began in rather than the one the pointer wandered into.
pub fn clamp_to_day(at: EpochMillis, day: (EpochMillis, EpochMillis)) -> EpochMillis {
    at.clamp(day.0, day.1)
}

/// Whether a press that has moved `dx`, `dy` pixels from where it
/// landed still counts as a click rather than a drag: under
/// `threshold`, GTK's own tolerance for a press that wanders before
/// release. The caller latches the answer once it turns `false`, so a
/// gesture that ever leaves the threshold stays a drag for the rest of
/// the press, however still the pointer sits by release.
pub fn is_click(dx: f64, dy: f64, threshold: f64) -> bool {
    dx.hypot(dy) < threshold
}

/// Whether a press with this `n_press` count opens the editor: the
/// second press of a double click, over the popover the first press
/// already opened through the card's own `clicked` signal. A third
/// press and beyond opens nothing more.
pub fn opens_editor(n_press: i32) -> bool {
    n_press == 2
}

/// Half an hour long.
const HALF_HOUR: EpochMillis = 30 * 60_000;

/// The slot a single click on empty time opens quick create at: the
/// half hour `at` falls in, so a click near the bottom of a slot does
/// not open the one below it.
pub fn click_slot(at: EpochMillis) -> (EpochMillis, EpochMillis) {
    let start = at.div_euclid(HALF_HOUR) * HALF_HOUR;
    (start, start + HALF_HOUR)
}

/// `at` rounded to the nearest multiple of `step_minutes`, a tie going up.
pub fn snap(at: EpochMillis, step_minutes: i64) -> EpochMillis {
    let step = step_minutes * 60_000;
    (at + step / 2).div_euclid(step) * step
}

/// How far a card past a boundary actually moves for an `overshoot` past
/// it, out of a `dimension` the drag runs in: less the further it goes,
/// so the card resists rather than stopping dead.
pub fn rubber_band(overshoot: f64, dimension: f64) -> f64 {
    const CONSTANT: f64 = 0.55;
    overshoot * dimension * CONSTANT / (dimension + CONSTANT * overshoot.abs())
}

/// A card's top, `top` pixels down a day `day` pixels tall, with
/// resistance past either edge.
pub fn banded(top: f64, height: f64, day: f64) -> f64 {
    let lowest = (day - height).max(0.0);
    if top < 0.0 {
        -rubber_band(-top, day)
    } else if top > lowest {
        lowest + rubber_band(top - lowest, day)
    } else {
        top
    }
}

/// Where N puts a new hour-long event: after the event with the focus,
/// else at the time last clicked, else at the next quarter hour when the
/// range holds now, else at `morning` (09:00 on the range's first day).
pub fn new_slot(
    focused: Option<(EpochMillis, EpochMillis)>,
    cursor: Option<EpochMillis>,
    now: EpochMillis,
    range: (EpochMillis, EpochMillis),
    morning: EpochMillis,
) -> (EpochMillis, EpochMillis) {
    let start = match (focused, cursor) {
        (Some((_, end)), _) => end,
        (None, Some(at)) => at,
        (None, None) if (range.0..range.1).contains(&now) => (now.div_euclid(STEP) + 1) * STEP,
        (None, None) => morning,
    };
    (start, start + HOUR)
}

/// The release velocity in pixels per second, from the first and last of
/// the last few pointer samples, each `(frame-clock time in
/// microseconds, y)`. Zero with fewer than two, or two at the same time.
pub fn velocity(samples: &VecDeque<(i64, f64)>) -> f64 {
    let (Some(&(t0, y0)), Some(&(t1, y1))) = (samples.front(), samples.back()) else {
        return 0.0;
    };
    let seconds = (t1 - t0) as f64 / 1_000_000.0;
    if seconds <= 0.0 { 0.0 } else { (y1 - y0) / seconds }
}

/// The event's span moved by `steps` quarter hours, keeping its length:
/// the keyboard path Shift+Up and Shift+Down offer beside the drag,
/// for whoever cannot make the drag's gesture.
pub fn nudge(start: EpochMillis, end: EpochMillis, steps: i64) -> (EpochMillis, EpochMillis) {
    let delta = steps * STEP;
    (start + delta, end + delta)
}

/// The event stretched by `steps` quarter hours at its end, never below
/// `SHORTEST`: the keyboard path Shift+Alt+Up and Shift+Alt+Down offer
/// beside the drag's stretch of a card's bottom edge.
pub fn stretch(start: EpochMillis, end: EpochMillis, steps: i64) -> (EpochMillis, EpochMillis) {
    (start, (end + steps * STEP).max(start + SHORTEST))
}

/// The event stretched by `steps` quarter hours at its start, never
/// closer to its end than `SHORTEST`: the keyboard path Ctrl+Shift+Up
/// and Ctrl+Shift+Down offer beside the drag of a card's top edge.
pub fn stretch_start(start: EpochMillis, end: EpochMillis, steps: i64) -> (EpochMillis, EpochMillis) {
    ((start + steps * STEP).min(end - SHORTEST), end)
}

/// The index of the Month cell under `(x, y)`, counted from the top left
/// of six week rows of seven cells: `width` split in seven equal columns,
/// numbered from the right in a right-to-left locale, and rows as tall as
/// `weeks` says. `None` off the grid, where a drag lets go of nothing.
pub fn month_day_at(x: f64, y: f64, width: f64, weeks: &[i32], rtl: bool) -> Option<usize> {
    if width <= 0.0 || !(0.0..width).contains(&x) || y < 0.0 {
        return None;
    }
    let column = ((x / width * 7.0).floor() as usize).min(6);
    let column = if rtl { 6 - column } else { column };
    let mut top = 0.0;
    for (week, &height) in weeks.iter().enumerate() {
        let bottom = top + f64::from(height);
        if y < bottom {
            return Some(week * 7 + column);
        }
        top = bottom;
    }
    None
}

/// The event moved `days` days: an all-day event by whole UTC days, as
/// its dates are UTC dates, and a timed one at the same wall-clock time
/// in `zone` ([`nudge_days`]).
pub fn by_days(start: EpochMillis, end: EpochMillis, all_day: bool, days: i64, zone: Tz) -> (EpochMillis, EpochMillis) {
    nudge_days(start, end, days, if all_day { Tz::UTC } else { zone })
}

/// The end of an all-day bar a drag took hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

const DAY: EpochMillis = 24 * HOUR;

/// Which end of an all-day card `x` points at, `width` being the
/// card's: the same reach in from each end as a timed card's edges.
pub fn edge_at(x: f64, width: f64) -> Option<Edge> {
    let reach = END_HANDLE.min(width / 3.0);
    if x < reach {
        Some(Edge::Start)
    } else if x >= width - reach {
        Some(Edge::End)
    } else {
        None
    }
}

/// An all-day bar with its `edge` moved `days` days, never shorter than
/// one day.
pub fn resize_days(start: EpochMillis, end: EpochMillis, edge: Edge, days: i64) -> (EpochMillis, EpochMillis) {
    match edge {
        Edge::Start => ((start + days * DAY).min(end - DAY), end),
        Edge::End => (start, (end + days * DAY).max(start + DAY)),
    }
}

/// Where a card in the all-day row lands after a drag of `days` columns,
/// by its body (`edge` `None`) or by one end. An all-day event moves by
/// UTC days, as its dates are UTC dates. A timed entry that covers whole
/// days, as a whole-day out of office does, stays timed and moves by
/// calendar days in `zone`, its own, so it keeps starting and ending at
/// midnight across a clock change; a stretch never leaves it shorter
/// than one day.
pub fn strip_landing(
    start: EpochMillis,
    end: EpochMillis,
    all_day: bool,
    edge: Option<Edge>,
    days: i64,
    zone: Tz,
) -> Landing {
    let (start, end) = match (all_day, edge) {
        (true, None) => by_days(start, end, true, days, zone),
        (true, Some(edge)) => resize_days(start, end, edge, days),
        (false, None) => nudge_days(start, end, days, zone),
        (false, Some(Edge::Start)) => (shift_days(start, days, zone).min(shift_days(end, -1, zone)), end),
        (false, Some(Edge::End)) => (start, shift_days(end, days, zone).max(shift_days(start, 1, zone))),
    };
    Landing { start, end, all_day }
}

/// The span of an event made all-day on `day`: UTC midnight to the next,
/// the way an all-day event keeps its dates.
pub fn all_day_on(day: NaiveDate) -> (EpochMillis, EpochMillis) {
    let midnight = day.and_hms_opt(0, 0, 0).map_or(0, |t| t.and_utc().timestamp_millis());
    (midnight, midnight + DAY)
}

/// The span of an all-day event dropped among the hours at `at`: an hour
/// from the nearest quarter hour.
pub fn timed_at(at: EpochMillis) -> (EpochMillis, EpochMillis) {
    let start = snap(at, STEP_MINUTES);
    (start, start + HOUR)
}

/// Where a drag lands an event: its new span, and whether it is all-day
/// there, which a drop between the all-day row and the hours changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landing {
    pub start: EpochMillis,
    pub end: EpochMillis,
    pub all_day: bool,
}

/// What a key does to the event the time grid has focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridKey {
    /// Shift+Up or Shift+Down: move it by quarter hours.
    Move(i64),
    /// Ctrl+Shift+Up or Ctrl+Shift+Down: move its start.
    Start(i64),
    /// Shift+Alt+Up or Shift+Alt+Down: move its end.
    End(i64),
    /// Shift+Left or Shift+Right: move it by days.
    Days(i64),
}

/// The change `key` with `modifiers` makes to the focused event, when it
/// is one of the grid's own keys, which all hold Shift.
pub fn grid_key(key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> Option<GridKey> {
    use gtk::gdk::{Key, ModifierType};
    if !modifiers.contains(ModifierType::SHIFT_MASK) {
        return None;
    }
    let alt = modifiers.contains(ModifierType::ALT_MASK);
    let control = modifiers.contains(ModifierType::CONTROL_MASK);
    let steps = match key {
        Key::Up => -1,
        Key::Down => 1,
        Key::Left if !alt && !control => return Some(GridKey::Days(-1)),
        Key::Right if !alt && !control => return Some(GridKey::Days(1)),
        _ => return None,
    };
    match (alt, control) {
        (false, false) => Some(GridKey::Move(steps)),
        (true, false) => Some(GridKey::End(steps)),
        (false, true) => Some(GridKey::Start(steps)),
        (true, true) => None,
    }
}

/// The event's span moved `days` calendar days in `zone`, keeping its
/// wall-clock time: the keyboard path Shift+Left and Shift+Right offer,
/// for a move between days the grid has no drag gesture for. Moving the
/// date and keeping the same local time, rather than adding a fixed
/// number of milliseconds, keeps the clock steady across a change to
/// `zone`'s own offset (see the rrule-until-dst skill).
pub fn nudge_days(start: EpochMillis, end: EpochMillis, days: i64, zone: Tz) -> (EpochMillis, EpochMillis) {
    (shift_days(start, days, zone), shift_days(end, days, zone))
}

/// `at` moved `days` calendar days later (or earlier, negative) in
/// `zone`, at the same wall-clock time. Falls back to `at` unshifted on
/// the near-impossible chance a date this far out overflows, or a local
/// time a clock change skips entirely.
fn shift_days(at: EpochMillis, days: i64, zone: Tz) -> EpochMillis {
    let Some(utc) = DateTime::<Utc>::from_timestamp_millis(at) else { return at };
    let local = utc.with_timezone(&zone);
    let Some(date) = shift_date(local.date_naive(), days) else { return at };
    let naive = date.and_time(local.time());
    zone.from_local_datetime(&naive)
        .earliest()
        .map_or(at, |shifted| shifted.with_timezone(&Utc).timestamp_millis())
}

fn shift_date(date: NaiveDate, days: i64) -> Option<NaiveDate> {
    match u64::try_from(days) {
        Ok(days) => date.checked_add_days(Days::new(days)),
        Err(_) => date.checked_sub_days(Days::new(days.unsigned_abs())),
    }
}

/// How long the keyboard must rest after the last Shift+arrow before the
/// calendar asks about the run of nudges, in milliseconds.
pub const NUDGE_QUIET: i64 = 1_000;

/// A run of keyboard nudges on one card. Each press moves the card on
/// screen at once; the calendar asks one question for the total move
/// once the presses stop, and a Cancel puts the card back at `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nudges {
    /// Where the card was before the first press.
    pub from: (EpochMillis, EpochMillis),
    /// Where the presses so far have put it.
    pub to: (EpochMillis, EpochMillis),
    /// When the last press came, in milliseconds on a monotonic clock.
    last_press: i64,
}

impl Nudges {
    /// A run that has not moved the card yet, started at `now`.
    pub fn start(from: (EpochMillis, EpochMillis), now: i64) -> Nudges {
        Nudges { from, to: from, last_press: now }
    }

    /// One more press, which put the card at `to`.
    pub fn press(&mut self, to: (EpochMillis, EpochMillis), now: i64) {
        self.to = to;
        self.last_press = now;
    }

    /// How long to wait from `now` before asking: zero once the keyboard
    /// has rested for [`NUDGE_QUIET`] since the last press.
    pub fn wait(&self, now: i64) -> i64 {
        (self.last_press + NUDGE_QUIET - now).max(0)
    }

    /// Whether the run ends somewhere else than it began. Nudging down
    /// and back up again leaves nothing to ask.
    pub fn moved(&self) -> bool {
        self.from != self.to
    }
}

/// Whether a drag may move `o`: the calendar must be one the account can
/// write to, the account must offer a calendar and not have withheld it,
/// and the event must not be a guest's own event or already leaving
/// through a queued removal.
pub fn can_move(o: &Occurrence, access: Access, offers_calendar: bool, withheld_calendar: bool) -> bool {
    access.can_write()
        && offers_calendar
        && !withheld_calendar
        && !super::draft::limited(&o.event)
        && o.event.status != Status::Cancelled
        && !o.event.kind.made_elsewhere()
}

#[cfg(test)]
mod tests {
    use super::*;
    const M: EpochMillis = 60_000;
    const H: EpochMillis = 60 * M;

    #[test]
    fn a_dragged_event_stays_under_the_pointer_where_it_was_grabbed() {
        // Grabbed 20 minutes into a 10:00 to 11:00 event.
        let grab = Grab::new(Handle::Body, 10 * H, 11 * H, 10 * H + 20 * M);
        assert_eq!(grab.follow(13 * H + 27 * M), (13 * H + 7 * M, 14 * H + 7 * M));
    }

    #[test]
    fn a_release_settles_on_the_nearest_quarter_hour() {
        let grab = Grab::new(Handle::Body, 10 * H, 11 * H, 10 * H + 20 * M);
        let day = (0, 24 * H);
        // 13:07 is nearer 13:00 than 13:15.
        assert_eq!(grab.settle(13 * H + 27 * M, day), (13 * H, 14 * H));
        assert_eq!(grab.settle(13 * H + 29 * M, day), (13 * H + 15 * M, 14 * H + 15 * M));
    }

    #[test]
    fn a_release_past_the_day_keeps_the_event_inside_it() {
        let grab = Grab::new(Handle::Body, 22 * H, 23 * H, 22 * H);
        assert_eq!(grab.settle(25 * H, (0, 24 * H)), (23 * H, 24 * H));
        assert_eq!(grab.settle(-2 * H, (0, 24 * H)), (0, H));
    }

    #[test]
    fn stretching_moves_only_the_end_and_never_below_a_quarter_hour() {
        let grab = Grab::new(Handle::End, 10 * H, 11 * H, 11 * H - 2 * M);
        assert_eq!(grab.follow(12 * H - 2 * M), (10 * H, 12 * H));
        assert_eq!(grab.settle(12 * H + 5 * M, (0, 24 * H)), (10 * H, 12 * H));
        assert_eq!(grab.settle(9 * H, (0, 24 * H)), (10 * H, 10 * H + SHORTEST));
    }

    #[test]
    fn the_bottom_edge_of_a_card_stretches_it() {
        assert_eq!(handle_at(10.0, 52.0), Handle::Body);
        assert_eq!(handle_at(46.0, 52.0), Handle::End);
        // A short card keeps most of itself for moving.
        assert_eq!(handle_at(8.0, 18.0), Handle::Body);
        assert_eq!(handle_at(14.0, 18.0), Handle::End);
    }

    #[test]
    fn a_drag_across_empty_time_covers_whole_quarter_hours() {
        assert_eq!(selection(10 * H + 50 * M, 10 * H + 5 * M), (10 * H, 11 * H));
        assert_eq!(selection(10 * H + 3 * M, 10 * H + 4 * M), (10 * H, 10 * H + SHORTEST));
    }

    #[test]
    fn a_press_under_the_threshold_is_a_click() {
        assert!(is_click(2.0, 1.0, 8.0));
        assert!(!is_click(6.0, 6.0, 8.0), "8.49 px of travel passes an 8 px threshold");
        assert!(is_click(0.0, 0.0, 8.0));
    }

    #[test]
    fn a_drag_across_empty_time_stays_inside_the_day_it_began_in() {
        let day = (0, 24 * H);
        // Past the grid's bottom edge, a day and a half in: clamped to
        // that day's own midnight, not spilled into the next one.
        assert_eq!(clamp_to_day(day.1 + 12 * H, day), day.1);
        // Above the grid's top edge.
        assert_eq!(clamp_to_day(-3 * H, day), day.0);
        // Already inside the day: untouched.
        assert_eq!(clamp_to_day(17 * H + 30 * M, day), 17 * H + 30 * M);
    }

    #[test]
    fn only_the_second_press_opens_the_editor() {
        assert!(!opens_editor(1), "the first press leaves the popover the button's own click opens");
        assert!(opens_editor(2));
        assert!(!opens_editor(3), "a third press opens nothing more");
    }

    #[test]
    fn a_click_opens_the_half_hour_it_falls_in() {
        assert_eq!(click_slot(10 * H + 12 * M), (10 * H, 10 * H + 30 * M));
        assert_eq!(click_slot(10 * H + 31 * M), (10 * H + 30 * M, 11 * H));
        assert_eq!(click_slot(10 * H), (10 * H, 10 * H + 30 * M));
    }

    #[test]
    fn snap_rounds_to_the_nearest_quarter_hour() {
        assert_eq!(snap(7 * M, 15), 0);
        assert_eq!(snap(8 * M, 15), 15 * M);
        // A tie, exactly 7 minutes 30 seconds from each mark, goes up.
        assert_eq!(snap(7 * M + 30_000, 15), 15 * M);
    }

    #[test]
    fn rubber_band_gives_less_the_further_it_goes() {
        let near = rubber_band(10.0, 500.0);
        let far = rubber_band(100.0, 500.0);
        assert!(near < 10.0 && far < 100.0);
        // The further the drag goes, the smaller a fraction of it the
        // card actually follows.
        assert!(far / 100.0 < near / 10.0);
    }

    #[test]
    fn past_the_edges_of_the_day_the_card_resists() {
        let day = 24.0 * f64::from(super::super::time_grid::HOUR);
        assert_eq!(banded(100.0, 52.0, day), 100.0);
        let above = banded(-60.0, 52.0, day);
        assert!(above < 0.0 && above > -60.0);
        let below = banded(day, 52.0, day);
        assert!(below > day - 52.0 && below < day);
    }

    #[test]
    fn a_new_event_starts_where_the_person_is() {
        let range = (0, 7 * 24 * H);
        let morning = 9 * H;
        assert_eq!(new_slot(Some((10 * H, 11 * H)), Some(15 * H), 30 * H, range, morning), (11 * H, 12 * H));
        assert_eq!(new_slot(None, Some(15 * H), 30 * H, range, morning), (15 * H, 16 * H));
        assert_eq!(new_slot(None, None, 30 * H + 5 * M, range, morning), (30 * H + 15 * M, 31 * H + 15 * M));
        assert_eq!(new_slot(None, None, 9 * 24 * H, range, morning), (9 * H, 10 * H));
    }

    #[test]
    fn the_release_velocity_reads_pixels_per_second_from_the_first_and_last_sample() {
        let mut samples = std::collections::VecDeque::new();
        samples.push_back((0, 100.0));
        samples.push_back((250_000, 130.0));
        // 30 px in a quarter second is 120 px/s.
        assert_eq!(velocity(&samples), 120.0);
        assert_eq!(velocity(&std::collections::VecDeque::new()), 0.0);
        let mut one = std::collections::VecDeque::new();
        one.push_back((0, 5.0));
        assert_eq!(velocity(&one), 0.0);
    }

    fn timed(all_day: bool, status: mailrs_domain::calendar::Status) -> Occurrence {
        use mailrs_domain::calendar::Event;
        use std::sync::Arc;
        Occurrence {
            account_id: 1,
            event: Arc::new(Event { all_day, status, ..Event::default() }),
            start: 0,
            end: H,
        }
    }

    #[test]
    fn only_a_writable_offered_granted_event_may_be_dragged() {
        use mailrs_domain::calendar::Status;
        let o = timed(false, Status::Confirmed);
        assert!(can_move(&o, Access::Owner, true, false));
        assert!(!can_move(&o, Access::Reader, true, false), "a read-only calendar starts no drag");
        assert!(!can_move(&o, Access::Owner, false, false), "an account with no calendar offer starts no drag");
        assert!(!can_move(&o, Access::Owner, true, true), "a withheld calendar permission starts no drag");
        assert!(can_move(&timed(true, Status::Confirmed), Access::Owner, true, false), "an all-day event drags in the all-day row and Month");
        assert!(!can_move(&timed(false, Status::Cancelled), Access::Owner, true, false), "a cancelled occurrence, on its way out, starts no drag");
    }

    #[test]
    fn a_birthday_starts_no_drag() {
        use mailrs_domain::calendar::{Event, Kind};
        let o = Occurrence { event: std::sync::Arc::new(Event { kind: Kind::Birthday, ..Event::default() }), ..timed(true, mailrs_domain::calendar::Status::Confirmed) };
        assert!(!can_move(&o, Access::Owner, true, false), "Google's own apps make birthdays");
    }

    #[test]
    fn a_run_of_nudges_asks_one_second_after_the_last_press() {
        let mut run = Nudges::start((10 * H, 11 * H), 0);
        run.press(nudge(10 * H, 11 * H, 1), 0);
        run.press(nudge(10 * H + 15 * M, 11 * H + 15 * M, 1), 600);
        assert_eq!(run.wait(1_000), 600, "the second press starts the wait again");
        assert_eq!(run.wait(1_600), 0);
        assert_eq!(run.wait(5_000), 0);
    }

    #[test]
    fn a_run_of_nudges_adds_up_and_remembers_where_it_began() {
        let mut run = Nudges::start((10 * H, 11 * H), 0);
        run.press((10 * H + 15 * M, 11 * H + 15 * M), 100);
        run.press((10 * H + 30 * M, 11 * H + 30 * M), 200);
        assert_eq!(run.from, (10 * H, 11 * H));
        assert_eq!(run.to, (10 * H + 30 * M, 11 * H + 30 * M));
        assert!(run.moved());
    }

    #[test]
    fn nudging_down_and_back_up_leaves_nothing_to_ask() {
        let mut run = Nudges::start((10 * H, 11 * H), 0);
        run.press((10 * H + 15 * M, 11 * H + 15 * M), 100);
        run.press((10 * H, 11 * H), 200);
        assert!(!run.moved());
    }

    #[test]
    fn shift_up_and_down_nudge_an_event_by_whole_quarter_hours() {
        assert_eq!(nudge(10 * H, 11 * H, 1), (10 * H + 15 * M, 11 * H + 15 * M));
        assert_eq!(nudge(10 * H, 11 * H, -2), (10 * H - 30 * M, 11 * H - 30 * M));
    }

    #[test]
    fn shift_alt_up_and_down_stretch_an_event_s_end_never_below_the_shortest() {
        assert_eq!(stretch(10 * H, 11 * H, 1), (10 * H, 11 * H + 15 * M));
        assert_eq!(stretch(10 * H, 10 * H + SHORTEST, -4), (10 * H, 10 * H + SHORTEST));
    }

    #[test]
    fn shift_left_and_right_move_an_event_by_whole_days() {
        let lisbon = chrono_tz::Europe::Lisbon;
        let start = lisbon.with_ymd_and_hms(2026, 9, 23, 10, 0, 0).unwrap().timestamp_millis();
        let end = start + H;
        assert_eq!(nudge_days(start, end, 1, lisbon), (start + 24 * H, end + 24 * H));
        assert_eq!(nudge_days(start, end, -2, lisbon), (start - 48 * H, end - 48 * H));
    }

    /// Lisbon falls back an hour on the last Sunday of October 2026, 25
    /// October. A day earlier than 26 October at 10:00 local must still
    /// read 10:00 local on 25 October: a fixed 24-hour subtraction would
    /// land on 09:00, an hour short of the wall clock.
    #[test]
    fn shift_left_keeps_the_wall_clock_time_across_a_clock_change() {
        let lisbon = chrono_tz::Europe::Lisbon;
        let start = lisbon.with_ymd_and_hms(2026, 10, 26, 10, 0, 0).unwrap().timestamp_millis();
        let end = start + H;
        let (moved_start, moved_end) = nudge_days(start, end, -1, lisbon);
        let local = DateTime::<Utc>::from_timestamp_millis(moved_start)
            .unwrap()
            .with_timezone(&lisbon);
        assert_eq!(local.date_naive(), NaiveDate::from_ymd_opt(2026, 10, 25).unwrap());
        assert_eq!(local.time(), chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap());
        assert_eq!(moved_end - moved_start, H, "the event's own length stays the same");
    }

    #[test]
    fn the_top_edge_of_a_card_moves_its_start() {
        assert_eq!(handle_at(3.0, 52.0), Handle::Start);
        assert_eq!(handle_at(9.0, 52.0), Handle::Body);
        // A short card keeps its middle third for moving.
        assert_eq!(handle_at(5.0, 18.0), Handle::Start);
        assert_eq!(handle_at(8.0, 18.0), Handle::Body);
    }

    #[test]
    fn dragging_the_top_edge_moves_the_start_and_keeps_the_end() {
        let grab = Grab::new(Handle::Start, 10 * H, 11 * H, 10 * H + 2 * M);
        assert_eq!(grab.follow(9 * H + 2 * M), (9 * H, 11 * H));
        // 09:22 snaps to 09:15; the end stays at 11:00.
        assert_eq!(grab.settle(9 * H + 24 * M, (0, 24 * H)), (9 * H + 15 * M, 11 * H));
    }

    #[test]
    fn the_top_edge_stops_a_quarter_hour_before_the_end() {
        let grab = Grab::new(Handle::Start, 10 * H, 11 * H, 10 * H);
        assert_eq!(grab.follow(12 * H), (11 * H - SHORTEST, 11 * H));
        assert_eq!(grab.settle(12 * H, (0, 24 * H)), (11 * H - SHORTEST, 11 * H));
    }

    #[test]
    fn the_top_edge_stops_at_the_top_of_the_day() {
        let grab = Grab::new(Handle::Start, 10 * H, 11 * H, 10 * H);
        assert_eq!(grab.settle(-2 * H, (0, 24 * H)), (0, 11 * H));
    }

    #[test]
    fn control_shift_up_and_down_move_the_start_never_past_the_shortest() {
        assert_eq!(stretch_start(10 * H, 11 * H, -1), (10 * H - 15 * M, 11 * H));
        assert_eq!(stretch_start(10 * H, 11 * H, 8), (11 * H - SHORTEST, 11 * H));
    }

    #[test]
    fn a_pointer_over_the_month_finds_its_day() {
        // Seven 100 px columns; week rows of 120, 80 and 100 px.
        let weeks = [120, 80, 100];
        assert_eq!(month_day_at(50.0, 10.0, 700.0, &weeks, false), Some(0));
        assert_eq!(month_day_at(650.0, 130.0, 700.0, &weeks, false), Some(13));
        assert_eq!(month_day_at(250.0, 299.0, 700.0, &weeks, false), Some(16));
    }

    #[test]
    fn a_right_to_left_month_counts_its_columns_from_the_right() {
        assert_eq!(month_day_at(50.0, 10.0, 700.0, &[120], true), Some(6));
    }

    #[test]
    fn a_pointer_off_the_month_finds_no_day() {
        let weeks = [120, 80];
        assert_eq!(month_day_at(-1.0, 10.0, 700.0, &weeks, false), None);
        assert_eq!(month_day_at(700.0, 10.0, 700.0, &weeks, false), None);
        assert_eq!(month_day_at(50.0, 200.0, 700.0, &weeks, false), None);
        assert_eq!(month_day_at(50.0, -3.0, 700.0, &weeks, false), None);
    }

    fn utc_day(y: i32, m: u32, d: u32) -> EpochMillis {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis()
    }

    #[test]
    fn an_all_day_event_moves_by_whole_utc_days() {
        let (start, end) = (utc_day(2026, 10, 1), utc_day(2026, 10, 3));
        let lisbon = chrono_tz::Europe::Lisbon;
        assert_eq!(by_days(start, end, true, 3, lisbon), (utc_day(2026, 10, 4), utc_day(2026, 10, 6)));
    }

    #[test]
    fn a_timed_event_moved_across_a_clock_change_keeps_its_wall_time() {
        let lisbon = chrono_tz::Europe::Lisbon;
        let start = lisbon.with_ymd_and_hms(2026, 10, 23, 10, 0, 0).unwrap().timestamp_millis();
        let (moved, _) = by_days(start, start + H, false, 3, lisbon);
        let after = lisbon.with_ymd_and_hms(2026, 10, 26, 10, 0, 0).unwrap().timestamp_millis();
        assert_eq!(moved, after);
    }

    #[test]
    fn a_timed_whole_day_card_moved_in_the_all_day_row_stays_timed_from_midnight() {
        let lisbon = chrono_tz::Europe::Lisbon;
        let midnight = |d| lisbon.with_ymd_and_hms(2026, 10, d, 0, 0, 0).unwrap().timestamp_millis();
        // The clocks go back in Lisbon on 25 October.
        let landing = strip_landing(midnight(24), midnight(25), false, None, 1, lisbon);
        assert_eq!(landing, Landing { start: midnight(25), end: midnight(26), all_day: false });
    }

    #[test]
    fn a_timed_whole_day_card_stretched_in_the_all_day_row_ends_at_midnight() {
        let lisbon = chrono_tz::Europe::Lisbon;
        let midnight = |d| lisbon.with_ymd_and_hms(2026, 10, d, 0, 0, 0).unwrap().timestamp_millis();
        let landing = strip_landing(midnight(24), midnight(25), false, Some(Edge::End), 2, lisbon);
        assert_eq!(landing, Landing { start: midnight(24), end: midnight(27), all_day: false });
        let landing = strip_landing(midnight(24), midnight(26), false, Some(Edge::Start), 5, lisbon);
        assert_eq!(landing, Landing { start: midnight(25), end: midnight(26), all_day: false });
    }

    #[test]
    fn an_all_day_card_moved_in_the_all_day_row_moves_by_utc_days() {
        let (start, end) = (utc_day(2026, 10, 1), utc_day(2026, 10, 3));
        let landing = strip_landing(start, end, true, None, 1, chrono_tz::Asia::Tokyo);
        assert_eq!(landing, Landing { start: utc_day(2026, 10, 2), end: utc_day(2026, 10, 4), all_day: true });
    }

    #[test]
    fn a_bar_s_end_stretches_by_whole_days() {
        let (start, end) = (utc_day(2026, 10, 1), utc_day(2026, 10, 3));
        assert_eq!(resize_days(start, end, Edge::End, 2), (start, utc_day(2026, 10, 5)));
        assert_eq!(resize_days(start, end, Edge::Start, -1), (utc_day(2026, 9, 30), end));
    }

    #[test]
    fn either_end_of_an_all_day_card_resizes_it() {
        assert_eq!(edge_at(3.0, 140.0), Some(Edge::Start));
        assert_eq!(edge_at(70.0, 140.0), None);
        assert_eq!(edge_at(135.0, 140.0), Some(Edge::End));
    }

    #[test]
    fn a_bar_never_shrinks_below_one_day() {
        let (start, end) = (utc_day(2026, 10, 1), utc_day(2026, 10, 3));
        assert_eq!(resize_days(start, end, Edge::End, -5), (start, utc_day(2026, 10, 2)));
        assert_eq!(resize_days(start, end, Edge::Start, 4), (utc_day(2026, 10, 2), end));
    }

    #[test]
    fn a_timed_event_dropped_on_the_all_day_row_covers_that_day() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        assert_eq!(all_day_on(day), (utc_day(2026, 10, 1), utc_day(2026, 10, 2)));
    }

    #[test]
    fn an_all_day_event_dropped_in_the_hours_lasts_an_hour_from_the_quarter_hour() {
        assert_eq!(timed_at(14 * H + 8 * M), (14 * H + 15 * M, 15 * H + 15 * M));
        assert_eq!(timed_at(14 * H + 7 * M), (14 * H, 15 * H));
    }

    #[test]
    fn the_grid_keys_move_resize_and_carry_the_focused_event() {
        use gtk::gdk::{Key, ModifierType as M};
        let shift = M::SHIFT_MASK;
        assert_eq!(grid_key(Key::Up, shift), Some(GridKey::Move(-1)));
        assert_eq!(grid_key(Key::Down, shift | M::ALT_MASK), Some(GridKey::End(1)));
        assert_eq!(grid_key(Key::Up, shift | M::CONTROL_MASK), Some(GridKey::Start(-1)));
        assert_eq!(grid_key(Key::Right, shift), Some(GridKey::Days(1)));
    }

    #[test]
    fn the_grid_keys_want_shift() {
        use gtk::gdk::{Key, ModifierType as M};
        assert_eq!(grid_key(Key::Up, M::empty()), None);
        assert_eq!(grid_key(Key::Up, M::CONTROL_MASK), None);
        assert_eq!(grid_key(Key::Up, M::SHIFT_MASK | M::CONTROL_MASK | M::ALT_MASK), None);
    }

    #[test]
    fn a_guest_s_own_event_cannot_be_dragged() {
        use mailrs_domain::calendar::{Event, Guest};
        use std::sync::Arc;
        let event = Event {
            guests: vec![
                Guest { email: "ana@example.com".into(), organizer: true, ..Guest::default() },
                Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
            ],
            ..Event::default()
        };
        let o = Occurrence { account_id: 1, event: Arc::new(event), start: 0, end: H };
        assert!(!can_move(&o, Access::Owner, true, false));
    }
}
