//! A view's date range: which days it covers, and how it steps and
//! titles itself. No widget knows a date; each is handed the range the
//! window worked out.

use chrono::{Datelike, Days, Months, NaiveDate, TimeZone};
use mailrs_domain::EpochMillis;
use mailrs_domain::translate::{date_locale, fill, gettext};
use mailrs_sync::calendar_copy::FIRST_READ_BACK;

/// A day in milliseconds, for turning [`FIRST_READ_BACK`] into a day
/// count.
const DAY_MS: EpochMillis = 24 * 60 * 60 * 1000;

/// How many days the narrow agenda's first window covers.
const AGENDA_WINDOW: u64 = 60;

/// How many earlier days one scroll-to-top load adds.
const AGENDA_STEP: u64 = 30;

/// Which grid the calendar page shows. Kept in [`crate::settings`], not
/// here, so settings never has to import from `ui`.
pub use crate::settings::CalendarView as ViewKind;

/// The days one view of the calendar shows: a single day, a
/// Monday-to-Sunday week, or a six-week month grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub kind: ViewKind,
    pub first: NaiveDate,
    pub days: u32,
}

impl Range {
    /// The range of `kind` that holds `day`. A week runs Monday to
    /// Sunday; a month is the six weeks (42 days) starting on the
    /// Monday on or before the 1st, so every row is a full week.
    pub fn around(kind: ViewKind, day: NaiveDate) -> Range {
        match kind {
            ViewKind::Day => Range {
                kind,
                first: day,
                days: 1,
            },
            ViewKind::Week => Range {
                kind,
                first: monday_of(day),
                days: 7,
            },
            ViewKind::Month => {
                let first_of_month = day.with_day(1).unwrap_or(day);
                Range {
                    kind,
                    first: monday_of(first_of_month),
                    days: 42,
                }
            }
        }
    }

    /// The range that follows this one: the next day, week, or calendar
    /// month.
    pub fn next(self) -> Range {
        match self.kind {
            ViewKind::Day => Range::around(self.kind, self.first + Days::new(1)),
            ViewKind::Week => Range::around(self.kind, self.first + Days::new(7)),
            ViewKind::Month => Range::around(self.kind, self.month() + Months::new(1)),
        }
    }

    /// The range before this one.
    pub fn previous(self) -> Range {
        match self.kind {
            ViewKind::Day => Range::around(self.kind, self.first - Days::new(1)),
            ViewKind::Week => Range::around(self.kind, self.first - Days::new(7)),
            ViewKind::Month => Range::around(self.kind, self.month() - Months::new(1)),
        }
    }

    /// The month a [`ViewKind::Month`] range shows. `first` can sit up
    /// to six days into the month before, so a week ahead always lands
    /// in the month the grid is actually showing.
    pub fn month(self) -> NaiveDate {
        let inside = self.first + Days::new(7);
        inside.with_day(1).unwrap_or(inside)
    }

    /// Local midnight of `first` to local midnight after the last day.
    pub fn span<Z: TimeZone>(self, tz: &Z) -> (EpochMillis, EpochMillis) {
        let last = self.first + Days::new(u64::from(self.days - 1));
        (
            local_midnight(self.first, tz),
            local_midnight(last + Days::new(1), tz),
        )
    }

    /// The range's title as a bold part, a dimmed part, and a week tag
    /// ("W39"), empty for a month.
    pub fn title(self) -> (String, String, String) {
        match self.kind {
            ViewKind::Day => (
                self.first
                    .format_localized(&gettext("%A %-d"), date_locale())
                    .to_string(),
                self.first
                    .format_localized(&gettext("%B %Y"), date_locale())
                    .to_string(),
                week_tag(self.first),
            ),
            ViewKind::Week => {
                let last = self.first + Days::new(6);
                let bold = if self.first.month() == last.month() {
                    self.first
                        .format_localized(&gettext("%B"), date_locale())
                        .to_string()
                } else {
                    fill(
                        &gettext("{first} – {last}"),
                        &[
                            (
                                "first",
                                &self
                                    .first
                                    .format_localized(&gettext("%b"), date_locale())
                                    .to_string(),
                            ),
                            (
                                "last",
                                &last
                                    .format_localized(&gettext("%b"), date_locale())
                                    .to_string(),
                            ),
                        ],
                    )
                };
                let dim = last
                    .format_localized(&gettext("%Y"), date_locale())
                    .to_string();
                (bold, dim, week_tag(self.first))
            }
            ViewKind::Month => {
                let month = self.month();
                (
                    month
                        .format_localized(&gettext("%B"), date_locale())
                        .to_string(),
                    month
                        .format_localized(&gettext("%Y"), date_locale())
                        .to_string(),
                    String::new(),
                )
            }
        }
    }
}

/// The narrow agenda's first window: `today` and the 59 days after it,
/// 60 in all.
pub fn agenda_window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    (today, today + Days::new(AGENDA_WINDOW - 1))
}

/// The next earlier day the narrow agenda loads once the reader scrolls
/// to the top of what it already holds: 30 days before `first`.
pub fn earlier(first: NaiveDate) -> NaiveDate {
    first - Days::new(AGENDA_STEP)
}

/// The earliest day the local copy could hold events for, counting back
/// from `today` by [`FIRST_READ_BACK`]. The copy keeps no record of
/// when its first read ran, so this is as close as the agenda can get
/// to knowing where its data runs out.
pub fn earliest_kept_day(today: NaiveDate) -> NaiveDate {
    today - Days::new((FIRST_READ_BACK / DAY_MS) as u64)
}

/// The Monday on or before `day`.
fn monday_of(day: NaiveDate) -> NaiveDate {
    day - Days::new(u64::from(day.weekday().num_days_from_monday()))
}

/// The ISO week tag a range's title carries, such as "W39".
fn week_tag(date: NaiveDate) -> String {
    fill(
        &gettext("W{week}"),
        &[("week", &date.iso_week().week().to_string())],
    )
}

/// Local midnight at the start of `date`, or the first hour of it that
/// exists: a clock change can push local midnight into a gap that never
/// happens.
fn local_midnight<Z: TimeZone>(date: NaiveDate, tz: &Z) -> EpochMillis {
    (0..24)
        .find_map(|hour| {
            date.and_hms_opt(hour, 0, 0)
                .and_then(|naive| tz.from_local_datetime(&naive).earliest())
        })
        .map(|at| at.timestamp_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn a_week_runs_monday_to_sunday() {
        let week = Range::around(ViewKind::Week, d(2026, 9, 23));
        assert_eq!((week.first, week.days), (d(2026, 9, 21), 7));
        assert_eq!(week.next().first, d(2026, 9, 28));
        assert_eq!(week.previous().first, d(2026, 9, 14));
    }

    #[test]
    fn a_month_is_six_weeks_from_the_monday_before_the_first() {
        let month = Range::around(ViewKind::Month, d(2026, 9, 23));
        assert_eq!((month.first, month.days), (d(2026, 8, 31), 42));
        assert_eq!(month.month(), d(2026, 9, 1));
        assert_eq!(month.next().month(), d(2026, 10, 1));
        assert_eq!(month.previous().month(), d(2026, 8, 1));
    }

    #[test]
    fn the_week_title_names_the_month_the_year_and_the_week() {
        let (bold, dim, tag) = Range::around(ViewKind::Week, d(2026, 9, 23)).title();
        assert_eq!(
            (bold.as_str(), dim.as_str(), tag.as_str()),
            ("September", "2026", "W39")
        );
    }

    #[test]
    fn a_week_across_two_months_names_both() {
        let (bold, _, _) = Range::around(ViewKind::Week, d(2026, 10, 1)).title();
        assert_eq!(bold, "Sep – Oct");
    }

    #[test]
    fn the_day_title_names_the_weekday_the_month_and_the_week() {
        let (bold, dim, tag) = Range::around(ViewKind::Day, d(2026, 9, 23)).title();
        assert_eq!(
            (bold.as_str(), dim.as_str(), tag.as_str()),
            ("Wednesday 23", "September 2026", "W39")
        );
    }

    #[test]
    fn the_month_title_carries_no_week_tag() {
        let (bold, dim, tag) = Range::around(ViewKind::Month, d(2026, 9, 23)).title();
        assert_eq!(
            (bold.as_str(), dim.as_str(), tag.as_str()),
            ("September", "2026", "")
        );
    }

    #[test]
    fn the_agenda_window_covers_sixty_days_from_today() {
        assert_eq!(
            agenda_window(d(2026, 9, 23)),
            (d(2026, 9, 23), d(2026, 11, 21))
        );
    }

    #[test]
    fn earlier_steps_back_thirty_days() {
        assert_eq!(earlier(d(2026, 9, 23)), d(2026, 8, 24));
    }

    #[test]
    fn the_earliest_kept_day_is_a_year_before_today() {
        assert_eq!(earliest_kept_day(d(2026, 9, 23)), d(2025, 9, 23));
    }

    #[test]
    fn a_day_range_spans_local_midnight_to_the_next() {
        use chrono::TimeZone;
        let day = Range::around(ViewKind::Day, d(2026, 9, 23));
        let (from, to) = day.span(&chrono::Utc);
        assert_eq!(
            from,
            chrono::Utc
                .from_utc_datetime(&d(2026, 9, 23).and_hms_opt(0, 0, 0).unwrap())
                .timestamp_millis()
        );
        assert_eq!(
            to,
            chrono::Utc
                .from_utc_datetime(&d(2026, 9, 24).and_hms_opt(0, 0, 0).unwrap())
                .timestamp_millis()
        );
    }
}
