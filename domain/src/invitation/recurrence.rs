//! `RRULE` and `DURATION` in words.
//!
//! An invitation that repeats carries its rule as `FREQ=WEEKLY;BYDAY=MO`.
//! Nobody reads that, so the card shows "Every Monday until 30 June"
//! instead. Rules this does not recognize give back `None` and the card
//! shows no repeat line, which beats showing the rule.
//!
//! Each piece of the line is a whole phrase with named values in it, so a
//! translator moves the day and the count where the language wants them.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone};

use crate::translate::{fill, fill_plural, gettext};

/// The rule in words, or `None` when it names no frequency this
/// understands. `start_year` is the year the event starts in: an end date
/// in that same year needs no year of its own.
pub(crate) fn in_words(rule: &str, start_year: Option<i32>) -> Option<String> {
    let parts = parts(rule);
    let every = interval(&parts);
    let count = [("count", every.to_string())];
    let count = [("count", count[0].1.as_str())];
    let mut words = match part(&parts, "FREQ")?.to_ascii_uppercase().as_str() {
        "DAILY" => fill_plural("Every day", "Every {count} days", every as usize, &count),
        "WEEKLY" => {
            let weekly = fill_plural("Every week", "Every {count} weeks", every as usize, &count);
            match weekdays(&parts) {
                Some(days) if every == 1 => fill(&gettext("Every {days}"), &[("days", &days)]),
                Some(days) => fill(
                    &gettext("{every} on {days}"),
                    &[("every", &weekly), ("days", &days)],
                ),
                None => weekly,
            }
        }
        "MONTHLY" => {
            let monthly = fill_plural(
                "Every month",
                "Every {count} months",
                every as usize,
                &count,
            );
            match monthly_day(&parts) {
                Some(day) => fill(
                    &gettext("{every} on {day}"),
                    &[("every", &monthly), ("day", &day)],
                ),
                None => monthly,
            }
        }
        "YEARLY" => fill_plural("Every year", "Every {count} years", every as usize, &count),
        "HOURLY" => fill_plural("Every hour", "Every {count} hours", every as usize, &count),
        "MINUTELY" => fill_plural(
            "Every minute",
            "Every {count} minutes",
            every as usize,
            &count,
        ),
        _ => return None,
    };
    if let Some(last) = part(&parts, "UNTIL").and_then(day_of) {
        words = fill(
            &gettext("{every} until {day}"),
            &[("every", &words), ("day", &spell_day(last, start_year))],
        );
    } else if let Some(times) = part(&parts, "COUNT").and_then(|c| c.parse::<u32>().ok()) {
        let repeats = match times {
            1 => gettext("once"),
            times => fill_plural(
                "{count} time",
                "{count} times",
                times as usize,
                &[("count", &times.to_string())],
            ),
        };
        words = fill(
            &gettext("{every}, {repeats}"),
            &[("every", &words), ("repeats", &repeats)],
        );
    }
    Some(words)
}

/// A `DURATION` such as `PT1H30M` or `P2D`. Weeks and days count as fixed
/// lengths, which is what every invitation means by them.
pub(crate) fn duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (sign, rest) = match text.as_bytes().first()? {
        b'-' => (-1, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let mut seconds: i64 = 0;
    let mut number = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            'T' | 't' => in_time = true,
            '0'..='9' => number.push(c),
            _ => {
                let value: i64 = number.parse().ok()?;
                number.clear();
                seconds += value
                    * match (c.to_ascii_uppercase(), in_time) {
                        ('W', _) => 7 * 24 * 3600,
                        ('D', _) => 24 * 3600,
                        ('H', true) => 3600,
                        ('M', true) => 60,
                        ('S', true) => 1,
                        _ => return None,
                    };
            }
        }
    }
    number.is_empty().then(|| Duration::seconds(sign * seconds))
}

/// `KEY=VALUE` pairs, semicolon separated, keys uppercased.
fn parts(rule: &str) -> Vec<(String, String)> {
    rule.split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_uppercase(), value.trim().to_string()))
        .collect()
}

fn part<'a>(parts: &'a [(String, String)], key: &str) -> Option<&'a str> {
    parts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.as_str())
}

fn interval(parts: &[(String, String)]) -> u32 {
    part(parts, "INTERVAL")
        .and_then(|value| value.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1)
}

/// The weekdays a `BYDAY` lists, as "Monday" or "Monday, Wednesday and
/// Friday". A `BYDAY` that counts weeks, such as `2TU`, is not a weekly
/// pattern and gives `None`.
fn weekdays(parts: &[(String, String)]) -> Option<String> {
    let listed = part(parts, "BYDAY")?;
    let names: Option<Vec<String>> = listed
        .split(',')
        .map(|day| weekday_name(day.trim()))
        .collect();
    let names = names?;
    match names.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        [rest @ .., last] => Some(fill(
            &gettext("{days} and {last}"),
            &[("days", &rest.join(", ")), ("last", last)],
        )),
    }
}

fn weekday_name(code: &str) -> Option<String> {
    match code.to_ascii_uppercase().as_str() {
        "MO" => Some(gettext("Monday")),
        "TU" => Some(gettext("Tuesday")),
        "WE" => Some(gettext("Wednesday")),
        "TH" => Some(gettext("Thursday")),
        "FR" => Some(gettext("Friday")),
        "SA" => Some(gettext("Saturday")),
        "SU" => Some(gettext("Sunday")),
        _ => None,
    }
}

/// Which day of the month a monthly rule picks: "the 15th", "the second
/// Tuesday", "the last Friday".
fn monthly_day(parts: &[(String, String)]) -> Option<String> {
    if let Some(day) = part(parts, "BYMONTHDAY").and_then(|d| d.trim().parse::<i32>().ok()) {
        return Some(match day {
            -1 => gettext("the last day"),
            day if day > 0 => ordinal(day as u32),
            _ => return None,
        });
    }
    let listed = part(parts, "BYDAY")?;
    let (count, code) = listed.trim().split_at(listed.trim().len().checked_sub(2)?);
    let name = weekday_name(code)?;
    Some(match count.parse::<i32>().ok()? {
        -1 => fill(&gettext("the last {weekday}"), &[("weekday", &name)]),
        n if n > 0 => fill(
            &gettext("the {nth} {weekday}"),
            &[("nth", &nth(n as u32)), ("weekday", &name)],
        ),
        _ => return None,
    })
}

fn nth(n: u32) -> String {
    match n {
        1 => gettext("first"),
        2 => gettext("second"),
        3 => gettext("third"),
        4 => gettext("fourth"),
        _ => gettext("fifth"),
    }
}

/// Which day of the month a rule picks, as the card words it: "the 15th"
/// in English. The number is named rather than glued on, because the
/// English suffix is English grammar and another language wants none of
/// it; each suffix carries its own sentence for a translator to replace
/// with one.
fn ordinal(n: u32) -> String {
    let day = [("day", n.to_string())];
    let day = [("day", day[0].1.as_str())];
    match (n % 10, n % 100) {
        (_, 11..=13) => fill(&gettext("the {day}th"), &day),
        (1, _) => fill(&gettext("the {day}st"), &day),
        (2, _) => fill(&gettext("the {day}nd"), &day),
        (3, _) => fill(&gettext("the {day}rd"), &day),
        _ => fill(&gettext("the {day}th"), &day),
    }
}

/// The local day an `UNTIL` value falls on. Organizers write it in UTC,
/// so a rule that ends late in the evening can land on the next day here.
fn day_of(until: &str) -> Option<NaiveDate> {
    let until = until.trim();
    if let Ok(date) = NaiveDate::parse_from_str(until, "%Y%m%d") {
        return Some(date);
    }
    if let Some(naive) = until
        .strip_suffix(['Z', 'z'])
        .and_then(|rest| NaiveDateTime::parse_from_str(rest, "%Y%m%dT%H%M%S").ok())
    {
        return Some(
            chrono::Utc
                .from_utc_datetime(&naive)
                .with_timezone(&chrono::Local)
                .date_naive(),
        );
    }
    NaiveDateTime::parse_from_str(until, "%Y%m%dT%H%M%S")
        .ok()
        .map(|naive| naive.date())
}

/// "30 June", with the year when the event does not start in it. The
/// month's name comes out of chrono in English whatever the locale says,
/// which is a gap worth closing the day this reaches for a locale-aware
/// formatter.
fn spell_day(day: NaiveDate, start_year: Option<i32>) -> String {
    if start_year == Some(day.year()) {
        day.format(&gettext("%-d %B")).to_string()
    } else {
        day.format(&gettext("%-d %B %Y")).to_string()
    }
}
