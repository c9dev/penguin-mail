//! Which day a week starts on, and the column order that follows from
//! it. The desktop's locale decides it by default (glibc's
//! `first_weekday`, read in `mailrs::locale_time`), unless Preferences'
//! Week Starts On row names a fixed day instead ([`WeekStart`]); this
//! module holds the arithmetic that reads a platform's raw answer, folds
//! in the person's own choice, and turns a start weekday into a grid's
//! columns, none of which touches the platform itself.

use chrono::{Datelike, Days, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};

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

/// Which day the Week grid, the Month grid and the mini month all start
/// on: the locale's own first weekday, or a fixed weekday the person
/// picked in Preferences instead. `mailrs::settings::Settings::week_start`
/// keeps one of these; [`week_start`] is the one function that turns it,
/// together with the locale's own answer, into the weekday a grid
/// actually starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WeekStart {
    /// The locale's own first weekday, as `nl_langinfo` names it
    /// (`mailrs::locale_time::first_weekday`, read on the platform).
    #[default]
    Automatic,
    Monday,
    Sunday,
}

/// The weekday a calendar grid starts its week on: `locale` for
/// [`WeekStart::Automatic`], or the fixed day the person chose instead.
/// Week, Month and the mini month draw their columns through this one
/// function, so they read a Preferences change and the locale's own
/// answer the same way.
pub fn week_start(setting: WeekStart, locale: Weekday) -> Weekday {
    match setting {
        WeekStart::Automatic => locale,
        WeekStart::Monday => Weekday::Mon,
        WeekStart::Sunday => Weekday::Sun,
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

/// The week number of the week holding `date`, for weeks that start on
/// `start`: the ISO number that most of the week's seven days carry.
/// That is the ISO week of the week's Thursday, since ISO weeks run
/// Monday to Sunday and Thursday is their fourth day. A week of Monday
/// to Sunday gets its plain ISO number. A week from Sunday to Saturday
/// takes the number of the six days after its Sunday, so the Week and
/// Day views give every day in it the same number.
pub fn week_number(date: NaiveDate, start: Weekday) -> u32 {
    let first = week_start_on_or_before(date, start);
    let thursday = first + Days::new(u64::from(Weekday::Thu.days_since(start)));
    thursday.iso_week().week()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn every_day_of_a_sunday_week_gets_the_number_most_of_its_days_carry() {
        // Sunday 27 September to Saturday 3 October 2026: the Sunday is
        // in ISO week 39, the six days after it in week 40.
        for offset in 0..7 {
            let date = day(2026, 9, 27) + Days::new(offset);
            assert_eq!(week_number(date, Weekday::Sun), 40, "{date}");
        }
    }

    #[test]
    fn a_monday_week_takes_its_iso_number() {
        assert_eq!(week_number(day(2026, 9, 27), Weekday::Mon), 39);
        assert_eq!(week_number(day(2026, 9, 28), Weekday::Mon), 40);
    }

    #[test]
    fn a_saturday_week_counts_with_the_monday_inside_it() {
        // Saturday 26 September to Friday 2 October 2026.
        assert_eq!(week_number(day(2026, 9, 26), Weekday::Sat), 40);
        assert_eq!(week_number(day(2026, 10, 2), Weekday::Sat), 40);
    }

    #[test]
    fn a_sunday_week_across_new_year_takes_the_new_years_first_week() {
        // Sunday 28 December 2025 to Saturday 3 January 2026: ISO week 1
        // of 2026 starts on Monday 29 December.
        assert_eq!(week_number(day(2025, 12, 28), Weekday::Sun), 1);
        assert_eq!(week_number(day(2026, 1, 3), Weekday::Sun), 1);
    }

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

    #[test]
    fn automatic_follows_the_locales_own_first_weekday() {
        assert_eq!(week_start(WeekStart::Automatic, Weekday::Sun), Weekday::Sun);
        assert_eq!(week_start(WeekStart::Automatic, Weekday::Mon), Weekday::Mon);
    }

    #[test]
    fn monday_overrides_a_locale_that_starts_on_sunday() {
        assert_eq!(week_start(WeekStart::Monday, Weekday::Sun), Weekday::Mon);
    }

    #[test]
    fn sunday_overrides_a_locale_that_starts_on_monday() {
        assert_eq!(week_start(WeekStart::Sunday, Weekday::Mon), Weekday::Sun);
    }

    #[test]
    fn week_start_defaults_to_automatic() {
        assert_eq!(WeekStart::default(), WeekStart::Automatic);
    }
}
