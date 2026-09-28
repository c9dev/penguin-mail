//! Which day a week starts on, and the column order that follows from
//! it. The desktop's locale decides it (glibc's `first_weekday`, read in
//! `mailrs::locale_time`); this module holds the arithmetic that reads a
//! platform's raw answer and turns a start weekday into a grid's columns,
//! neither of which touches the platform itself.

use chrono::{Datelike, Days, NaiveDate, Weekday};

/// glibc's `nl_langinfo(_NL_TIME_FIRST_WEEKDAY)` answers with the
/// `ABDAY_*` index of the first day of the week: 1 for Sunday up to 7 for
/// Saturday, the same order `%a` abbreviates days in. A byte outside that
/// range, from a platform with no such answer, keeps the app's own
/// default of Monday.
pub fn weekday_from_first_weekday_byte(byte: u8) -> Weekday {
    match byte {
        1 => Weekday::Sun,
        2 => Weekday::Mon,
        3 => Weekday::Tue,
        4 => Weekday::Wed,
        5 => Weekday::Thu,
        6 => Weekday::Fri,
        7 => Weekday::Sat,
        _ => Weekday::Mon,
    }
}

/// The seven weekdays in column order for a grid whose first column is
/// `start`.
pub fn week_columns(start: Weekday) -> [Weekday; 7] {
    let mut days = [start; 7];
    let mut day = start;
    for slot in &mut days {
        *slot = day;
        day = day.succ();
    }
    days
}

/// The first day, on or before `date`, of the week that starts on
/// `start`.
pub fn week_start_on_or_before(date: NaiveDate, start: Weekday) -> NaiveDate {
    let offset = date.weekday().days_since(start);
    date - Days::new(u64::from(offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glibcs_first_weekday_byte_one_is_sunday() {
        assert_eq!(weekday_from_first_weekday_byte(1), Weekday::Sun);
    }

    #[test]
    fn glibcs_first_weekday_byte_two_is_monday() {
        assert_eq!(weekday_from_first_weekday_byte(2), Weekday::Mon);
    }

    #[test]
    fn an_unknown_first_weekday_byte_falls_back_to_monday() {
        assert_eq!(weekday_from_first_weekday_byte(0), Weekday::Mon);
        assert_eq!(weekday_from_first_weekday_byte(9), Weekday::Mon);
    }

    #[test]
    fn week_columns_starting_on_sunday_run_sunday_to_saturday() {
        assert_eq!(
            week_columns(Weekday::Sun),
            [
                Weekday::Sun,
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
            ]
        );
    }

    #[test]
    fn week_columns_starting_on_monday_run_monday_to_sunday() {
        assert_eq!(
            week_columns(Weekday::Mon),
            [
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ]
        );
    }

    #[test]
    fn a_wednesday_weeks_start_is_the_sunday_before_it_when_weeks_start_on_sunday() {
        let wednesday = NaiveDate::from_ymd_opt(2026, 9, 23).expect("2026-09-23 is a Wednesday");
        let sunday = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        assert_eq!(week_start_on_or_before(wednesday, Weekday::Sun), sunday);
    }

    #[test]
    fn a_wednesday_weeks_start_is_the_monday_before_it_when_weeks_start_on_monday() {
        let wednesday = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        assert_eq!(week_start_on_or_before(wednesday, Weekday::Mon), monday);
    }

    #[test]
    fn the_first_day_of_its_own_week_is_itself() {
        let sunday = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        assert_eq!(week_start_on_or_before(sunday, Weekday::Sun), sunday);
    }
}
