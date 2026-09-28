//! Which range of a calendar the copy still has to fetch, when the person
//! goes further back than the first read went.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use mailrs_domain::EpochMillis;

/// The range to fetch, `from` to `to`, so a copy that holds everything
/// ending after `reach` also holds everything ending after `wanted`. It is
/// `None` when the copy already reaches that far, or when its reach is
/// unknown because its first read has not happened. The start is the first
/// of `wanted`'s month in UTC, so a week and the week before it ask for
/// one range, not two, and the end is where the copy's reach began, so the
/// ranges join without a gap or a repeat.
pub fn missing_range(reach: Option<EpochMillis>, wanted: EpochMillis) -> Option<(EpochMillis, EpochMillis)> {
    let reach = reach?;
    if wanted >= reach {
        return None;
    }
    let month_start = DateTime::<Utc>::from_timestamp_millis(wanted)
        .and_then(|at| Utc.with_ymd_and_hms(at.year(), at.month(), 1, 0, 0, 0).single())
        .map_or(0, |first| first.timestamp_millis());
    Some((month_start.max(0), reach))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u32, day: u32) -> EpochMillis {
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0).unwrap().timestamp_millis()
    }

    #[test]
    fn a_week_before_the_reach_asks_for_its_month_up_to_the_reach() {
        let reach = at(2025, 9, 23);
        let (from, to) = missing_range(Some(reach), at(2024, 9, 10)).unwrap();
        assert_eq!(from, Utc.with_ymd_and_hms(2024, 9, 1, 0, 0, 0).unwrap().timestamp_millis());
        assert_eq!(to, reach);
    }

    #[test]
    fn a_range_the_copy_already_reaches_asks_for_nothing() {
        let reach = at(2025, 9, 23);
        assert_eq!(missing_range(Some(reach), at(2025, 9, 23)), None);
        assert_eq!(missing_range(Some(reach), at(2026, 1, 1)), None);
    }

    #[test]
    fn a_copy_never_read_asks_for_nothing() {
        assert_eq!(missing_range(None, at(2020, 1, 1)), None);
    }

    #[test]
    fn a_second_range_starts_where_the_first_began_so_none_is_fetched_twice() {
        let reach = at(2025, 9, 23);
        let (first_from, _) = missing_range(Some(reach), at(2025, 3, 10)).unwrap();
        // The week before, on the same screen, is already inside it.
        assert_eq!(missing_range(Some(first_from), at(2025, 3, 3)), None);
        // One month further back asks for that month alone.
        let (from, to) = missing_range(Some(first_from), at(2025, 2, 20)).unwrap();
        assert_eq!(to, first_from);
        assert_eq!(from, Utc.with_ymd_and_hms(2025, 2, 1, 0, 0, 0).unwrap().timestamp_millis());
    }

    #[test]
    fn a_date_before_1970_asks_from_the_epoch() {
        let (from, _) = missing_range(Some(at(2025, 9, 23)), at(1960, 5, 5)).unwrap();
        assert_eq!(from, 0);
    }
}
