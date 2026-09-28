//! The words the month grid, the agenda and the event popover show: when
//! an occurrence runs, how many guests said yes, what a "Join" button
//! reads, the map link a place opens, and the date words several widgets
//! share so a reader sees the same phrasing everywhere.

use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc};
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::repeat::{Custom, Ends, Frequency, Repeat};
use mailrs_domain::calendar::{Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::invitation::recurrence::in_words;
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

/// When a span runs, for the popover's time line and quick create's time
/// label: the date and the clock for a timed span, "All day" for one
/// that lasts a single day, or the first and last day for one that spans
/// several. An all-day span is dated from its own UTC date, never
/// converted to local time, which would move it a day west of UTC.
pub fn span_words<Z: TimeZone>(start: EpochMillis, end: EpochMillis, all_day: bool, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    if all_day {
        let (Some(first), Some(last)) = (
            utc_date(start),
            utc_date(end).and_then(|d| d.checked_sub_days(Days::new(1))),
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
                ("date", &full_date_words(local_date(start, zone))),
                ("start", &clock_words(start, zone)),
                ("end", &clock_words(end, zone)),
            ],
        )
    }
}

/// [`span_words`] of an occurrence's own start, end and all-day flag, for
/// the popover's time line.
pub fn when_words<Z: TimeZone>(o: &Occurrence, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    span_words(o.start, o.end, o.event.all_day, zone)
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

/// "Wed 15:00": the day and time the calendar sidebar's "Waiting for
/// your answer" card shows, short since the card has no room for the
/// full weekday. An all-day occurrence gives just the day, since it has
/// no clock to add.
pub fn waiting_when_words<Z: TimeZone>(start: EpochMillis, all_day: bool, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    let Some(day) = waiting_day(start, all_day, zone) else {
        return String::new();
    };
    let weekday = day.format_localized(&gettext("%a"), date_locale()).to_string();
    if all_day {
        weekday
    } else {
        fill(
            &gettext("{weekday} {time}"),
            &[("weekday", &weekday), ("time", &clock_words(start, zone))],
        )
    }
}

/// The full words for a "Waiting for your answer" card's own day and
/// time, read after its name: the day in full, then the clock unless
/// the occurrence runs all day.
pub fn waiting_card_detail<Z: TimeZone>(start: EpochMillis, all_day: bool, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    let Some(day) = waiting_day(start, all_day, zone) else {
        return String::new();
    };
    let day_text = day_words(day);
    if all_day {
        day_text
    } else {
        fill(
            &gettext("{day}, {time}"),
            &[("day", &day_text), ("time", &clock_words(start, zone))],
        )
    }
}

/// The day a "Waiting for your answer" card's own words read from: the
/// occurrence's own UTC date for an all-day one, never converted to
/// local time, as [`span_words`] reads it; the local day otherwise.
fn waiting_day<Z: TimeZone>(start: EpochMillis, all_day: bool, zone: &Z) -> Option<NaiveDate> {
    if all_day { utc_date(start) } else { Some(local_date(start, zone)) }
}

/// The "Waiting for your answer" card's own "Open mail" door, named so
/// several cards on screen read apart, as "Open mail" repeated on every
/// one would sound alike to a screen reader.
pub fn waiting_mail_name(title: &str) -> String {
    fill(
        &gettext("Open the invitation for “{title}” in Mail"),
        &[("title", title)],
    )
}

/// The reminder times the editor offers, in minutes before the start.
pub const REMINDER_CHOICES: [u32; 9] = [0, 5, 10, 15, 30, 60, 120, 1440, 10080];

/// "10 minutes before", in the largest whole unit that divides the time.
pub fn reminder_words(minutes: u32) -> String {
    let count = |n: u32| n.to_string();
    match minutes {
        0 => gettext("At the start"),
        m if m % 10080 == 0 => fill_plural(
            "{count} week before",
            "{count} weeks before",
            (m / 10080) as usize,
            &[("count", &count(m / 10080))],
        ),
        m if m % 1440 == 0 => fill_plural(
            "{count} day before",
            "{count} days before",
            (m / 1440) as usize,
            &[("count", &count(m / 1440))],
        ),
        m if m % 60 == 0 => fill_plural(
            "{count} hour before",
            "{count} hours before",
            (m / 60) as usize,
            &[("count", &count(m / 60))],
        ),
        m => fill_plural(
            "{count} minute before",
            "{count} minutes before",
            m as usize,
            &[("count", &count(m))],
        ),
    }
}

/// The repeat menu's line for `repeat`. The four fixed choices are said
/// through `fill_plural` with a count of one, since "Every day" and its
/// three companions already exist in the template as the singular of a
/// plural the invitation card counts with (`update-po.sh` refuses one
/// msgid used both ways). A custom repeat is said the way the invitation
/// card says a rule, `in_words`, so the two phrasings never drift apart
/// (ruling R6).
pub fn repeat_words(repeat: &Repeat) -> String {
    let once = |one: &str, many: &str| fill_plural(one, many, 1, &[("count", "1")]);
    match repeat {
        Repeat::Never => gettext("Never"),
        Repeat::EveryDay => once("Every day", "Every {count} days"),
        Repeat::EveryWeekday => gettext("Every weekday"),
        Repeat::EveryWeek => once("Every week", "Every {count} weeks"),
        Repeat::EveryMonth => once("Every month", "Every {count} months"),
        Repeat::EveryYear => once("Every year", "Every {count} years"),
        Repeat::Kept(_) => gettext("A rule set in another app"),
        Repeat::Custom(custom) => custom_words(custom),
    }
}

/// A custom repeat's line, read the way `in_words` reads an invitation's
/// `RRULE`. `repeat_words` has no day to hand `Repeat::rule`, but a
/// custom choice needs none: its own fields already carry everything a
/// rule needs (the weekdays, unlike a plain "every week", are explicit).
fn custom_words(custom: &Custom) -> String {
    let rule = custom_rule_value(custom);
    // No event dates it against, so the end date's own year is always
    // "the start year", which keeps a bare rule from carrying a year.
    let start_year = match custom.ends {
        Ends::On(last) => Some(last.year()),
        Ends::Never | Ends::After(_) => None,
    };
    in_words(&rule, start_year).unwrap_or_default()
}

/// The bare `RRULE` value `custom` names, as `in_words` reads one off an
/// invitation's `RRULE` property (no leading `RRULE:`).
fn custom_rule_value(custom: &Custom) -> String {
    let mut parts = vec![format!(
        "FREQ={}",
        match custom.frequency {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
        }
    )];
    if custom.every > 1 {
        parts.push(format!("INTERVAL={}", custom.every));
    }
    if custom.frequency == Frequency::Weekly && !custom.days.is_empty() {
        let mut days = custom.days.clone();
        days.sort_by_key(chrono::Weekday::num_days_from_monday);
        let codes: Vec<&str> = days.iter().copied().map(byday_code).collect();
        parts.push(format!("BYDAY={}", codes.join(",")));
    }
    match custom.ends {
        Ends::Never => {}
        Ends::On(last) => parts.push(format!("UNTIL={}", last.format("%Y%m%d"))),
        Ends::After(times) => parts.push(format!("COUNT={times}")),
    }
    parts.join(";")
}

/// The two-letter `BYDAY` code for a weekday.
fn byday_code(day: chrono::Weekday) -> &'static str {
    match day {
        chrono::Weekday::Mon => "MO",
        chrono::Weekday::Tue => "TU",
        chrono::Weekday::Wed => "WE",
        chrono::Weekday::Thu => "TH",
        chrono::Weekday::Fri => "FR",
        chrono::Weekday::Sat => "SA",
        chrono::Weekday::Sun => "SU",
    }
}

/// What a guest answered, in a word.
pub fn answer_words(guest: &Guest) -> String {
    if guest.organizer {
        return gettext("Organizer");
    }
    guest.answer.map_or_else(|| gettext("No answer yet"), Answer::said)
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

    #[test]
    fn reminders_read_in_the_largest_whole_unit() {
        let said: Vec<String> = [0, 1, 10, 60, 90, 120, 1440, 2880, 10080].into_iter().map(reminder_words).collect();
        assert_eq!(
            said,
            [
                "At the start",
                "1 minute before",
                "10 minutes before",
                "1 hour before",
                "90 minutes before",
                "2 hours before",
                "1 day before",
                "2 days before",
                "1 week before",
            ]
        );
    }

    #[test]
    fn a_repeat_reads_as_a_short_sentence() {
        use chrono::Weekday;
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(repeat_words(&Repeat::EveryWeekday), "Every weekday");
        let two_weeks = Repeat::Custom(Custom {
            every: 2,
            frequency: Frequency::Weekly,
            days: vec![Weekday::Wed, Weekday::Mon],
            ends: Ends::On(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
        });
        // The invitation card's own wording: no
        // comma before "until", and the end date's own year is always
        // taken as the start year, so it never shows.
        assert_eq!(repeat_words(&two_weeks), "Every 2 weeks on Monday and Wednesday until 31 December");
        let five = Repeat::Custom(Custom { every: 1, frequency: Frequency::Daily, days: vec![], ends: Ends::After(5) });
        assert_eq!(repeat_words(&five), "Every day, 5 times");
        assert_eq!(repeat_words(&Repeat::Kept("RRULE:FREQ=MONTHLY;BYDAY=1MO".into())), "A rule set in another app");
    }

    #[test]
    fn a_guest_answer_reads_as_a_word() {
        let guest = |answer, organizer| Guest { email: "ana@example.com".into(), answer, organizer, ..Guest::default() };
        assert_eq!(answer_words(&guest(Some(Answer::Yes), false)), "Going");
        assert_eq!(answer_words(&guest(Some(Answer::No), false)), "Not going");
        assert_eq!(answer_words(&guest(Some(Answer::Maybe), false)), "Maybe");
        assert_eq!(answer_words(&guest(None, false)), "No answer yet");
        assert_eq!(answer_words(&guest(Some(Answer::Yes), true)), "Organizer");
    }

    fn wednesday_at(hour: u32) -> EpochMillis {
        Utc.with_ymd_and_hms(2026, 9, 23, hour, 0, 0).unwrap().timestamp_millis()
    }

    #[test]
    fn waiting_when_words_gives_the_short_day_and_time() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(waiting_when_words(wednesday_at(15), false, &Utc), "Wed 15:00");
    }

    #[test]
    fn waiting_when_words_for_an_all_day_occurrence_gives_just_the_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(waiting_when_words(midnight(d(2026, 9, 23)), true, &Utc), "Wed");
    }

    #[test]
    fn waiting_card_detail_gives_the_full_day_and_time() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(waiting_card_detail(wednesday_at(15), false, &Utc), "Wednesday 23, 15:00");
    }

    #[test]
    fn waiting_card_detail_for_an_all_day_occurrence_gives_just_the_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(waiting_card_detail(midnight(d(2026, 9, 23)), true, &Utc), "Wednesday 23");
    }

    #[test]
    fn waiting_mail_name_names_the_invitation_it_opens() {
        assert_eq!(
            waiting_mail_name("Quarterly review"),
            "Open the invitation for “Quarterly review” in Mail"
        );
    }
}
