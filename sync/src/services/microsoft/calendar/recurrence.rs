//! Graph's patterned recurrence and iCalendar's RRULE, both ways. Every
//! Graph pattern has an RRULE; an RRULE Graph has no pattern for is
//! refused on the way out rather than written as something else.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use mailrs_graph::{PatternedRecurrence, RecurrencePattern, RecurrenceRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unmapped(pub String);

const DAYS: [(&str, &str); 7] = [
    ("sunday", "SU"),
    ("monday", "MO"),
    ("tuesday", "TU"),
    ("wednesday", "WE"),
    ("thursday", "TH"),
    ("friday", "FR"),
    ("saturday", "SA"),
];

const INDEX: [(&str, i32); 5] = [("first", 1), ("second", 2), ("third", 3), ("fourth", 4), ("last", -1)];

fn byday(days: &[String]) -> Option<String> {
    let codes: Vec<&str> = days
        .iter()
        .filter_map(|d| DAYS.iter().find(|(name, _)| name.eq_ignore_ascii_case(d)).map(|(_, code)| *code))
        .collect();
    (!codes.is_empty()).then(|| codes.join(","))
}

/// The `RRULE:` line for `recurrence`, or `None` for a pattern Graph did
/// not name. `zone` is where the series keeps its wall-clock times: an end
/// day stops the series at the end of that day there. An all-day series
/// ends on a date.
pub(crate) fn rrule_of(recurrence: &PatternedRecurrence, zone: Tz, all_day: bool) -> Option<String> {
    let p = &recurrence.pattern;
    let freq = match p.kind.as_str() {
        "daily" => "DAILY",
        "weekly" => "WEEKLY",
        "absoluteMonthly" | "relativeMonthly" => "MONTHLY",
        "absoluteYearly" | "relativeYearly" => "YEARLY",
        _ => return None,
    };
    let mut parts = vec![format!("FREQ={freq}"), format!("INTERVAL={}", p.interval.max(1))];
    let yearly = matches!(p.kind.as_str(), "absoluteYearly" | "relativeYearly");
    if yearly && p.month > 0 {
        parts.push(format!("BYMONTH={}", p.month));
    }
    let names_days = matches!(p.kind.as_str(), "weekly" | "relativeMonthly" | "relativeYearly");
    if let Some(days) = byday(&p.days_of_week).filter(|_| names_days) {
        parts.push(format!("BYDAY={days}"));
    }
    if matches!(p.kind.as_str(), "absoluteMonthly" | "absoluteYearly") && p.day_of_month > 0 {
        parts.push(format!("BYMONTHDAY={}", p.day_of_month));
    }
    if matches!(p.kind.as_str(), "relativeMonthly" | "relativeYearly") {
        let index = p.index.as_deref().unwrap_or("first");
        let position = INDEX.iter().find(|(name, _)| *name == index).map_or(1, |(_, n)| *n);
        parts.push(format!("BYSETPOS={position}"));
    }
    let r = &recurrence.range;
    match r.kind.as_str() {
        "numbered" if r.number_of_occurrences > 0 => parts.push(format!("COUNT={}", r.number_of_occurrences)),
        "endDate" => {
            if let Some(end) = r.end_date.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
                parts.push(format!("UNTIL={}", until_text(end, zone, all_day)?));
            }
        }
        _ => {}
    }
    Some(format!("RRULE:{}", parts.join(";")))
}

/// The `UNTIL` value that keeps the occurrence on `last` and drops the
/// next: the end of that day in `zone`, in UTC.
fn until_text(last: NaiveDate, zone: Tz, all_day: bool) -> Option<String> {
    if all_day {
        return Some(last.format("%Y%m%d").to_string());
    }
    let end = last.and_time(NaiveTime::from_hms_opt(23, 59, 59)?);
    let at = zone.from_local_datetime(&end).latest()?.with_timezone(&Utc);
    Some(at.format("%Y%m%dT%H%M%SZ").to_string())
}

/// The day an `UNTIL` value names in `zone`: a date as it stands, an
/// instant as the day it falls on there.
fn until_day(value: &str, zone: Tz) -> Option<NaiveDate> {
    if let Some(utc) = value.strip_suffix('Z') {
        let at = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some(at.and_utc().with_timezone(&zone).date_naive());
    }
    NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d").ok()
}

/// One `BYDAY` entry: the weekday's name and its ordinal, if it has one.
fn day_entry(token: &str) -> Option<(&'static str, Option<i32>)> {
    let split = token.len().checked_sub(2)?;
    let (number, code) = token.split_at_checked(split)?;
    let name = DAYS.iter().find(|(_, c)| *c == code)?.0;
    let ordinal = if number.is_empty() { None } else { Some(number.parse().ok()?) };
    Some((name, ordinal))
}

/// Graph's recurrence for the `RRULE` among `rules`, for a series starting
/// on `start` in `zone`; `None` when the event does not repeat. Reads every
/// shape the editor writes: no `INTERVAL`, the day left to the start,
/// `BYDAY=2TU`, and an end as a date or as an instant. `EXDATE` and
/// `RDATE` lines are not Graph's to take on a write; an occurrence taken
/// out goes as its own delete.
pub(crate) fn recurrence_of(rules: &[String], start: NaiveDate, zone: Tz) -> Result<Option<PatternedRecurrence>, Unmapped> {
    let Some(rule) = rules.iter().find_map(|r| r.strip_prefix("RRULE:")) else {
        return Ok(None);
    };
    let refuse = || Unmapped(rule.to_string());
    let mut pattern = RecurrencePattern { interval: 1, ..RecurrencePattern::default() };
    let mut range = RecurrenceRange { kind: "noEnd".into(), start_date: start.format("%Y-%m-%d").to_string(), ..RecurrenceRange::default() };
    let (mut freq, mut position, mut days) = (String::new(), None::<i32>, Vec::<String>::new());
    for part in rule.split(';') {
        let (key, value) = part.split_once('=').ok_or_else(refuse)?;
        match key {
            "FREQ" => freq = value.to_string(),
            "INTERVAL" => pattern.interval = value.parse().map_err(|_| refuse())?,
            "BYDAY" => {
                let entries: Vec<_> = value.split(',').map(day_entry).collect::<Option<_>>().ok_or_else(refuse)?;
                if entries.iter().any(|(_, n)| n.is_some()) {
                    // "The second Tuesday" is one weekday and one ordinal.
                    let [(name, Some(n))] = entries.as_slice() else {
                        return Err(refuse());
                    };
                    position = Some(*n);
                    days.push((*name).to_string());
                } else {
                    days.extend(entries.iter().map(|(name, _)| (*name).to_string()));
                }
            }
            "BYMONTHDAY" if !value.contains(',') => pattern.day_of_month = value.parse().map_err(|_| refuse())?,
            "BYMONTH" if !value.contains(',') => pattern.month = value.parse().map_err(|_| refuse())?,
            "BYSETPOS" => position = Some(value.parse().map_err(|_| refuse())?),
            "COUNT" => {
                range.kind = "numbered".into();
                range.number_of_occurrences = value.parse().map_err(|_| refuse())?;
            }
            "UNTIL" => {
                let day = until_day(value, zone).ok_or_else(refuse)?;
                range.kind = "endDate".into();
                range.end_date = Some(day.format("%Y-%m-%d").to_string());
            }
            "WKST" => {}
            _ => return Err(refuse()),
        }
    }
    if let Some(n) = position {
        pattern.index = Some(INDEX.iter().find(|(_, i)| *i == n).ok_or_else(refuse)?.0.to_string());
    }
    let kind = match (freq.as_str(), position.is_some(), days.is_empty()) {
        ("DAILY", false, true) => "daily",
        ("WEEKLY", false, _) => "weekly",
        ("MONTHLY", false, true) => "absoluteMonthly",
        ("MONTHLY", true, false) => "relativeMonthly",
        ("YEARLY", false, true) => "absoluteYearly",
        ("YEARLY", true, false) => "relativeYearly",
        _ => return Err(refuse()),
    };
    pattern.kind = kind.to_string();
    // A rule that leaves the day or month out takes them from the start.
    if matches!(kind, "absoluteMonthly" | "absoluteYearly") && pattern.day_of_month == 0 {
        pattern.day_of_month = start.day();
    }
    if matches!(kind, "absoluteYearly" | "relativeYearly") && pattern.month == 0 {
        pattern.month = start.month();
    }
    if kind == "weekly" && days.is_empty() {
        let weekday = usize::try_from(start.weekday().num_days_from_sunday()).unwrap_or(0);
        days.push(DAYS[weekday].0.to_string());
    }
    pattern.days_of_week = days;
    Ok(Some(PatternedRecurrence { pattern, range }))
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, Weekday};
    use chrono_tz::{Tz, UTC};
    use mailrs_domain::calendar::repeat::{Custom, Ends, Frequency, Repeat};
    use mailrs_graph::{PatternedRecurrence, RecurrencePattern, RecurrenceRange};

    use super::*;

    fn pattern(kind: &str) -> RecurrencePattern {
        RecurrencePattern { kind: kind.into(), interval: 1, ..RecurrencePattern::default() }
    }

    fn range(kind: &str) -> RecurrenceRange {
        RecurrenceRange { kind: kind.into(), start_date: "2026-10-05".into(), ..RecurrenceRange::default() }
    }

    fn start() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
    }

    fn both_ways(recurrence: PatternedRecurrence, rule: &str) {
        assert_eq!(rrule_of(&recurrence, UTC, false).as_deref(), Some(rule));
        let back = recurrence_of(&[rule.to_string()], start(), UTC).unwrap().unwrap();
        assert_eq!(rrule_of(&back, UTC, false).as_deref(), Some(rule), "{back:?}");
    }

    #[test]
    fn every_graph_pattern_has_an_rrule_and_comes_back() {
        both_ways(PatternedRecurrence { pattern: pattern("daily"), range: range("noEnd") }, "RRULE:FREQ=DAILY;INTERVAL=1");
        both_ways(
            PatternedRecurrence {
                pattern: RecurrencePattern { days_of_week: vec!["monday".into(), "wednesday".into()], interval: 2, ..pattern("weekly") },
                range: RecurrenceRange { number_of_occurrences: 10, ..range("numbered") },
            },
            "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=10",
        );
        both_ways(
            PatternedRecurrence { pattern: RecurrencePattern { day_of_month: 15, ..pattern("absoluteMonthly") }, range: range("noEnd") },
            "RRULE:FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=15",
        );
        both_ways(
            PatternedRecurrence {
                pattern: RecurrencePattern { days_of_week: vec!["tuesday".into()], index: Some("second".into()), ..pattern("relativeMonthly") },
                range: RecurrenceRange { end_date: Some("2027-06-30".into()), ..range("endDate") },
            },
            "RRULE:FREQ=MONTHLY;INTERVAL=1;BYDAY=TU;BYSETPOS=2;UNTIL=20270630T235959Z",
        );
        both_ways(
            PatternedRecurrence { pattern: RecurrencePattern { month: 3, day_of_month: 9, ..pattern("absoluteYearly") }, range: range("noEnd") },
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYMONTH=3;BYMONTHDAY=9",
        );
        both_ways(
            PatternedRecurrence {
                pattern: RecurrencePattern { month: 3, days_of_week: vec!["friday".into()], index: Some("last".into()), ..pattern("relativeYearly") },
                range: range("noEnd"),
            },
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYMONTH=3;BYDAY=FR;BYSETPOS=-1",
        );
    }

    #[test]
    fn a_rule_outlook_cannot_hold_is_refused() {
        assert!(recurrence_of(&["RRULE:FREQ=HOURLY;INTERVAL=1".into()], start(), UTC).is_err());
        assert!(recurrence_of(&["RRULE:FREQ=MONTHLY;BYMONTHDAY=1,15".into()], start(), UTC).is_err());
        assert!(recurrence_of(&["RRULE:FREQ=WEEKLY;BYDAY=MO;BYHOUR=9".into()], start(), UTC).is_err());
        assert_eq!(recurrence_of(&[], start(), UTC).unwrap(), None);
    }

    /// Every choice of the editor's Repeat menu writes a rule that Outlook
    /// can hold, for a timed and for an all-day event.
    #[test]
    fn every_repeat_choice_the_editor_writes_maps_to_a_recurrence() {
        let day = start();
        let zone: Tz = "Europe/Lisbon".parse().unwrap();
        let custom = |every, frequency, days: Vec<Weekday>, ends| Repeat::Custom(Custom { every, frequency, days, ends });
        let mut choices = mailrs_domain::calendar::repeat::presets(day);
        choices.extend([
            Repeat::MonthlyByDay(-1, Weekday::Fri),
            Repeat::MonthlyByDay(2, Weekday::Tue),
            custom(1, Frequency::Daily, vec![], Ends::Never),
            custom(3, Frequency::Daily, vec![], Ends::After(4)),
            custom(2, Frequency::Weekly, vec![Weekday::Mon, Weekday::Thu], Ends::On(NaiveDate::from_ymd_opt(2027, 1, 31).unwrap())),
            custom(1, Frequency::Weekly, vec![], Ends::Never),
            custom(2, Frequency::Monthly, vec![], Ends::After(6)),
            custom(1, Frequency::Yearly, vec![], Ends::On(NaiveDate::from_ymd_opt(2030, 10, 5).unwrap())),
        ]);
        for all_day in [false, true] {
            for choice in &choices {
                let Some(rule) = choice.rule(day, zone, all_day) else {
                    assert_eq!(*choice, Repeat::Never);
                    continue;
                };
                let found = recurrence_of(std::slice::from_ref(&rule), day, zone);
                assert!(matches!(found, Ok(Some(_))), "{rule} (all day: {all_day}): {found:?}");
            }
        }
    }

    #[test]
    fn the_day_a_rule_leaves_out_comes_from_the_start() {
        let monthly = recurrence_of(&["RRULE:FREQ=MONTHLY".into()], start(), UTC).unwrap().unwrap();
        assert_eq!((monthly.pattern.kind.as_str(), monthly.pattern.day_of_month, monthly.pattern.interval), ("absoluteMonthly", 5, 1));
        let yearly = recurrence_of(&["RRULE:FREQ=YEARLY".into()], start(), UTC).unwrap().unwrap();
        assert_eq!((yearly.pattern.month, yearly.pattern.day_of_month), (10, 5));
        let weekly = recurrence_of(&["RRULE:FREQ=WEEKLY".into()], start(), UTC).unwrap().unwrap();
        assert_eq!(weekly.pattern.days_of_week, vec!["monday".to_string()]);
    }

    #[test]
    fn an_ordinal_weekday_is_a_relative_pattern() {
        let second = recurrence_of(&["RRULE:FREQ=MONTHLY;BYDAY=2TU".into()], start(), UTC).unwrap().unwrap();
        assert_eq!(second.pattern.kind, "relativeMonthly");
        assert_eq!(second.pattern.index.as_deref(), Some("second"));
        assert_eq!(second.pattern.days_of_week, vec!["tuesday".to_string()]);
        let last = recurrence_of(&["RRULE:FREQ=MONTHLY;BYDAY=-1FR".into()], start(), UTC).unwrap().unwrap();
        assert_eq!(last.pattern.index.as_deref(), Some("last"));
    }

    #[test]
    fn an_end_day_is_read_in_the_series_zone() {
        // 23:59:59 on 2027-01-31 in New York is 04:59:59 UTC the day after.
        let york: Tz = "America/New_York".parse().unwrap();
        let rule = "RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20270201T045959Z".to_string();
        let found = recurrence_of(&[rule], start(), york).unwrap().unwrap();
        assert_eq!(found.range.end_date.as_deref(), Some("2027-01-31"));
        assert_eq!(rrule_of(&found, york, false).as_deref(), Some("RRULE:FREQ=WEEKLY;INTERVAL=1;BYDAY=MO;UNTIL=20270201T045959Z"));
    }

    #[test]
    fn an_all_day_series_ends_on_a_date() {
        let found = recurrence_of(&["RRULE:FREQ=DAILY;UNTIL=20261231".into()], start(), UTC).unwrap().unwrap();
        assert_eq!(rrule_of(&found, UTC, true).as_deref(), Some("RRULE:FREQ=DAILY;INTERVAL=1;UNTIL=20261231"));
    }
}
