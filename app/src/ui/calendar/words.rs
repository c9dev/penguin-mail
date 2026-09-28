//! The words the month grid, the agenda and the event popover show: when
//! an occurrence runs, how many guests said yes, what a "Join" button
//! reads, the map link a place opens, and the date words several widgets
//! share so a reader sees the same phrasing everywhere.

use std::fmt::Write as _;

use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::repeat::{Custom, Ends, Frequency, Repeat};
use mailrs_domain::calendar::{Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::invitation::recurrence::in_words;
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext};

/// Past this many of the notes' own lines, or this many characters on
/// one line the label would still wrap over several screen lines, the
/// popover's notes row shows a "Show more" button and
/// [`notes_collapsed`] cuts the rest from what it shows first.
const MOST_NOTE_LINES: usize = 4;
const MOST_NOTE_CHARS: usize = 220;

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

/// "10:00" or "10:00 AM" in `zone`'s local time, in the clock
/// [`crate::clock_format::current`] names.
pub fn clock_words<Z: TimeZone>(at: EpochMillis, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    utc(at)
        .map(|at| crate::clock_format::time_text(at.with_timezone(zone).time()))
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

/// A month bar's spoken name: the title, the days the whole event
/// covers, and its calendar. The bar in each week row says the whole
/// span, so a reader on its second row still hears where it began.
pub fn bar_words<Z: TimeZone>(o: &Occurrence, calendar: &str, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    let title = &o.event.title;
    if o.event.all_day {
        fill(
            &gettext("{title}, {days}, all day, {calendar}"),
            &[
                ("title", title),
                ("days", &span_words(o.start, o.end, true, zone)),
                ("calendar", calendar),
            ],
        )
    } else {
        let at = |instant: EpochMillis| {
            fill(
                &gettext("{date} {time}"),
                &[
                    ("date", &full_date_words(local_date(instant, zone))),
                    ("time", &clock_words(instant, zone)),
                ],
            )
        };
        fill(
            &gettext("{title}, {start} to {end}, {calendar}"),
            &[("title", title), ("start", &at(o.start)), ("end", &at(o.end)), ("calendar", calendar)],
        )
    }
}

/// The agenda row's time column: "All day", or the start and end clock
/// with no date, which the row's own heading already carries.
pub fn agenda_span_words<Z: TimeZone>(o: &Occurrence, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    if o.event.all_day {
        gettext("All day")
    } else {
        fill(
            &gettext("{start}–{end}"),
            &[("start", &clock_words(o.start, zone)), ("end", &clock_words(o.end, zone))],
        )
    }
}

/// The agenda row's dimmed second line: the place and the calendar name
/// together when both are known, whichever one is known alone, or
/// nothing when neither is.
pub fn agenda_subtitle_words(place: &str, calendar: &str) -> String {
    match (place.is_empty(), calendar.is_empty()) {
        (false, false) => fill(&gettext("{place} · {calendar}"), &[("place", place), ("calendar", calendar)]),
        (false, true) => place.to_string(),
        (true, false) => calendar.to_string(),
        (true, true) => String::new(),
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
/// card says a rule, `in_words`, so the two phrasings never drift apart.
pub fn repeat_words(repeat: &Repeat) -> String {
    let once = |one: &str, many: &str| fill_plural(one, many, 1, &[("count", "1")]);
    match repeat {
        Repeat::Never => gettext("Never"),
        Repeat::EveryDay => once("Every day", "Every {count} days"),
        Repeat::EveryWeekday => gettext("Every weekday"),
        Repeat::EveryWeek => once("Every week", "Every {count} weeks"),
        Repeat::EveryMonth => once("Every month", "Every {count} months"),
        Repeat::EveryYear => once("Every year", "Every {count} years"),
        Repeat::MonthlyByDay(ordinal, weekday) => monthly_by_day_words(*ordinal, *weekday),
        Repeat::Kept(_) => gettext("A rule set in another app"),
        Repeat::Custom(custom) => custom_words(custom),
    }
}

/// "Monthly on the second Tuesday" or "Monthly on the last Friday", said
/// the way `in_words` reads an invitation's own monthly ordinal `BYDAY`,
/// so the two phrasings never drift apart.
fn monthly_by_day_words(ordinal: i8, weekday: chrono::Weekday) -> String {
    let rule = format!("FREQ=MONTHLY;BYDAY={ordinal}{}", byday_code(weekday));
    in_words(&rule, None).unwrap_or_default()
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

/// What a guest answered, in a word. The editor's own guest list reads
/// the organizer this way, so the answer never shows once it has said
/// who ran the meeting; the popover's guest list marks the two apart
/// instead ([`guest_name_words`], [`guest_answer_words`]).
pub fn answer_words(guest: &Guest) -> String {
    if guest.organizer {
        return gettext("Organizer");
    }
    guest_answer_words(guest)
}

/// What a guest answered, ignoring whether they organized the event:
/// "Going", "Not going", "Maybe", or "No answer yet". The popover's
/// guest list marks the organizer beside their name instead
/// ([`guest_name_words`]), so their own answer still reads here.
pub fn guest_answer_words(guest: &Guest) -> String {
    guest.answer.map_or_else(|| gettext("No answer yet"), Answer::said)
}

/// The symbolic icon a guest's answer shows in the popover's list: a
/// check for yes, a question mark for maybe, a cross for no, and a
/// loading ring for nobody has answered yet.
pub fn answer_icon(answer: Option<Answer>) -> &'static str {
    match answer {
        Some(Answer::Yes) => "object-select-symbolic",
        Some(Answer::Maybe) => "dialog-question-symbolic",
        Some(Answer::No) => "process-stop-symbolic",
        None => "content-loading-symbolic",
    }
}

/// A guest row's own name: their name, or their address when they gave
/// none, with ", organizer" added for whoever organized the event, the
/// same words the summary line already gives the organizer
/// ([`people_words`]).
pub fn guest_name_words(guest: &Guest) -> String {
    let shown = guest.name.clone().unwrap_or_else(|| guest.email.clone());
    if guest.organizer {
        fill(&gettext("{name}, organizer"), &[("name", &shown)])
    } else {
        shown
    }
}

/// Whether the notes need a "Show more" button: past a few of their own
/// lines, or a single line long enough that the label would still wrap
/// it over several screen lines.
pub fn notes_need_more(notes: &str) -> bool {
    notes.lines().count() > MOST_NOTE_LINES || notes.chars().count() > MOST_NOTE_CHARS
}

/// The notes cut down to what the popover's collapsed row shows: its
/// first few lines, cut again to a character count past which one of
/// those lines would still wrap the label over several screen lines of
/// its own. The notes come back whole when [`notes_need_more`] is
/// false, so "Show more" only ever reveals text this did not already
/// show.
pub fn notes_collapsed(notes: &str) -> String {
    if !notes_need_more(notes) {
        return notes.to_string();
    }
    let lines: String = notes.lines().take(MOST_NOTE_LINES).collect::<Vec<_>>().join("\n");
    if lines.chars().count() <= MOST_NOTE_CHARS {
        return lines;
    }
    let cut: String = lines.chars().take(MOST_NOTE_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// The notes as Pango markup for the popover's label: the text escaped,
/// with each `http` or `https` address turned into a link a click can
/// open. [`mailrs_mime::notes::text`] has already turned Google's HTML
/// into these lines; this only makes the addresses left in them
/// clickable.
pub fn notes_markup(notes: &str) -> String {
    let mut out = String::with_capacity(notes.len());
    let mut rest = notes;
    while let Some(start) = find_url(rest) {
        out.push_str(&escape_markup(&rest[..start]));
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace())
            .unwrap_or(candidate.len());
        // Punctuation after the address ends the sentence, not the link.
        let url = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        let escaped = escape_markup(url);
        let _ = write!(out, "<a href=\"{escaped}\">{escaped}</a>");
        rest = &candidate[url.len()..];
    }
    out.push_str(&escape_markup(rest));
    out
}

/// `text` with the characters Pango markup reads as tags written as
/// references, so plain text the notes carry never opens a tag of its
/// own.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Where the next `http://` or `https://` address starts in `s`, if any.
fn find_url(s: &str) -> Option<usize> {
    [s.find("https://"), s.find("http://")].into_iter().flatten().min()
}

/// Whether the occurrence repeats, in the editor's own words ("Weekly
/// on Wednesday", "Every weekday"), or `None` for one that does not, so
/// the popover's row only shows for a series. `zone` is the event's own
/// zone when it names one the day starts from, matching
/// [`Repeat::read`], which the editor's own draft calls the same way.
pub fn series_words(rules: &[String], start: EpochMillis, zone: Tz) -> Option<String> {
    let day = local_date(start, &zone);
    let repeat = Repeat::read(rules, day, zone);
    (repeat != Repeat::Never).then(|| repeat_words(&repeat))
}

/// The event's own time added beside the desktop's, for the popover:
/// "09:00 New York" once `event_zone` differs from `desktop_zone`.
/// `None` when they are the same zone, so the popover names the time
/// once. The time reads through the shared clock formatting
/// ([`crate::clock_format::time_text`]), the same 12- or 24-hour choice
/// the desktop's own time already shows in.
pub fn own_zone_words(start: EpochMillis, event_zone: Tz, desktop_zone: Tz) -> Option<String> {
    if event_zone == desktop_zone {
        return None;
    }
    let time = utc(start)?.with_timezone(&event_zone).time();
    Some(fill(
        &gettext("{time} {city}"),
        &[
            ("time", &crate::clock_format::time_text(time)),
            ("city", &zone_city(event_zone)),
        ],
    ))
}

/// The city an IANA zone id ends in, for a reader who does not parse
/// zone ids: "New York" for `America/New_York`, "Lisbon" for
/// `Europe/Lisbon`.
fn zone_city(zone: Tz) -> String {
    zone.name()
        .rsplit('/')
        .next()
        .unwrap_or_else(|| zone.name())
        .replace('_', " ")
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
    fn a_bar_names_the_days_an_all_day_event_covers() {
        mailrs_domain::translate::set_date_locale("en_US");
        let mut o = occurrence(true, midnight(d(2026, 10, 2)), midnight(d(2026, 10, 5)));
        Arc::make_mut(&mut o.event).title = "Lisbon offsite".into();
        assert_eq!(
            bar_words(&o, "Work", &Utc),
            "Lisbon offsite, Friday 2 – Sunday 4 October, all day, Work"
        );
    }

    #[test]
    fn a_bar_names_both_days_and_clocks_of_a_timed_event() {
        mailrs_domain::translate::set_date_locale("en_US");
        let start = Utc.with_ymd_and_hms(2026, 10, 2, 22, 0, 0).unwrap().timestamp_millis();
        let end = Utc.with_ymd_and_hms(2026, 10, 3, 2, 0, 0).unwrap().timestamp_millis();
        let mut o = occurrence(false, start, end);
        Arc::make_mut(&mut o.event).title = "Night shift".into();
        assert_eq!(
            bar_words(&o, "Work", &Utc),
            "Night shift, Friday 2 October 22:00 to Saturday 3 October 02:00, Work"
        );
    }

    #[test]
    fn an_agenda_row_times_a_timed_occurrence_with_no_date() {
        mailrs_domain::translate::set_date_locale("en_US");
        let start = Utc.with_ymd_and_hms(2026, 9, 23, 15, 0, 0).unwrap().timestamp_millis();
        let end = Utc.with_ymd_and_hms(2026, 9, 23, 16, 0, 0).unwrap().timestamp_millis();
        let o = occurrence(false, start, end);
        assert_eq!(agenda_span_words(&o, &Utc), "15:00–16:00");
    }

    #[test]
    fn an_agenda_row_times_an_all_day_occurrence_as_all_day() {
        let o = occurrence(true, midnight(d(2026, 9, 23)), midnight(d(2026, 9, 24)));
        assert_eq!(agenda_span_words(&o, &Utc), "All day");
    }

    #[test]
    fn an_agenda_subtitle_joins_the_place_and_the_calendar() {
        assert_eq!(agenda_subtitle_words("Room 5", "Work"), "Room 5 · Work");
    }

    #[test]
    fn an_agenda_subtitle_with_no_place_is_just_the_calendar() {
        assert_eq!(agenda_subtitle_words("", "Work"), "Work");
    }

    #[test]
    fn an_agenda_subtitle_with_no_calendar_is_just_the_place() {
        assert_eq!(agenda_subtitle_words("Room 5", ""), "Room 5");
    }

    #[test]
    fn an_agenda_subtitle_with_neither_is_empty() {
        assert_eq!(agenda_subtitle_words("", ""), "");
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
        assert_eq!(repeat_words(&Repeat::Kept("RRULE:FREQ=MONTHLY;BYDAY=1MO,3MO".into())), "A rule set in another app");
    }

    #[test]
    fn a_monthly_ordinal_choice_reads_as_the_nth_or_the_last_weekday() {
        use chrono::Weekday;
        mailrs_domain::translate::set_date_locale("en_US");
        assert_eq!(
            repeat_words(&Repeat::MonthlyByDay(2, Weekday::Tue)),
            "Every month on the second Tuesday"
        );
        assert_eq!(
            repeat_words(&Repeat::MonthlyByDay(-1, Weekday::Fri)),
            "Every month on the last Friday"
        );
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

    #[test]
    fn guest_answer_words_reads_the_answer_even_for_the_organizer() {
        let guest = |answer| Guest { email: "ana@example.com".into(), answer, organizer: true, ..Guest::default() };
        assert_eq!(guest_answer_words(&guest(Some(Answer::Yes))), "Going");
        assert_eq!(guest_answer_words(&guest(None)), "No answer yet");
    }

    #[test]
    fn answer_icon_names_a_symbolic_icon_for_each_answer() {
        assert_eq!(answer_icon(Some(Answer::Yes)), "object-select-symbolic");
        assert_eq!(answer_icon(Some(Answer::Maybe)), "dialog-question-symbolic");
        assert_eq!(answer_icon(Some(Answer::No)), "process-stop-symbolic");
        assert_eq!(answer_icon(None), "content-loading-symbolic");
    }

    #[test]
    fn guest_name_words_marks_the_organizer_and_falls_back_to_the_address() {
        let named = Guest { name: Some("Rita Lopes".into()), organizer: true, ..Guest::default() };
        assert_eq!(guest_name_words(&named), "Rita Lopes, organizer");
        let unnamed = Guest { email: "ana@example.com".into(), ..Guest::default() };
        assert_eq!(guest_name_words(&unnamed), "ana@example.com");
    }

    #[test]
    fn notes_need_more_reads_true_past_the_line_cap() {
        assert!(!notes_need_more("Room 5\nDial in: 555-0100"));
        assert!(notes_need_more("One\nTwo\nThree\nFour\nFive"));
    }

    #[test]
    fn notes_need_more_reads_true_for_one_very_long_line() {
        assert!(notes_need_more(&"word ".repeat(60)));
    }

    #[test]
    fn notes_collapsed_gives_the_notes_whole_under_the_cap() {
        assert_eq!(notes_collapsed("Room 5\nDial in: 555-0100"), "Room 5\nDial in: 555-0100");
    }

    #[test]
    fn notes_collapsed_cuts_to_the_first_few_lines() {
        assert_eq!(notes_collapsed("One\nTwo\nThree\nFour\nFive\nSix"), "One\nTwo\nThree\nFour");
    }

    #[test]
    fn notes_collapsed_cuts_one_very_long_line_to_the_character_cap() {
        let long = "word ".repeat(60);
        let collapsed = notes_collapsed(&long);
        assert!(collapsed.ends_with('…'));
        assert!(collapsed.chars().count() <= MOST_NOTE_CHARS + 1);
    }

    #[test]
    fn notes_markup_escapes_plain_text() {
        assert_eq!(notes_markup("Tom & Jerry <3"), "Tom &amp; Jerry &lt;3");
    }

    #[test]
    fn notes_markup_links_an_address_and_keeps_trailing_punctuation_out() {
        assert_eq!(
            notes_markup("Dial in at https://meet.example.com/room, then wait."),
            "Dial in at <a href=\"https://meet.example.com/room\">https://meet.example.com/room</a>, then wait."
        );
    }

    #[test]
    fn notes_markup_escapes_an_address_that_itself_needs_escaping() {
        assert_eq!(
            notes_markup("https://example.com/a?b=1&c=2"),
            "<a href=\"https://example.com/a?b=1&amp;c=2\">https://example.com/a?b=1&amp;c=2</a>"
        );
    }

    #[test]
    fn series_words_names_a_weekly_repeat() {
        mailrs_domain::translate::set_date_locale("en_US");
        let rules = vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,WE".to_string()];
        assert_eq!(
            series_words(&rules, wednesday_at(15), Tz::UTC),
            Some("Every Monday and Wednesday".to_string())
        );
    }

    #[test]
    fn series_words_gives_none_for_an_event_that_does_not_repeat() {
        assert_eq!(series_words(&[], wednesday_at(15), Tz::UTC), None);
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
    fn own_zone_words_names_the_event_s_own_zone_when_it_differs() {
        mailrs_domain::translate::set_date_locale("en_US");
        let start = Utc.with_ymd_and_hms(2026, 9, 23, 15, 0, 0).unwrap().timestamp_millis();
        // 15:00 UTC is 11:00 in New York, on Eastern Daylight Time in
        // September.
        assert_eq!(
            own_zone_words(start, Tz::America__New_York, Tz::Europe__Lisbon),
            Some("11:00 New York".to_string())
        );
    }

    #[test]
    fn own_zone_words_says_nothing_for_the_desktop_s_own_zone() {
        let start = Utc.with_ymd_and_hms(2026, 9, 23, 15, 0, 0).unwrap().timestamp_millis();
        assert_eq!(own_zone_words(start, Tz::Europe__Lisbon, Tz::Europe__Lisbon), None);
    }

    #[test]
    fn zone_city_reads_the_last_segment_of_a_zone_id() {
        assert_eq!(zone_city(Tz::America__New_York), "New York");
        assert_eq!(zone_city(Tz::Europe__Lisbon), "Lisbon");
    }

    #[test]
    fn waiting_mail_name_names_the_invitation_it_opens() {
        assert_eq!(
            waiting_mail_name("Quarterly review"),
            "Open the invitation for “Quarterly review” in Mail"
        );
    }
}
