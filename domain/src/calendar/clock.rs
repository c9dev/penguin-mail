//! Printing a time of day in 12-hour or 24-hour clock. `mailrs::clock_format`
//! decides which one GNOME's `clock-format` or the locale asks for; this
//! module only turns that choice into text, so the choosing needs no
//! display to test.

use chrono::{Locale, NaiveDate, NaiveTime, Timelike};

use crate::translate::gettext;

/// Which clock a time prints in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockFormat {
    Hour12,
    Hour24,
}

/// `at`'s time of day in `format`, with `locale` naming the day period's
/// own words ("AM"/"PM" in English). Paired with a fixed date, since
/// neither `NaiveTime` nor `NaiveDateTime` has `format_localized` and a
/// time pattern names no day.
pub fn format_time(at: NaiveTime, format: ClockFormat, locale: Locale) -> String {
    let pattern = match format {
        ClockFormat::Hour24 => gettext("%H:%M"),
        ClockFormat::Hour12 => {
            let (am, pm) = periods(locale);
            let word = if at.hour() < 12 { am } else { pm };
            // A leading zero on the hour would read "03:05 PM" where every
            // 12-hour clock on the desktop reads "3:05 PM".
            gettext("%-I:%M %p").replace("%p", &word.replace('%', "%%"))
        }
    };
    fixed_date()
        .and_time(at)
        .and_utc()
        .format_localized(&pattern, locale)
        .to_string()
}

/// Reads `text` back as a time of day, trying the 24-hour pattern and
/// then the 12-hour one with `locale`'s day period words or English
/// ones: a dropdown built by [`format_time`] round-trips through
/// whichever clock was current when it was built, even after the clock
/// changes under it.
pub fn parse_time(text: &str, locale: Locale) -> Option<NaiveTime> {
    let text = text.trim();
    if let Ok(at) = NaiveTime::parse_from_str(text, "%H:%M") {
        return Some(at);
    }
    let (am, pm) = periods(locale);
    let words = [(pm, 12), (am, 0), ("PM".to_string(), 12), ("AM".to_string(), 0)];
    words.iter().find_map(|(word, add)| {
        let clock = strip_word(text, word)?;
        let (hour, minute) = clock.split_once(':')?;
        let (hour, minute): (u32, u32) = (hour.trim().parse().ok()?, minute.trim().parse().ok()?);
        if !(1..=12).contains(&hour) {
            return None;
        }
        NaiveTime::from_hms_opt(hour % 12 + add, minute, 0)
    })
}

/// `text` without `word` at its end or its start, ignoring case.
fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let lower = text.to_lowercase();
    let word = word.to_lowercase();
    // Lowercasing keeps byte lengths for the scripts chrono's locales
    // spell their day periods in; when it does not, nothing matches.
    if lower.len() != text.len() || word.is_empty() {
        return None;
    }
    if lower.ends_with(&word) {
        return Some(&text[..text.len() - word.len()]);
    }
    lower.starts_with(&word).then(|| &text[word.len()..])
}

/// `locale`'s words for morning and afternoon. chrono gives empty words
/// for some locales, Portuguese and German among them, and a 12-hour
/// clock without them reads 03:00 and 15:00 the same, so those get the
/// English "AM" and "PM".
fn periods(locale: Locale) -> (String, String) {
    let word = |hour| {
        fixed_date()
            .and_hms_opt(hour, 0, 0)
            .map(|at| at.and_utc().format_localized("%p", locale).to_string().trim().to_string())
            .unwrap_or_default()
    };
    let (am, pm) = (word(1), word(13));
    if am.is_empty() || pm.is_empty() || am == pm {
        ("AM".to_string(), "PM".to_string())
    } else {
        (am, pm)
    }
}

/// Whether glibc's `T_FMT` (the locale's own `strftime` pattern for a
/// bare time, read by `mailrs::locale_time`) names a 12-hour clock: it
/// spells the hour with `%I` or `%l`, or defers to the locale's own
/// 12-hour pattern with `%r`.
pub fn is_12_hour_pattern(t_fmt: &str) -> bool {
    ["%I", "%l", "%r", "%p", "%P"].iter().any(|marker| t_fmt.contains(marker))
}

fn fixed_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2000, 1, 1).expect("2000-01-01 is a real date")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_in_the_24_hour_clock_has_no_am_or_pm() {
        let at = NaiveTime::from_hms_opt(15, 5, 0).unwrap();
        assert_eq!(format_time(at, ClockFormat::Hour24, Locale::en_US), "15:05");
    }

    #[test]
    fn a_time_in_the_12_hour_clock_drops_the_leading_zero_and_names_the_period() {
        let at = NaiveTime::from_hms_opt(15, 5, 0).unwrap();
        assert_eq!(format_time(at, ClockFormat::Hour12, Locale::en_US), "3:05 PM");
    }

    #[test]
    fn midnight_in_the_12_hour_clock_reads_twelve_am() {
        let at = NaiveTime::from_hms_opt(0, 5, 0).unwrap();
        assert_eq!(format_time(at, ClockFormat::Hour12, Locale::en_US), "12:05 AM");
    }

    #[test]
    fn a_24_hour_time_reads_back_the_same_time() {
        assert_eq!(parse_time("15:05", Locale::en_US), NaiveTime::from_hms_opt(15, 5, 0));
    }

    #[test]
    fn a_12_hour_time_reads_back_the_same_time() {
        assert_eq!(parse_time("3:05 PM", Locale::en_US), NaiveTime::from_hms_opt(15, 5, 0));
    }

    #[test]
    fn nonsense_text_reads_back_nothing() {
        assert_eq!(parse_time("not a time", Locale::en_US), None);
        assert_eq!(parse_time("13:05 PM", Locale::en_US), None);
    }

    #[test]
    fn a_locale_without_day_period_words_tells_morning_from_afternoon() {
        let morning = format_time(NaiveTime::from_hms_opt(3, 0, 0).unwrap(), ClockFormat::Hour12, Locale::pt_PT);
        let afternoon = format_time(NaiveTime::from_hms_opt(15, 0, 0).unwrap(), ClockFormat::Hour12, Locale::pt_PT);
        assert_ne!(morning, afternoon);
        assert_eq!(afternoon, "3:00 PM");
    }

    #[test]
    fn every_12_hour_time_reads_back_in_its_own_locale() {
        for locale in [Locale::en_US, Locale::pt_PT, Locale::de_DE, Locale::el_GR, Locale::ko_KR] {
            for hour in 0..24 {
                let at = NaiveTime::from_hms_opt(hour, 30, 0).unwrap();
                let text = format_time(at, ClockFormat::Hour12, locale);
                assert_eq!(parse_time(&text, locale), Some(at), "{text} in {locale:?}");
            }
        }
    }

    #[test]
    fn a_t_fmt_with_percent_capital_i_names_a_12_hour_clock() {
        assert!(is_12_hour_pattern("%I:%M %p"));
    }

    #[test]
    fn a_t_fmt_with_percent_r_names_a_12_hour_clock() {
        assert!(is_12_hour_pattern("%r"));
    }

    #[test]
    fn a_t_fmt_with_percent_capital_h_names_a_24_hour_clock() {
        assert!(!is_12_hour_pattern("%H:%M:%S"));
        assert!(!is_12_hour_pattern("%T"));
    }
}
