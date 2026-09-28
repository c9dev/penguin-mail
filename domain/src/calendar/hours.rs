//! Working hours: the stretch of the day, and the weekdays, a person
//! keeps meetings in. The Day and Week grid shades what falls outside
//! them, Month shades a day that is not one of them, and the assistant's
//! free-time tool bounds its search to them unless asked otherwise.
//! Defaults to 09:00 to 18:00, Monday to Friday.

use chrono::{NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

/// A day worked, and the minutes of it that are: `start_minutes` and
/// `end_minutes` count from midnight, and `days` says which weekdays
/// count at all, Monday first, so the default working week reads at a
/// glance as `[true, true, true, true, true, false, false]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkingHours {
    pub start_minutes: u16,
    pub end_minutes: u16,
    pub days: [bool; 7],
}

impl Default for WorkingHours {
    fn default() -> Self {
        WorkingHours {
            start_minutes: 9 * 60,
            end_minutes: 18 * 60,
            days: [true, true, true, true, true, false, false],
        }
    }
}

impl WorkingHours {
    /// Whether `day` is one of the days worked.
    pub fn is_working_day(&self, day: Weekday) -> bool {
        self.days[day.num_days_from_monday() as usize]
    }

    /// Whether the hour starting at `hour` (0 to 23, wall clock) on `day`
    /// falls outside the working stretch, and so should be shaded: every
    /// hour of a day not worked, and the hours before `start_minutes` or
    /// from `end_minutes` on a day that is.
    pub fn hour_shaded(&self, day: Weekday, hour: u32) -> bool {
        if !self.is_working_day(day) {
            return true;
        }
        let minutes = hour * 60;
        minutes < u32::from(self.start_minutes) || minutes >= u32::from(self.end_minutes)
    }

    /// The shaded hours of `day`, merged into as few `(start, end)`
    /// ranges as cover them, `end` exclusive: the Day and Week grid draws
    /// one rectangle per range rather than one per hour, so two shaded
    /// hours in a row share one edge instead of drawing it twice.
    pub fn shaded_hour_ranges(&self, day: Weekday) -> Vec<(u32, u32)> {
        if !self.is_working_day(day) {
            return vec![(0, 24)];
        }
        let mut ranges = Vec::new();
        let mut run_start = None;
        for hour in 0..24 {
            if self.hour_shaded(day, hour) {
                run_start.get_or_insert(hour);
            } else if let Some(start) = run_start.take() {
                ranges.push((start, hour));
            }
        }
        if let Some(start) = run_start {
            ranges.push((start, 24));
        }
        ranges
    }

    /// The working day's start as a time of day, for a field that shows
    /// or edits it.
    pub fn start_time(&self) -> NaiveTime {
        minutes_to_time(self.start_minutes)
    }

    /// The working day's end as a time of day.
    pub fn end_time(&self) -> NaiveTime {
        minutes_to_time(self.end_minutes)
    }
}

fn minutes_to_time(minutes: u16) -> NaiveTime {
    NaiveTime::from_hms_opt(u32::from(minutes) / 60, u32::from(minutes) % 60, 0).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_working_week_is_nine_to_six_on_weekdays() {
        let hours = WorkingHours::default();
        assert_eq!(hours.start_minutes, 540);
        assert_eq!(hours.end_minutes, 1080);
        assert!(hours.is_working_day(Weekday::Mon));
        assert!(hours.is_working_day(Weekday::Fri));
        assert!(!hours.is_working_day(Weekday::Sat));
        assert!(!hours.is_working_day(Weekday::Sun));
    }

    #[test]
    fn an_hour_before_the_working_day_starts_is_shaded() {
        assert!(WorkingHours::default().hour_shaded(Weekday::Mon, 8));
    }

    #[test]
    fn the_hour_the_working_day_starts_is_not_shaded() {
        assert!(!WorkingHours::default().hour_shaded(Weekday::Mon, 9));
    }

    #[test]
    fn the_last_working_hour_is_not_shaded() {
        assert!(!WorkingHours::default().hour_shaded(Weekday::Mon, 17));
    }

    #[test]
    fn the_hour_the_working_day_ends_is_shaded() {
        assert!(WorkingHours::default().hour_shaded(Weekday::Mon, 18));
    }

    #[test]
    fn every_hour_of_a_weekend_day_is_shaded() {
        let hours = WorkingHours::default();
        for hour in 0..24 {
            assert!(hours.hour_shaded(Weekday::Sat, hour), "hour {hour} on Saturday");
        }
    }

    #[test]
    fn a_weekday_shades_the_ranges_before_and_after_the_working_hours() {
        assert_eq!(
            WorkingHours::default().shaded_hour_ranges(Weekday::Mon),
            vec![(0, 9), (18, 24)]
        );
    }

    #[test]
    fn a_weekend_day_shades_the_whole_day_as_one_range() {
        assert_eq!(WorkingHours::default().shaded_hour_ranges(Weekday::Sat), vec![(0, 24)]);
    }

    #[test]
    fn a_working_day_that_runs_the_whole_day_shades_nothing() {
        let hours = WorkingHours {
            start_minutes: 0,
            end_minutes: 24 * 60,
            days: [true; 7],
        };
        assert_eq!(hours.shaded_hour_ranges(Weekday::Mon), Vec::<(u32, u32)>::new());
    }

    #[test]
    fn start_and_end_time_read_back_as_naive_times() {
        let hours = WorkingHours::default();
        assert_eq!(hours.start_time(), NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(hours.end_time(), NaiveTime::from_hms_opt(18, 0, 0).unwrap());
    }

    #[test]
    fn a_custom_working_week_can_run_tuesday_to_saturday() {
        let hours = WorkingHours {
            start_minutes: 600,
            end_minutes: 900,
            days: [false, true, true, true, true, true, false],
        };
        assert!(!hours.is_working_day(Weekday::Mon));
        assert!(hours.is_working_day(Weekday::Sat));
        assert!(hours.hour_shaded(Weekday::Mon, 11));
        assert!(!hours.hour_shaded(Weekday::Sat, 11));
    }
}
