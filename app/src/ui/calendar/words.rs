//! The words the month grid, the agenda and the event popover show: when
//! an occurrence runs, how many guests said yes, what a "Join" button
//! reads, the map link a place opens, and the date words several widgets
//! share so a reader sees the same phrasing everywhere.

use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc};
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext};

/// A day as a person reads it, with no year: "Wednesday 23 September".
/// Shared by the agenda's date headings, the popover's time line, the
/// month view's "N more" button and the mini month's day buttons.
pub fn full_date_words(date: NaiveDate) -> String {
    date.format_localized(&gettext("%A %-d %B"), date_locale())
        .to_string()
}

/// "Monday 21": a day of the week on screen, which needs no month.
pub fn day_words(date: NaiveDate) -> String {
    date.format_localized(&gettext("%A %-d"), date_locale())
        .to_string()
}

/// "10:00" in `zone`'s local time, the pattern the rest of the app clocks
/// a moment with.
pub fn clock_words<Z: TimeZone>(at: EpochMillis, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    utc(at)
        .map(|at| {
            at.with_timezone(zone)
                .format_localized(&gettext("%H:%M"), date_locale())
                .to_string()
        })
        .unwrap_or_default()
}

/// When an occurrence runs, for the popover's time line: the date and the
/// clock for a timed event, "All day" for one that lasts a single day,
/// or the first and last day for one that spans several. An all-day
/// event is dated from its own UTC date, never converted to local time
/// (reconcile.md, "Every task" item 8).
pub fn when_words<Z: TimeZone>(o: &Occurrence, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    if o.event.all_day {
        let (Some(first), Some(last)) = (
            utc_date(o.start),
            utc_date(o.end).and_then(|d| d.checked_sub_days(Days::new(1))),
        ) else {
            return String::new();
        };
        if first == last {
            gettext("All day")
        } else {
            all_day_range_words(first, last)
        }
    } else {
        fill(
            &gettext("{date} · {start}–{end}"),
            &[
                ("date", &full_date_words(local_date(o.start, zone))),
                ("start", &clock_words(o.start, zone)),
                ("end", &clock_words(o.end, zone)),
            ],
        )
    }
}

/// "Thursday 24 – Friday 25 September": the last day always carries its
/// month, and the first day carries one too only when it falls in a
/// different month.
fn all_day_range_words(first: NaiveDate, last: NaiveDate) -> String {
    let first_words = if first.year() == last.year() && first.month() == last.month() {
        day_words(first)
    } else {
        full_date_words(first)
    };
    fill(
        &gettext("{first} – {last}"),
        &[("first", &first_words), ("last", &full_date_words(last))],
    )
}

/// How many of `guests` said yes, for the popover's answer line: "4 of 6
/// said yes". The plural picks on the yes count, since that is the word
/// pt_PT conjugates ("disse" for one, "disseram" for several), not the
/// total.
pub fn answers_words(guests: &[Guest]) -> String {
    let yes = guests
        .iter()
        .filter(|g| g.answer == Some(Answer::Yes))
        .count();
    let count = guests.len();
    fill_plural(
        "{yes} of {count} said yes",
        "{yes} of {count} said yes",
        yes,
        &[("yes", &yes.to_string()), ("count", &count.to_string())],
    )
}

/// The popover's people line, as the mockup writes it: who organized the
/// event and how many guests said yes, "Rita Lopes, organizer · 4 of 6
/// said yes", either half alone when the other is missing.
pub fn people_words(organizer: Option<&str>, guests: &[Guest]) -> String {
    let organizer = organizer.map(|name| fill(&gettext("{name}, organizer"), &[("name", name)]));
    let answers = (!guests.is_empty()).then(|| answers_words(guests));
    match (organizer, answers) {
        (Some(organizer), Some(answers)) => fill(
            &gettext("{organizer} · {answers}"),
            &[("organizer", &organizer), ("answers", &answers)],
        ),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => String::new(),
    }
}

/// "and 3 more", for the popover's guest list once it passes five names.
pub fn more_guests_words(count: usize) -> String {
    fill_plural(
        "and {count} more",
        "and {count} more",
        count,
        &[("count", &count.to_string())],
    )
}

/// The full-width "Join" button's label: Google Meet gets its own name,
/// any other conference link a plain one.
pub fn join_words(link: &str) -> String {
    if is_meet(link) {
        gettext("Join with Google Meet")
    } else {
        gettext("Join Call")
    }
}

/// Whether `link` is a Google Meet call.
pub fn is_meet(link: &str) -> bool {
    link.contains("meet.google.com")
}

/// What the popover's place row says it does: it opens the place in a
/// map, though it shows only the place.
pub fn open_place_words(place: &str) -> String {
    fill(&gettext("Open {place} in Maps"), &[("place", place)])
}

/// Where the popover's place row opens: an OpenStreetMap search for
/// `place`, form-encoded the way a calendar event's free-text place
/// needs to be.
pub fn maps_url(place: &str) -> String {
    let query: String = url::form_urlencoded::byte_serialize(place.as_bytes()).collect();
    format!("https://www.openstreetmap.org/search?query={query}")
}

/// What a crowded month day's button shows: "3 more". Its spoken name is
/// [`month_more_words`], which names the day as well.
pub fn more_count_words(count: usize) -> String {
    fill_plural(
        "{count} more",
        "{count} more",
        count,
        &[("count", &count.to_string())],
    )
}

/// The month view's "N more" button, named with the day it opens since up
/// to 42 such buttons read alike otherwise.
pub fn month_more_words(count: usize, date: NaiveDate) -> String {
    fill_plural(
        "{count} more event on {date}",
        "{count} more events on {date}",
        count,
        &[
            ("count", &count.to_string()),
            ("date", &full_date_words(date)),
        ],
    )
}

/// A mini month day button's accessible name: the full date, with ", has
/// events" added for a day the dot marks.
pub fn mini_day_words(date: NaiveDate, has_events: bool) -> String {
    let base = full_date_words(date);
    if has_events {
        fill(&gettext("{date}, has events"), &[("date", &base)])
    } else {
        base
    }
}

fn utc(at: EpochMillis) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(at)
}

fn utc_date(at: EpochMillis) -> Option<NaiveDate> {
    utc(at).map(|at| at.date_naive())
}

fn local_date<Z: TimeZone>(at: EpochMillis, zone: &Z) -> NaiveDate {
    utc(at)
        .map(|at| at.with_timezone(zone).date_naive())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).expect("a valid date"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::calendar::Event;

    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn midnight(date: NaiveDate) -> EpochMillis {
        Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .timestamp_millis()
    }

    fn occurrence(all_day: bool, start: EpochMillis, end: EpochMillis) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: Arc::new(Event {
                all_day,
                start,
                end,
                ..Event::default()
            }),
            start,
            end,
        }
    }

    #[test]
    fn a_timed_occurrence_reads_the_date_and_the_clock() {
        mailrs_domain::translate::set_date_locale("en_US");
        let start = Utc
            .with_ymd_and_hms(2026, 9, 23, 15, 0, 0)
            .unwrap()
            .timestamp_millis();
        let end = Utc
            .with_ymd_and_hms(2026, 9, 23, 16, 0, 0)
            .unwrap()
            .timestamp_millis();
        let o = occurrence(false, start, end);
        assert_eq!(when_words(&o, &Utc), "Wednesday 23 September · 15:00–16:00");
    }

    #[test]
    fn a_single_day_all_day_occurrence_just_says_all_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        let o = occurrence(true, midnight(d(2026, 9, 23)), midnight(d(2026, 9, 24)));
        assert_eq!(when_words(&o, &Utc), "All day");
    }

    #[test]
    fn a_multi_day_all_day_occurrence_names_both_ends() {
        mailrs_domain::translate::set_date_locale("en_US");
        let o = occurrence(true, midnight(d(2026, 9, 24)), midnight(d(2026, 9, 26)));
        assert_eq!(when_words(&o, &Utc), "Thursday 24 – Friday 25 September");
    }

    fn guest(answer: Option<Answer>) -> Guest {
        Guest {
            answer,
            ..Default::default()
        }
    }

    #[test]
    fn answers_words_counts_who_said_yes() {
        let guests = vec![
            guest(Some(Answer::Yes)),
            guest(Some(Answer::Yes)),
            guest(Some(Answer::Yes)),
            guest(Some(Answer::Yes)),
            guest(Some(Answer::No)),
            guest(None),
        ];
        assert_eq!(answers_words(&guests), "4 of 6 said yes");
    }

    #[test]
    fn join_words_names_google_meet_apart_from_any_other_call() {
        assert_eq!(
            join_words("https://meet.google.com/abc-defg-hij"),
            "Join with Google Meet"
        );
        assert_eq!(join_words("https://zoom.example/call/1"), "Join Call");
    }

    #[test]
    fn is_meet_reads_a_google_meet_link() {
        assert!(is_meet("https://meet.google.com/abc-defg-hij"));
        assert!(!is_meet("https://zoom.example/meet/1"));
    }

    #[test]
    fn the_place_row_says_it_opens_the_place_in_maps() {
        assert_eq!(
            open_place_words("Room 2.04, Rua Augusta 24"),
            "Open Room 2.04, Rua Augusta 24 in Maps"
        );
    }

    #[test]
    fn maps_url_form_encodes_the_place() {
        assert_eq!(
            maps_url("Room 2.04, Rua Augusta 24"),
            "https://www.openstreetmap.org/search?query=Room+2.04%2C+Rua+Augusta+24"
        );
    }

    #[test]
    fn a_crowded_month_day_shows_a_short_count() {
        assert_eq!(more_count_words(3), "3 more");
    }

    #[test]
    fn month_more_words_names_the_day_and_takes_a_plural() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(
            month_more_words(1, d(2026, 9, 23)),
            "1 more event on Wednesday 23 September"
        );
        assert_eq!(
            month_more_words(3, d(2026, 9, 23)),
            "3 more events on Wednesday 23 September"
        );
    }

    #[test]
    fn mini_day_words_adds_has_events_only_for_a_busy_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(
            mini_day_words(d(2026, 9, 23), false),
            "Wednesday 23 September"
        );
        assert_eq!(
            mini_day_words(d(2026, 9, 23), true),
            "Wednesday 23 September, has events"
        );
    }

    #[test]
    fn more_guests_words_reads_and_n_more() {
        assert_eq!(more_guests_words(3), "and 3 more");
    }

    #[test]
    fn the_people_line_names_the_organizer_and_the_yes_count() {
        let guests = [
            Guest { answer: Some(Answer::Yes), ..Default::default() },
            Guest { answer: None, ..Default::default() },
        ];
        assert_eq!(
            people_words(Some("Rita Lopes"), &guests),
            "Rita Lopes, organizer · 1 of 2 said yes"
        );
    }

    #[test]
    fn the_people_line_without_an_organizer_counts_the_answers() {
        let guests = [Guest { answer: Some(Answer::Yes), ..Default::default() }];
        assert_eq!(people_words(None, &guests), "1 of 1 said yes");
        assert_eq!(people_words(Some("Rita Lopes"), &[]), "Rita Lopes, organizer");
    }
}
