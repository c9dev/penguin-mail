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

use mailrs_domain::EpochMillis;

pub const STEP_MINUTES: i64 = 15;
const STEP: EpochMillis = STEP_MINUTES * 60_000;
/// The shortest event a drag makes.
pub const SHORTEST: EpochMillis = STEP;
/// How far up from a card's bottom edge a press stretches it.
pub const END_HANDLE: f64 = 8.0;
const HOUR: EpochMillis = 3_600_000;

/// The part of a card the pointer took hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// The body moves the event.
    Body,
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
            Handle::Body => start,
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
            Handle::End => (start, snap(end, STEP_MINUTES).clamp(start + SHORTEST, day.1.max(start + SHORTEST))),
        }
    }
}

/// Which part of a card `y` points at, `height` being the card's.
pub fn handle_at(y: f64, height: f64) -> Handle {
    if y >= height - END_HANDLE.min(height / 3.0) { Handle::End } else { Handle::Body }
}

/// The span a drag across empty time from `a` to `b` covers: whole
/// quarter hours, at least one.
pub fn selection(a: EpochMillis, b: EpochMillis) -> (EpochMillis, EpochMillis) {
    let start = a.min(b).div_euclid(STEP) * STEP;
    let end = (a.max(b) + STEP - 1).div_euclid(STEP) * STEP;
    (start, end.max(start + SHORTEST))
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
}
