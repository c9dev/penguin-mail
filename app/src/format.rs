//! Text for the UI: dates, sizes, initials, and colours.

use chrono::{DateTime, Datelike, Local, TimeZone};
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
    match days {
        ..=0 => when.format("%H:%M").to_string(),
        1 => "Yesterday".into(),
        2..=6 => when.format("%A").to_string(),
        _ if when.year() == now.year() => when.format("%-d %b").to_string(),
        _ => when.format("%Y-%m-%d").to_string(),
    }
}

/// A long date for conversation headers.
pub fn full_date(ts: EpochMillis) -> String {
    local(ts)
        .map(|when| when.format("%A, %-d %B %Y at %H:%M").to_string())
        .unwrap_or_default()
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

/// Each account's colour, assigned in the order accounts were added.
pub fn account_color(account_id: AccountId) -> &'static str {
    PALETTE[(account_id.max(1) - 1) as usize % PALETTE.len()]
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
    fn full_dates_spell_everything_out() {
        assert_eq!(
            full_date(at(2026, 9, 3, 14, 32)),
            "Thursday, 3 September 2026 at 14:32"
        );
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
        assert_eq!(account_color(1), PALETTE[0]);
        assert_eq!(account_color(10), PALETTE[0]);
    }
}
