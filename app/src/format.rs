//! Text for the UI: dates, sizes, initials, and colours.
//!
//! A date pattern is translated whole, so a language that puts the time
//! before the day can. The weekday and month names that `%A`, `%a`, `%B`
//! and `%b` stand for come in the language of the catalogue the interface
//! reads its words from, so one date never mixes two languages.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use mailrs_domain::invitation::When;
use mailrs_domain::translate::{date_locale, fill, gettext};
use mailrs_domain::{AccountId, EpochMillis};

/// Accent colours from the libadwaita palette.
pub const PALETTE: [&str; 9] = [
    "#3584e4", "#2190a4", "#3a944a", "#c88800", "#ed5b00", "#e62d42", "#d56199", "#9141ac",
    "#6f8396",
];

pub fn local(ts: EpochMillis) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ts).single()
}

/// A short date for list rows: the time today, then "Yesterday", then the
/// weekday for the past week, then day and month, then the full date for
/// earlier years.
pub fn relative_date(ts: EpochMillis, now: DateTime<Local>) -> String {
    let Some(when) = local(ts) else {
        return String::new();
    };
    let days = (now.date_naive() - when.date_naive()).num_days();
    let pattern = match days {
        ..=0 => gettext("%H:%M"),
        1 => return gettext("Yesterday"),
        2..=6 => gettext("%A"),
        _ if when.year() == now.year() => gettext("%-d %b"),
        _ => gettext("%Y-%m-%d"),
    };
    when.format_localized(&pattern, date_locale()).to_string()
}

/// A date for message headers: "Today at 10:12", "Yesterday at 14:50",
/// "Fri 18 Sep at 08:50", or "3 Sep 2024" for earlier years.
pub fn header_date(ts: EpochMillis, now: DateTime<Local>) -> String {
    let Some(when) = local(ts) else {
        return String::new();
    };
    let pattern = match (now.date_naive() - when.date_naive()).num_days() {
        ..=0 => gettext("Today at %H:%M"),
        1 => gettext("Yesterday at %H:%M"),
        _ if when.year() == now.year() => gettext("%a %-d %b at %H:%M"),
        _ => gettext("%-d %b %Y"),
    };
    when.format_localized(&pattern, date_locale()).to_string()
}

/// A long date for reply attributions and forwarded headers.
pub fn full_date(ts: EpochMillis) -> String {
    local(ts)
        .map(|when| {
            when.format_localized(&gettext("%A, %-d %B %Y at %H:%M"), date_locale())
                .to_string()
        })
        .unwrap_or_default()
}

/// When a scheduled message goes out: "today at 21:00", "tomorrow at
/// 08:00", "Monday at 08:00" within the week, then "Tue 3 Nov at 08:00".
pub fn future_date(ts: EpochMillis, now: DateTime<Local>) -> String {
    let Some(when) = local(ts) else {
        return String::new();
    };
    let pattern = match (when.date_naive() - now.date_naive()).num_days() {
        ..=0 => gettext("today at %H:%M"),
        1 => gettext("tomorrow at %H:%M"),
        2..=6 => gettext("%A at %H:%M"),
        _ if when.year() == now.year() => gettext("%a %-d %b at %H:%M"),
        _ => gettext("%-d %b %Y at %H:%M"),
    };
    when.format_localized(&pattern, date_locale()).to_string()
}

/// Apple Mail's Send Later presets: tonight at 21:00 while there is time,
/// tomorrow at 08:00, and next Monday at 08:00 when that is not tomorrow.
pub fn send_later_presets(now: DateTime<Local>) -> Vec<(String, EpochMillis)> {
    later_presets(now)
        .into_iter()
        .map(|(label, at)| (fill(&gettext("Send {when}"), &[("when", &label)]), at))
        .collect()
}

/// Remind Me's presets: an hour from now, then the Send Later times.
pub fn remind_presets(now: DateTime<Local>) -> Vec<(String, EpochMillis)> {
    let mut presets = vec![(
        gettext("In 1 Hour"),
        now.timestamp_millis() + 60 * 60 * 1000,
    )];
    presets.extend(later_presets(now));
    presets
}

/// Tonight at 21:00 while there is time, tomorrow at 08:00, and next
/// Monday at 08:00 when that is not tomorrow.
fn later_presets(now: DateTime<Local>) -> Vec<(String, EpochMillis)> {
    let at = |date: chrono::NaiveDate, hour: u32| {
        date.and_hms_opt(hour, 0, 0)
            .and_then(|t| Local.from_local_datetime(&t).earliest())
            .map(|t| t.timestamp_millis())
    };
    let today = now.date_naive();
    let mut presets = Vec::new();
    if now.hour() < 20
        && let Some(ts) = at(today, 21)
    {
        presets.push((gettext("Tonight at 21:00"), ts));
    }
    let tomorrow = today + chrono::Days::new(1);
    if let Some(ts) = at(tomorrow, 8) {
        presets.push((gettext("Tomorrow at 08:00"), ts));
    }
    let to_monday = (7 - today.weekday().num_days_from_monday()) % 7;
    let monday = today + chrono::Days::new(if to_monday == 0 { 7 } else { to_monday as u64 });
    if monday != tomorrow
        && let Some(ts) = at(monday, 8)
    {
        presets.push((gettext("Monday at 08:00"), ts));
    }
    presets
}

/// When an event runs, in the reader's own time zone: "Tuesday, 9 June ·
/// 15:00 to 16:00". A meeting in the next few days is named by its
/// weekday, since that is how people talk about one.
pub fn event_when(when: &When, now: DateTime<Local>) -> String {
    match when {
        When::Days { first, last } if first == last => fill(
            &gettext("{day} · All day"),
            &[("day", &event_day(*first, now))],
        ),
        When::Days { first, last } => fill(
            &gettext("{first} to {last} · All day"),
            &[
                ("first", &span_start(*first, *last)),
                ("last", &span_end(*last, now)),
            ],
        ),
        When::At { starts_at, ends_at } => {
            let Some(start) = local(*starts_at) else {
                return String::new();
            };
            let day = event_day(start.date_naive(), now);
            let from = start.format("%H:%M").to_string();
            match ends_at.and_then(local) {
                None => fill(
                    &gettext("{day} · {start}"),
                    &[("day", &day), ("start", &from)],
                ),
                // A meeting that runs past midnight names the day it ends on.
                Some(end) if end.date_naive() != start.date_naive() => fill(
                    &gettext("{day} · {start} to {end}"),
                    &[
                        ("day", &day),
                        ("start", &from),
                        (
                            "end",
                            &end.format_localized(&gettext("%-d %b %H:%M"), date_locale())
                                .to_string(),
                        ),
                    ],
                ),
                Some(end) => fill(
                    &gettext("{day} · {start} to {end}"),
                    &[
                        ("day", &day),
                        ("start", &from),
                        ("end", &end.format("%H:%M").to_string()),
                    ],
                ),
            }
        }
    }
}

/// The start of a run of days, with the month left off while both ends
/// share it: "14" in "14 to 16 July", "30 June" in "30 June to 2 July".
fn span_start(first: NaiveDate, last: NaiveDate) -> String {
    if first.month() == last.month() && first.year() == last.year() {
        first.format("%-d").to_string()
    } else {
        first
            .format_localized(&gettext("%-d %B"), date_locale())
            .to_string()
    }
}

/// The end of a run of days: "16 July", with the year when the reader is
/// in another one.
fn span_end(last: NaiveDate, now: DateTime<Local>) -> String {
    if last.year() == now.year() {
        last.format_localized(&gettext("%-d %B"), date_locale())
            .to_string()
    } else {
        last.format_localized(&gettext("%-d %B %Y"), date_locale())
            .to_string()
    }
}

/// The day an event falls on: "Today", "Tomorrow", a weekday within the
/// week, then the date.
fn event_day(day: NaiveDate, now: DateTime<Local>) -> String {
    let pattern = match (day - now.date_naive()).num_days() {
        0 => return gettext("Today"),
        1 => return gettext("Tomorrow"),
        -1 => return gettext("Yesterday"),
        2..=6 => gettext("%A"),
        _ if day.year() == now.year() => gettext("%A, %-d %B"),
        _ => gettext("%A, %-d %B %Y"),
    };
    day.format_localized(&pattern, date_locale()).to_string()
}

/// The start an event had before it moved, for the line that says so:
/// "Tuesday 10:00", or "Tuesday, 14 July" for an all-day one.
pub fn event_moved_from(was: EpochMillis, all_day: bool, now: DateTime<Local>) -> String {
    let Some(start) = local(was) else {
        return String::new();
    };
    let day = event_day(start.date_naive(), now);
    if all_day {
        day
    } else {
        fill(
            &gettext("{day} {time}"),
            &[("day", &day), ("time", &start.format("%H:%M").to_string())],
        )
    }
}

/// The month and day for the card's date tile: ("JUN", "9").
pub fn event_tile(when: &When) -> (String, String) {
    let day = match when {
        When::Days { first, .. } => *first,
        When::At { starts_at, .. } => match local(*starts_at) {
            Some(start) => start.date_naive(),
            None => return (String::new(), String::new()),
        },
    };
    (
        day.format_localized("%b", date_locale())
            .to_string()
            .to_uppercase(),
        day.format("%-d").to_string(),
    )
}

pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// One or two letters for an avatar: first and last word of a name, or the
/// parts of an address's local part.
pub fn initials(display: &str) -> String {
    let base = display.split('@').next().unwrap_or(display);
    let words: Vec<&str> = base
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-' | '"' | '\''))
        .filter(|w| w.chars().next().is_some_and(char::is_alphanumeric))
        .collect();
    let first_letter = |w: &str| {
        w.chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_default()
    };
    match words.as_slice() {
        [] => "?".into(),
        [only] => first_letter(only),
        [first, .., last] => first_letter(first) + &first_letter(last),
    }
}

/// A stable palette colour for a string, such as a sender address.
pub fn color_for(seed: &str) -> &'static str {
    let hash = seed
        .to_lowercase()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
    PALETTE[(hash % PALETTE.len() as u64) as usize]
}

/// What the account colour menu calls the [`PALETTE`] colour at `index`.
pub fn palette_name(index: usize) -> String {
    match index {
        0 => gettext("Blue"),
        1 => gettext("Teal"),
        2 => gettext("Green"),
        3 => gettext("Yellow"),
        4 => gettext("Orange"),
        5 => gettext("Red"),
        6 => gettext("Pink"),
        7 => gettext("Purple"),
        _ => gettext("Slate"),
    }
}

thread_local! {
    /// Colours chosen in the account menu, by account.
    static ACCOUNT_COLORS: std::cell::RefCell<std::collections::HashMap<AccountId, usize>> =
        Default::default();
}

/// Replaces the chosen account colours.
pub fn set_account_colors(colors: std::collections::HashMap<AccountId, usize>) {
    ACCOUNT_COLORS.with(|c| *c.borrow_mut() = colors);
}

/// Each account's colour, as an index into [`PALETTE`]: the one chosen, or
/// one assigned in the order accounts were added. The stylesheet defines
/// `account-0` to `account-8`.
pub fn account_color_index(account_id: AccountId) -> usize {
    ACCOUNT_COLORS
        .with(|c| c.borrow().get(&account_id).copied())
        .filter(|i| *i < PALETTE.len())
        .unwrap_or((account_id.max(1) - 1) as usize % PALETTE.len())
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone};

    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> EpochMillis {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .unwrap()
            .timestamp_millis()
    }

    /// The same as `at`, under a name the event tests read better with.
    fn at_local(y: i32, m: u32, d: u32, h: u32, min: u32) -> EpochMillis {
        at(y, m, d, h, min)
    }

    #[test]
    fn dates_get_shorter_the_closer_they_are() {
        let now = Local.with_ymd_and_hms(2026, 9, 17, 15, 0, 0).unwrap();
        assert_eq!(relative_date(at(2026, 9, 17, 9, 5), now), "09:05");
        assert_eq!(relative_date(at(2026, 9, 16, 23, 0), now), "Yesterday");
        assert_eq!(relative_date(at(2026, 9, 14, 12, 0), now), "Monday");
        assert_eq!(relative_date(at(2026, 3, 3, 12, 0), now), "3 Mar");
        assert_eq!(relative_date(at(2024, 9, 3, 12, 0), now), "2024-09-03");
        assert_eq!(relative_date(at(2026, 9, 18, 8, 0), now), "08:00");
    }

    #[test]
    fn header_dates_stay_short() {
        let now = Local.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap();
        assert_eq!(header_date(at(2026, 9, 19, 10, 12), now), "Today at 10:12");
        assert_eq!(
            header_date(at(2026, 9, 18, 14, 50), now),
            "Yesterday at 14:50"
        );
        assert_eq!(
            header_date(at(2026, 9, 11, 8, 50), now),
            "Fri 11 Sep at 08:50"
        );
        assert_eq!(header_date(at(2024, 9, 3, 8, 50), now), "3 Sep 2024");
    }

    #[test]
    fn full_dates_spell_everything_out() {
        assert_eq!(
            full_date(at(2026, 9, 3, 14, 32)),
            "Thursday, 3 September 2026 at 14:32"
        );
    }

    #[test]
    fn portuguese_names_its_weekdays_and_months() {
        // Each test runs on its own thread, which keeps the locale here.
        mailrs_domain::translate::set_date_locale("pt_PT");
        let now = Local.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap();
        assert_eq!(relative_date(at(2026, 9, 14, 12, 0), now), "segunda");
        assert_eq!(relative_date(at(2026, 3, 3, 12, 0), now), "3 mar");
        assert_eq!(
            header_date(at(2026, 9, 11, 8, 50), now),
            "sex 11 set at 08:50"
        );
        assert_eq!(
            full_date(at(2026, 9, 3, 14, 32)),
            "quinta, 3 setembro 2026 at 14:32"
        );
        let (month, _) = event_tile(&When::At {
            starts_at: at_local(2026, 2, 9, 15, 0),
            ends_at: None,
        });
        assert_eq!(month, "FEV");
    }

    #[test]
    fn an_event_reads_as_a_day_and_a_time() {
        let now = Local.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap();
        let at = |start: EpochMillis, end: Option<EpochMillis>| {
            event_when(
                &When::At {
                    starts_at: start,
                    ends_at: end,
                },
                now,
            )
        };
        assert_eq!(
            at(
                at_local(2026, 9, 19, 16, 0),
                Some(at_local(2026, 9, 19, 17, 0))
            ),
            "Today · 16:00 to 17:00"
        );
        assert_eq!(
            at(
                at_local(2026, 9, 20, 9, 30),
                Some(at_local(2026, 9, 20, 10, 15))
            ),
            "Tomorrow · 09:30 to 10:15"
        );
        assert_eq!(
            at(
                at_local(2026, 9, 22, 14, 0),
                Some(at_local(2026, 9, 22, 14, 45))
            ),
            "Tuesday · 14:00 to 14:45"
        );
        assert_eq!(
            at(at_local(2026, 11, 3, 14, 0), None),
            "Tuesday, 3 November · 14:00"
        );
        assert_eq!(
            at(
                at_local(2027, 1, 4, 9, 0),
                Some(at_local(2027, 1, 4, 10, 0))
            ),
            "Monday, 4 January 2027 · 09:00 to 10:00"
        );
        // A meeting that runs past midnight names the day it ends on.
        assert_eq!(
            at(
                at_local(2026, 11, 3, 23, 0),
                Some(at_local(2026, 11, 4, 1, 0))
            ),
            "Tuesday, 3 November · 23:00 to 4 Nov 01:00"
        );
    }

    #[test]
    fn an_all_day_event_says_so() {
        let now = Local.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap();
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(
            event_when(
                &When::Days {
                    first: day(2026, 7, 14),
                    last: day(2026, 7, 14)
                },
                now
            ),
            "Tuesday, 14 July · All day"
        );
        assert_eq!(
            event_when(
                &When::Days {
                    first: day(2026, 7, 14),
                    last: day(2026, 7, 16)
                },
                now
            ),
            "14 to 16 July · All day"
        );
        assert_eq!(
            event_when(
                &When::Days {
                    first: day(2026, 6, 30),
                    last: day(2026, 7, 2)
                },
                now
            ),
            "30 June to 2 July · All day"
        );
    }

    #[test]
    fn a_moved_meeting_names_where_it_was() {
        let now = Local.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap();
        assert_eq!(
            event_moved_from(at_local(2026, 9, 22, 10, 0), false, now),
            "Tuesday 10:00"
        );
        assert_eq!(
            event_moved_from(at_local(2026, 9, 22, 0, 0), true, now),
            "Tuesday"
        );
    }

    #[test]
    fn the_date_tile_holds_a_month_and_a_day() {
        let (month, day) = event_tile(&When::At {
            starts_at: at_local(2026, 6, 9, 15, 0),
            ends_at: None,
        });
        assert_eq!((month.as_str(), day.as_str()), ("JUN", "9"));
    }

    #[test]
    fn sizes_use_readable_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(250 * 1024), "250 KB");
        assert_eq!(human_size(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
    }

    #[test]
    fn initials_come_from_names_or_addresses() {
        assert_eq!(initials("Ann Lee"), "AL");
        assert_eq!(initials("Mary Jane Watson"), "MW");
        assert_eq!(initials("bob@example.com"), "B");
        assert_eq!(initials("dana.reyes@example.com"), "DR");
        assert_eq!(initials("élodie"), "É");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn colours_are_stable() {
        assert_eq!(color_for("ann@example.com"), color_for("ANN@example.com"));
        assert_eq!(account_color_index(1), 0);
        assert_eq!(account_color_index(10), 0);
    }
}
