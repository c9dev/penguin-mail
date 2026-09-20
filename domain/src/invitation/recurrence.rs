//! `RRULE` and `DURATION` in words.
//!
//! An invitation that repeats carries its rule as `FREQ=WEEKLY;BYDAY=MO`.
//! Nobody reads that, so the card shows "Every Monday until 30 June"
//! instead. Rules this does not recognize give back `None` and the card
//! shows no repeat line, which beats showing the rule.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone};

/// The rule in English, or `None` when it names no frequency this
/// understands. `start_year` is the year the event starts in: an end date
/// in that same year needs no year of its own.
pub(crate) fn in_words(rule: &str, start_year: Option<i32>) -> Option<String> {
    let parts = parts(rule);
    let every = interval(&parts);
    let mut words = match part(&parts, "FREQ")?.to_ascii_uppercase().as_str() {
        "DAILY" => plural(every, "day"),
        "WEEKLY" => match weekdays(&parts) {
            Some(days) if every == 1 => format!("Every {days}"),
            Some(days) => format!("{} on {days}", plural(every, "week")),
            None => plural(every, "week"),
        },
        "MONTHLY" => match monthly_day(&parts) {
            Some(day) => format!("{} on {day}", plural(every, "month")),
            None => plural(every, "month"),
        },
        "YEARLY" => plural(every, "year"),
        "HOURLY" => plural(every, "hour"),
        "MINUTELY" => plural(every, "minute"),
        _ => return None,
    };
    if let Some(last) = part(&parts, "UNTIL").and_then(day_of) {
        words.push_str(&format!(" until {}", spell_day(last, start_year)));
    } else if let Some(count) = part(&parts, "COUNT").and_then(|c| c.parse::<u32>().ok()) {
        words.push_str(&match count {
            1 => ", once".to_string(),
            count => format!(", {count} times"),
        });
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

/// "Every day" for one, "Every 3 days" for more.
fn plural(every: u32, unit: &str) -> String {
    match every {
        1 => format!("Every {unit}"),
        n => format!("Every {n} {unit}s"),
    }
}

/// The weekdays a `BYDAY` lists, as "Monday" or "Monday, Wednesday and
/// Friday". A `BYDAY` that counts weeks, such as `2TU`, is not a weekly
/// pattern and gives `None`.
fn weekdays(parts: &[(String, String)]) -> Option<String> {
    let listed = part(parts, "BYDAY")?;
    let names: Option<Vec<&str>> = listed
        .split(',')
        .map(|day| weekday_name(day.trim()))
        .collect();
    let names = names?;
    match names.as_slice() {
        [] => None,
        [one] => Some((*one).to_string()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
}

fn weekday_name(code: &str) -> Option<&'static str> {
    match code.to_ascii_uppercase().as_str() {
        "MO" => Some("Monday"),
        "TU" => Some("Tuesday"),
        "WE" => Some("Wednesday"),
        "TH" => Some("Thursday"),
        "FR" => Some("Friday"),
        "SA" => Some("Saturday"),
        "SU" => Some("Sunday"),
        _ => None,
    }
}

/// Which day of the month a monthly rule picks: "the 15th", "the second
/// Tuesday", "the last Friday".
fn monthly_day(parts: &[(String, String)]) -> Option<String> {
    if let Some(day) = part(parts, "BYMONTHDAY").and_then(|d| d.trim().parse::<i32>().ok()) {
        return Some(match day {
            -1 => "the last day".to_string(),
            day if day > 0 => format!("the {}", ordinal(day as u32)),
            _ => return None,
        });
    }
    let listed = part(parts, "BYDAY")?;
    let (count, code) = listed.trim().split_at(listed.trim().len().checked_sub(2)?);
    let name = weekday_name(code)?;
    Some(match count.parse::<i32>().ok()? {
        -1 => format!("the last {name}"),
        n if n > 0 => format!("the {} {name}", nth(n as u32)),
        _ => return None,
    })
}

fn nth(n: u32) -> &'static str {
    match n {
        1 => "first",
        2 => "second",
        3 => "third",
        4 => "fourth",
        _ => "fifth",
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
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

/// "30 June", with the year when the event does not start in it.
fn spell_day(day: NaiveDate, start_year: Option<i32>) -> String {
    if start_year == Some(day.year()) {
        day.format("%-d %B").to_string()
    } else {
        day.format("%-d %B %Y").to_string()
    }
}
