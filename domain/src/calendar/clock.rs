//! Printing a time of day in 12-hour or 24-hour clock. `mailrs::clock_format`
//! decides which one GNOME's `clock-format` or the locale asks for; this
//! module only turns that choice into text, so the choosing needs no
//! display to test.

use chrono::{Locale, NaiveDate, NaiveTime};

use crate::translate::gettext;

/// Which clock a time prints in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockFormat {
    Hour12,
    Hour24,
}

/// `at`'s time of day in `format`, with `locale` naming the day period's
/// own words ("AM"/"PM" in English, "a.m."/"p.m." in Portuguese). Paired
/// with a fixed date, since neither `NaiveTime` nor `NaiveDateTime` has
/// `format_localized` and a time pattern names no day.
pub fn format_time(at: NaiveTime, format: ClockFormat, locale: Locale) -> String {
    let pattern = match format {
        ClockFormat::Hour24 => gettext("%H:%M"),
        // A leading zero on the hour would read "03:05 PM" where every
        // 12-hour clock on the desktop reads "3:05 PM".
        ClockFormat::Hour12 => gettext("%-I:%M %p"),
    };
    fixed_date()
        .and_time(at)
        .and_utc()
        .format_localized(&pattern, locale)
        .to_string()
}

/// Reads `text` back as a time of day, trying the 24-hour pattern and
/// then the 12-hour one: a dropdown built by [`format_time`] round-trips
/// through whichever clock was current when it was built, even after the
/// clock changes under it.
pub fn parse_time(text: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(text, "%I:%M %p"))
        .ok()
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
        assert_eq!(parse_time("15:05"), NaiveTime::from_hms_opt(15, 5, 0));
    }

    #[test]
    fn a_12_hour_time_reads_back_the_same_time() {
        assert_eq!(parse_time("3:05 PM"), NaiveTime::from_hms_opt(15, 5, 0));
    }

    #[test]
    fn nonsense_text_reads_back_nothing() {
        assert_eq!(parse_time("not a time"), None);
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
