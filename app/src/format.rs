//! Text for the UI: dates, sizes, initials, and colours.

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
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

/// A date for message headers: "Today at 10:12", "Yesterday at 14:50",
/// "Fri 18 Sep at 08:50", or "3 Sep 2024" for earlier years.
pub fn header_date(ts: EpochMillis, now: DateTime<Local>) -> String {
    let Some(when) = local(ts) else {
        return String::new();
    };
    match (now.date_naive() - when.date_naive()).num_days() {
        ..=0 => when.format("Today at %H:%M").to_string(),
        1 => when.format("Yesterday at %H:%M").to_string(),
        _ if when.year() == now.year() => when.format("%a %-d %b at %H:%M").to_string(),
        _ => when.format("%-d %b %Y").to_string(),
    }
}

/// A long date for reply attributions and forwarded headers.
pub fn full_date(ts: EpochMillis) -> String {
    local(ts)
        .map(|when| when.format("%A, %-d %B %Y at %H:%M").to_string())
        .unwrap_or_default()
}

/// When a scheduled message goes out: "today at 21:00", "tomorrow at
/// 08:00", "Monday at 08:00" within the week, then "Tue 3 Nov at 08:00".
pub fn future_date(ts: EpochMillis, now: DateTime<Local>) -> String {
    let Some(when) = local(ts) else {
        return String::new();
    };
    match (when.date_naive() - now.date_naive()).num_days() {
        ..=0 => when.format("today at %H:%M").to_string(),
        1 => when.format("tomorrow at %H:%M").to_string(),
        2..=6 => when.format("%A at %H:%M").to_string(),
        _ if when.year() == now.year() => when.format("%a %-d %b at %H:%M").to_string(),
        _ => when.format("%-d %b %Y at %H:%M").to_string(),
    }
}

/// Apple Mail's Send Later presets: tonight at 21:00 while there is time,
/// tomorrow at 08:00, and next Monday at 08:00 when that is not tomorrow.
pub fn send_later_presets(now: DateTime<Local>) -> Vec<(String, EpochMillis)> {
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
        presets.push(("Send Tonight at 21:00".to_string(), ts));
    }
    let tomorrow = today + chrono::Days::new(1);
    if let Some(ts) = at(tomorrow, 8) {
        presets.push(("Send Tomorrow at 08:00".to_string(), ts));
    }
    let to_monday = (7 - today.weekday().num_days_from_monday()) % 7;
    let monday = today + chrono::Days::new(if to_monday == 0 { 7 } else { to_monday as u64 });
    if monday != tomorrow
        && let Some(ts) = at(monday, 8)
    {
        presets.push(("Send Monday at 08:00".to_string(), ts));
    }
    presets
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

/// Each account's colour, as an index into [`PALETTE`], assigned in the
/// order accounts were added. The stylesheet defines `account-0` to `account-8`.
pub fn account_color_index(account_id: AccountId) -> usize {
    (account_id.max(1) - 1) as usize % PALETTE.len()
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
