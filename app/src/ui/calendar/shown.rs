//! What the calendar view shows and how its controls read, worked out
//! without a widget: which grid a window of a given width gets, what the
//! view switch offers, what the arrows are called, where a step lands,
//! which events stay out, and which days the mini month marks.

use std::collections::HashSet;

use chrono::{DateTime, Days, Months, NaiveDate, TimeZone, Utc};
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::Occurrence;
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::gettext;

pub use super::range::ViewKind;

/// What the calendar page has on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Showing {
    Day,
    Week,
    Month,
    /// The agenda, which a narrow window shows in place of a week or a
    /// month.
    List,
}

/// The view a window gets for the grid the person picked. A narrow
/// window has no room for seven columns, so a week or a month becomes the
/// list; a day still fits.
pub fn showing(kind: ViewKind, narrow: bool) -> Showing {
    match (kind, narrow) {
        (ViewKind::Day, _) => Showing::Day,
        (_, true) => Showing::List,
        (ViewKind::Week, false) => Showing::Week,
        (ViewKind::Month, false) => Showing::Month,
    }
}

/// The view switch's toggles by name, in the order they sit, and whether
/// each one is offered: List and Day in a narrow window, Day, Week and
/// Month in a wide one.
pub fn offered(narrow: bool) -> [(&'static str, bool); 4] {
    [
        ("list", narrow),
        ("day", true),
        ("week", !narrow),
        ("month", !narrow),
    ]
}

/// The name of the toggle that stands for `showing`.
pub fn toggle_name(showing: Showing) -> &'static str {
    match showing {
        Showing::Day => "day",
        Showing::Week => "week",
        Showing::Month => "month",
        Showing::List => "list",
    }
}

/// The grid a toggle picks. List is not a grid of its own: it stands for
/// the week or month the person had before Day, `before_day`, which a
/// wider window shows again.
pub fn kind_for(name: &str, before_day: ViewKind) -> Option<ViewKind> {
    match name {
        "day" => Some(ViewKind::Day),
        "week" => Some(ViewKind::Week),
        "month" => Some(ViewKind::Month),
        "list" => Some(before_day),
        _ => None,
    }
}

/// The accessible names of the back and forward arrows, which are icons
/// alone.
pub fn arrow_names(showing: Showing) -> (String, String) {
    match showing {
        Showing::Day => (gettext("Previous Day"), gettext("Next Day")),
        Showing::Week => (gettext("Previous Week"), gettext("Next Week")),
        Showing::Month | Showing::List => (gettext("Previous Month"), gettext("Next Month")),
    }
}

/// The day the view holds after `by` steps of `kind` from `day`. A week
/// keeps its weekday and a month its date where the month has one, so
/// switching to Day after a few steps lands where the person was looking.
pub fn stepped(kind: ViewKind, day: NaiveDate, by: i32) -> NaiveDate {
    let count = by.unsigned_abs();
    let moved = match (kind, by >= 0) {
        (ViewKind::Day, true) => day.checked_add_days(Days::new(u64::from(count))),
        (ViewKind::Day, false) => day.checked_sub_days(Days::new(u64::from(count))),
        (ViewKind::Week, true) => day.checked_add_days(Days::new(7 * u64::from(count))),
        (ViewKind::Week, false) => day.checked_sub_days(Days::new(7 * u64::from(count))),
        (ViewKind::Month, true) => day.checked_add_months(Months::new(count)),
        (ViewKind::Month, false) => day.checked_sub_months(Months::new(count)),
    };
    moved.unwrap_or(day)
}

/// Whether an occurrence shows: one the person declined stays out unless
/// they asked to see declined events.
pub fn keep(o: &Occurrence, show_declined: bool) -> bool {
    show_declined || o.event.my_answer != Some(Answer::No)
}

/// The days among `count` days from `first` that hold an event, for the
/// mini month's dots. A timed event marks each local day it touches; an
/// all-day event its own UTC dates, never shifted by the zone.
pub fn busy_days<Z: TimeZone>(
    occurrences: &[Occurrence],
    first: NaiveDate,
    count: u32,
    zone: &Z,
) -> HashSet<NaiveDate> {
    let last = first + Days::new(u64::from(count.max(1) - 1));
    let mut busy = HashSet::new();
    for o in occurrences {
        let (Some(start), Some(end)) = (date_of(o.start, o.event.all_day, zone), {
            // The end is exclusive: an event that stops at midnight has
            // left the day that starts there.
            let end = if o.end > o.start { o.end - 1 } else { o.end };
            date_of(end, o.event.all_day, zone)
        }) else {
            continue;
        };
        let mut day = start.max(first);
        let stop = end.min(last);
        while day <= stop {
            busy.insert(day);
            day = day + Days::new(1);
        }
    }
    busy
}

/// The date `at` falls on: the UTC date for an all-day event, the local
/// date otherwise.
fn date_of<Z: TimeZone>(at: EpochMillis, all_day: bool, zone: &Z) -> Option<NaiveDate> {
    let utc = DateTime::<Utc>::from_timestamp_millis(at)?;
    Some(match all_day {
        true => utc.date_naive(),
        false => utc.with_timezone(zone).date_naive(),
    })
}

/// What a read of the days before the narrow list leaves to add: the
/// occurrences that end by `listed_from`, where the list's own reads
/// begin. The store returns every occurrence that overlaps a read, so
/// one that runs on into the listed days came back with those days
/// already, and adding it again would list it twice.
pub fn not_yet_listed(found: Vec<Occurrence>, listed_from: EpochMillis) -> Vec<Occurrence> {
    found.into_iter().filter(|o| o.end <= listed_from).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{NaiveDate, TimeZone};
    use mailrs_domain::calendar::{Event, Occurrence};
    use mailrs_domain::invitation::Answer;

    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn lisbon(y: i32, m: u32, day: u32, h: u32, min: u32) -> i64 {
        chrono_tz::Europe::Lisbon
            .with_ymd_and_hms(y, m, day, h, min, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn utc_midnight(date: NaiveDate) -> i64 {
        date.and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn an_earlier_read_drops_an_event_that_runs_into_the_listed_days() {
        // The list starts on the 25th; Lisbon offsite runs 24 to 25 and
        // came back with the first read already.
        let listed_from = lisbon(2026, 9, 25, 0, 0);
        let offsite = occurrence(true, utc_midnight(d(2026, 9, 24)), utc_midnight(d(2026, 9, 26)), None);
        let workshop = occurrence(false, lisbon(2026, 9, 24, 11, 0), lisbon(2026, 9, 24, 13, 0), None);
        let kept = not_yet_listed(vec![offsite, workshop.clone()], listed_from);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].start, workshop.start);
    }

    #[test]
    fn an_earlier_read_keeps_an_event_that_ends_as_the_list_begins() {
        let listed_from = lisbon(2026, 9, 25, 0, 0);
        let late = occurrence(false, lisbon(2026, 9, 24, 23, 0), listed_from, None);
        assert_eq!(not_yet_listed(vec![late], listed_from).len(), 1);
    }

    fn occurrence(all_day: bool, start: i64, end: i64, my_answer: Option<Answer>) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: Arc::new(Event {
                all_day,
                start,
                end,
                my_answer,
                ..Default::default()
            }),
            start,
            end,
        }
    }

    #[test]
    fn a_wide_window_shows_the_grid_it_was_asked_for() {
        assert_eq!(showing(ViewKind::Week, false), Showing::Week);
        assert_eq!(showing(ViewKind::Month, false), Showing::Month);
        assert_eq!(showing(ViewKind::Day, false), Showing::Day);
    }

    #[test]
    fn a_narrow_window_lists_in_place_of_the_week_and_the_month() {
        assert_eq!(showing(ViewKind::Week, true), Showing::List);
        assert_eq!(showing(ViewKind::Month, true), Showing::List);
        assert_eq!(showing(ViewKind::Day, true), Showing::Day);
    }

    #[test]
    fn a_narrow_switch_offers_list_and_day() {
        let on: Vec<&str> = offered(true)
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name)
            .collect();
        assert_eq!(on, ["list", "day"]);
    }

    #[test]
    fn a_wide_switch_offers_day_week_and_month() {
        let on: Vec<&str> = offered(false)
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name)
            .collect();
        assert_eq!(on, ["day", "week", "month"]);
    }

    #[test]
    fn each_view_names_the_toggle_that_shows_it() {
        for view in [Showing::Day, Showing::Week, Showing::Month, Showing::List] {
            let name = toggle_name(view);
            assert!(
                offered(view == Showing::List)
                    .iter()
                    .any(|(n, on)| *n == name && *on)
            );
        }
    }

    #[test]
    fn list_goes_back_to_the_grid_that_was_there_before_day() {
        assert_eq!(kind_for("list", ViewKind::Month), Some(ViewKind::Month));
        assert_eq!(kind_for("day", ViewKind::Month), Some(ViewKind::Day));
        assert_eq!(kind_for("week", ViewKind::Month), Some(ViewKind::Week));
        assert_eq!(kind_for("nothing", ViewKind::Month), None);
    }

    #[test]
    fn the_arrows_name_the_range_they_move_by() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(
            arrow_names(Showing::Week),
            ("Previous Week".to_string(), "Next Week".to_string())
        );
        assert_eq!(arrow_names(Showing::Day).0, "Previous Day");
        assert_eq!(arrow_names(Showing::Month).1, "Next Month");
    }

    #[test]
    fn a_step_keeps_the_weekday_and_a_month_keeps_the_date() {
        assert_eq!(stepped(ViewKind::Week, d(2026, 9, 23), 1), d(2026, 9, 30));
        assert_eq!(stepped(ViewKind::Day, d(2026, 9, 23), -1), d(2026, 9, 22));
        assert_eq!(stepped(ViewKind::Month, d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(stepped(ViewKind::Month, d(2026, 3, 15), -2), d(2026, 1, 15));
    }

    #[test]
    fn a_declined_event_stays_out_unless_asked_for() {
        let declined = occurrence(false, 0, 1, Some(Answer::No));
        let maybe = occurrence(false, 0, 1, Some(Answer::Maybe));
        assert!(!keep(&declined, false));
        assert!(keep(&declined, true));
        assert!(keep(&maybe, false));
    }

    #[test]
    fn busy_days_mark_each_local_day_a_timed_event_touches() {
        let late = occurrence(
            false,
            lisbon(2026, 9, 23, 22, 0),
            lisbon(2026, 9, 24, 1, 0),
            None,
        );
        let days = busy_days(&[late], d(2026, 9, 21), 42, &chrono_tz::Europe::Lisbon);
        let mut days: Vec<NaiveDate> = days.into_iter().collect();
        days.sort();
        assert_eq!(days, [d(2026, 9, 23), d(2026, 9, 24)]);
    }

    #[test]
    fn an_event_ending_at_midnight_leaves_the_next_day_free() {
        let evening = occurrence(
            false,
            lisbon(2026, 9, 23, 20, 0),
            lisbon(2026, 9, 24, 0, 0),
            None,
        );
        let days = busy_days(&[evening], d(2026, 9, 21), 42, &chrono_tz::Europe::Lisbon);
        assert_eq!(days.len(), 1);
        assert!(days.contains(&d(2026, 9, 23)));
    }

    #[test]
    fn busy_days_read_an_all_day_event_by_its_utc_dates() {
        // A two-day all-day event, which a zone west of UTC must not move
        // a day early.
        let trip = occurrence(
            true,
            utc_midnight(d(2026, 9, 24)),
            utc_midnight(d(2026, 9, 26)),
            None,
        );
        let days = busy_days(&[trip], d(2026, 9, 21), 42, &chrono_tz::America::New_York);
        let mut days: Vec<NaiveDate> = days.into_iter().collect();
        days.sort();
        assert_eq!(days, [d(2026, 9, 24), d(2026, 9, 25)]);
    }

    #[test]
    fn busy_days_stay_inside_the_days_asked_about() {
        let long = occurrence(
            true,
            utc_midnight(d(2026, 1, 1)),
            utc_midnight(d(2027, 1, 1)),
            None,
        );
        let days = busy_days(&[long], d(2026, 9, 21), 42, &chrono::Utc);
        assert_eq!(days.len(), 42);
    }
}
