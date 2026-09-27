//! The repeat menu's choices and the `RRULE` line each one writes, and
//! the way back from a rule to the choice that shows it. A rule the menu
//! cannot show, such as "the first Monday of the month", stays as it came.

use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone, Weekday};
use chrono_tz::Tz;

use super::is_rule_line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    fn word(self) -> &'static str {
        match self {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
        }
    }

    fn parse(word: &str) -> Option<Frequency> {
        Some(match word {
            "DAILY" => Frequency::Daily,
            "WEEKLY" => Frequency::Weekly,
            "MONTHLY" => Frequency::Monthly,
            "YEARLY" => Frequency::Yearly,
            _ => return None,
        })
    }
}

/// When a custom repeat stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ends {
    Never,
    /// After the occurrence on this day, in the event's zone.
    On(NaiveDate),
    After(u32),
}

/// What the Custom page sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Custom {
    pub every: u32,
    pub frequency: Frequency,
    /// For a weekly repeat, the days it falls on. Empty means the start's.
    pub days: Vec<Weekday>,
    pub ends: Ends,
}

/// One choice of the repeat menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repeat {
    Never,
    EveryDay,
    EveryWeekday,
    EveryWeek,
    EveryMonth,
    EveryYear,
    Custom(Custom),
    /// A rule the menu cannot show, kept whole.
    Kept(String),
}

const WEEKDAYS: [Weekday; 5] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
];

fn code(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

fn day_of(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn days_text(days: &[Weekday]) -> String {
    let mut days = days.to_vec();
    days.sort_by_key(|d| d.num_days_from_monday());
    days.dedup();
    days.iter().map(|d| code(*d)).collect::<Vec<_>>().join(",")
}

impl Repeat {
    /// The `RRULE` line for a series whose first occurrence falls on `day`
    /// in `zone`, or `None` for one that does not repeat.
    pub fn rule(&self, day: NaiveDate, zone: Tz, all_day: bool) -> Option<String> {
        Some(match self {
            Repeat::Never => return None,
            Repeat::EveryDay => "RRULE:FREQ=DAILY".to_string(),
            Repeat::EveryWeekday => format!("RRULE:FREQ=WEEKLY;BYDAY={}", days_text(&WEEKDAYS)),
            Repeat::EveryWeek => format!("RRULE:FREQ=WEEKLY;BYDAY={}", code(day.weekday())),
            Repeat::EveryMonth => "RRULE:FREQ=MONTHLY".to_string(),
            Repeat::EveryYear => "RRULE:FREQ=YEARLY".to_string(),
            Repeat::Kept(line) => line.clone(),
            Repeat::Custom(custom) => {
                let mut parts = vec![format!("FREQ={}", custom.frequency.word())];
                if custom.every > 1 {
                    parts.push(format!("INTERVAL={}", custom.every));
                }
                if custom.frequency == Frequency::Weekly && !custom.days.is_empty() {
                    parts.push(format!("BYDAY={}", days_text(&custom.days)));
                }
                match custom.ends {
                    Ends::Never => {}
                    Ends::After(count) => parts.push(format!("COUNT={count}")),
                    Ends::On(last) if all_day => {
                        parts.push(format!("UNTIL={}", last.format("%Y%m%d")))
                    }
                    Ends::On(last) => {
                        // The end of that day where the event lives, in UTC,
                        // as RFC 5545 wants beside a DTSTART with a zone.
                        let end = last.and_time(NaiveTime::from_hms_opt(23, 59, 59)?);
                        let at = zone
                            .from_local_datetime(&end)
                            .latest()?
                            .with_timezone(&chrono::Utc);
                        parts.push(format!("UNTIL={}", at.format("%Y%m%dT%H%M%SZ")));
                    }
                }
                format!("RRULE:{}", parts.join(";"))
            }
        })
    }

    /// The menu choice that shows `rules`, for a series starting on `day`.
    /// Google allows more than one `RRULE` line. The menu has no way to
    /// show two rules at once, so a series with more than one keeps its
    /// first as `Kept`, whole, rather than dropping the rest.
    pub fn read(rules: &[String], day: NaiveDate, zone: Tz) -> Repeat {
        let mut lines = rules.iter().filter(|l| is_rule_line(l));
        let Some(line) = lines.next() else {
            return Repeat::Never;
        };
        if lines.next().is_some() {
            return Repeat::Kept(line.clone());
        }
        Repeat::parse(line, day, zone).unwrap_or_else(|| Repeat::Kept(line.clone()))
    }

    fn parse(line: &str, day: NaiveDate, zone: Tz) -> Option<Repeat> {
        let body = line.split_once(':')?.1.to_ascii_uppercase();
        let mut frequency = None;
        let (mut every, mut days, mut ends) = (1, Vec::new(), Ends::Never);
        for part in body.split(';') {
            let (key, value) = part.split_once('=')?;
            match key {
                "FREQ" => frequency = Frequency::parse(value),
                "INTERVAL" => every = value.parse().ok()?,
                "BYDAY" => days = value.split(',').map(day_of).collect::<Option<Vec<_>>>()?,
                "COUNT" => ends = Ends::After(value.parse().ok()?),
                "UNTIL" => ends = Ends::On(until_day(value, zone)?),
                "WKST" => {}
                _ => return None,
            }
        }
        let frequency = frequency?;
        if !days.is_empty() && frequency != Frequency::Weekly {
            return None;
        }
        days.sort_by_key(|d| d.num_days_from_monday());
        let plain = every == 1 && ends == Ends::Never;
        Some(match frequency {
            Frequency::Daily if plain => Repeat::EveryDay,
            Frequency::Weekly if plain && days == WEEKDAYS => Repeat::EveryWeekday,
            Frequency::Weekly if plain && (days.is_empty() || days == [day.weekday()]) => {
                Repeat::EveryWeek
            }
            Frequency::Monthly if plain => Repeat::EveryMonth,
            Frequency::Yearly if plain => Repeat::EveryYear,
            _ => Repeat::Custom(Custom {
                every,
                frequency,
                days,
                ends,
            }),
        })
    }
}

/// Whether `rules` repeat weekly on days they list, the kind of series
/// whose `BYDAY` has to follow the series to another weekday.
pub(crate) fn names_weekdays(rules: &[String], day: NaiveDate, zone: Tz) -> bool {
    match Repeat::read(rules, day, zone) {
        Repeat::EveryWeek => true,
        Repeat::Custom(custom) => custom.frequency == Frequency::Weekly && !custom.days.is_empty(),
        _ => false,
    }
}

/// `line` with each weekday its `BYDAY` lists moved `days` later, and
/// every other part, such as `INTERVAL`, `COUNT` or `UNTIL`, as written.
pub(crate) fn later_weekdays(line: &str, days: i64) -> String {
    let Some((head, body)) = line.split_once(':') else {
        return line.to_string();
    };
    let parts: Vec<String> = body
        .split(';')
        .map(|part| match part.split_once('=') {
            Some((key, value)) if key.eq_ignore_ascii_case("BYDAY") => {
                let moved: Option<Vec<&str>> = value
                    .split(',')
                    .map(|c| {
                        let from = day_of(&c.to_ascii_uppercase())?;
                        let to = (i64::from(from.num_days_from_monday()) + days).rem_euclid(7);
                        Some(code(Weekday::try_from(u8::try_from(to).ok()?).ok()?))
                    })
                    .collect();
                moved.map_or_else(|| part.to_string(), |m| format!("{key}={}", m.join(",")))
            }
            _ => part.to_string(),
        })
        .collect();
    format!("{head}:{}", parts.join(";"))
}

/// The last day an `UNTIL` covers, in the event's zone.
fn until_day(value: &str, zone: Tz) -> Option<NaiveDate> {
    if value.len() == 8 {
        return NaiveDate::parse_from_str(value, "%Y%m%d").ok();
    }
    let utc =
        chrono::NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    Some(utc.and_utc().with_timezone(&zone).date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Lisbon;

    fn wednesday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()
    }

    fn rule(repeat: Repeat) -> Option<String> {
        repeat.rule(wednesday(), Lisbon, false)
    }

    fn custom(every: u32, frequency: Frequency, days: &[Weekday], ends: Ends) -> Repeat {
        Repeat::Custom(Custom {
            every,
            frequency,
            days: days.to_vec(),
            ends,
        })
    }

    #[test]
    fn each_menu_choice_writes_its_rule() {
        assert_eq!(rule(Repeat::Never), None);
        assert_eq!(rule(Repeat::EveryDay).as_deref(), Some("RRULE:FREQ=DAILY"));
        assert_eq!(
            rule(Repeat::EveryWeekday).as_deref(),
            Some("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")
        );
        assert_eq!(
            rule(Repeat::EveryWeek).as_deref(),
            Some("RRULE:FREQ=WEEKLY;BYDAY=WE")
        );
        assert_eq!(
            rule(Repeat::EveryMonth).as_deref(),
            Some("RRULE:FREQ=MONTHLY")
        );
        assert_eq!(
            rule(Repeat::EveryYear).as_deref(),
            Some("RRULE:FREQ=YEARLY")
        );
    }

    #[test]
    fn a_custom_repeat_writes_interval_days_and_end() {
        let until = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        // Lisbon is on UTC in December.
        assert_eq!(
            rule(custom(
                2,
                Frequency::Weekly,
                &[Weekday::Wed, Weekday::Mon],
                Ends::On(until)
            ))
            .as_deref(),
            Some("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20261231T235959Z")
        );
        assert_eq!(
            rule(custom(1, Frequency::Daily, &[], Ends::After(5))).as_deref(),
            Some("RRULE:FREQ=DAILY;COUNT=5")
        );
    }

    #[test]
    fn a_summer_end_date_is_written_in_utc() {
        let until = NaiveDate::from_ymd_opt(2027, 7, 31).unwrap();
        assert_eq!(
            rule(custom(1, Frequency::Daily, &[], Ends::On(until))).as_deref(),
            Some("RRULE:FREQ=DAILY;UNTIL=20270731T225959Z")
        );
    }

    #[test]
    fn an_all_day_series_ends_on_a_date() {
        let until = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        let repeat = custom(1, Frequency::Weekly, &[], Ends::On(until));
        assert_eq!(
            repeat.rule(wednesday(), Lisbon, true).as_deref(),
            Some("RRULE:FREQ=WEEKLY;UNTIL=20261231")
        );
    }

    #[test]
    fn a_rule_reads_back_as_the_choice_that_wrote_it() {
        for repeat in [
            Repeat::EveryDay,
            Repeat::EveryWeekday,
            Repeat::EveryWeek,
            Repeat::EveryMonth,
            Repeat::EveryYear,
        ] {
            let lines = vec![rule(repeat.clone()).unwrap()];
            assert_eq!(Repeat::read(&lines, wednesday(), Lisbon), repeat);
        }
        let until = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        let two_weeks = custom(
            2,
            Frequency::Weekly,
            &[Weekday::Mon, Weekday::Wed],
            Ends::On(until),
        );
        assert_eq!(
            Repeat::read(&[rule(two_weeks.clone()).unwrap()], wednesday(), Lisbon),
            two_weeks
        );
        assert_eq!(Repeat::read(&[], wednesday(), Lisbon), Repeat::Never);
    }

    #[test]
    fn a_weekly_rule_on_another_day_is_custom() {
        let read = Repeat::read(&["RRULE:FREQ=WEEKLY;BYDAY=FR".into()], wednesday(), Lisbon);
        assert_eq!(
            read,
            custom(1, Frequency::Weekly, &[Weekday::Fri], Ends::Never)
        );
    }

    #[test]
    fn a_rule_the_menu_cannot_show_is_kept_as_it_is() {
        let line = "RRULE:FREQ=MONTHLY;BYDAY=1MO";
        assert_eq!(
            Repeat::read(&[line.into()], wednesday(), Lisbon),
            Repeat::Kept(line.into())
        );
        assert_eq!(rule(Repeat::Kept(line.into())).as_deref(), Some(line));
        let setpos = "RRULE:FREQ=MONTHLY;BYDAY=MO,TU;BYSETPOS=-1";
        assert_eq!(
            Repeat::read(&[setpos.into()], wednesday(), Lisbon),
            Repeat::Kept(setpos.into())
        );
    }

    #[test]
    fn two_repeat_rules_are_kept_as_they_came() {
        let first = "RRULE:FREQ=DAILY";
        let second = "RRULE:FREQ=WEEKLY;BYDAY=MO";
        let read = Repeat::read(&[first.into(), second.into()], wednesday(), Lisbon);
        assert_eq!(read, Repeat::Kept(first.into()));
    }
}
