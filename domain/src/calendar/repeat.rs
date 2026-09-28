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
    /// Monthly on the Nth occurrence of a weekday, or the last with -1:
    /// "the second Tuesday", "the last Friday". [`presets`] works the
    /// ordinal out from a date; [`Repeat::read`] recognizes one back from
    /// a plain `BYDAY=2TU` or `BYDAY=-1FR` rule with no interval or end.
    MonthlyByDay(i8, Weekday),
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

/// The ordinal and weekday a monthly `BYDAY` value such as `2TU` or
/// `-1FR` names, or `None` for a shape this does not recognize.
fn ordinal_byday(raw: &str) -> Option<(i8, Weekday)> {
    let split = raw.len().checked_sub(2)?;
    let (number, code_part) = raw.split_at(split);
    let weekday = day_of(code_part)?;
    let ordinal: i8 = number.parse().ok()?;
    ((1..=5).contains(&ordinal) || ordinal == -1).then_some((ordinal, weekday))
}

/// Which occurrence of its own weekday `day` is within its month: 1 for
/// the first Tuesday of the month, 2 for the second, and so on.
pub fn ordinal_of(day: NaiveDate) -> i8 {
    i8::try_from((day.day() - 1) / 7 + 1).unwrap_or(5)
}

/// Whether `day` falls in the last seven days of its month, the reach of
/// a monthly rule's "last" weekday: the [`presets`] menu offers that
/// choice only then, since "the last Tuesday" and "the fourth Tuesday"
/// would otherwise name two different dates.
pub fn in_last_week(day: NaiveDate) -> bool {
    last_day_of_month(day).day() - day.day() < 7
}

fn last_day_of_month(day: NaiveDate) -> NaiveDate {
    let first_of_next = if day.month() == 12 {
        NaiveDate::from_ymd_opt(day.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(day.year(), day.month() + 1, 1)
    };
    first_of_next
        .and_then(|d| d.pred_opt())
        .unwrap_or(day)
}

/// The Repeats row's choices for a series starting on `day`: the six
/// fixed ones, a monthly ordinal worked out from `day` itself ("the
/// second Tuesday"), and, only in the last seven days of the month
/// ([`in_last_week`]), "the last" weekday too, as Google Calendar offers
/// it.
pub fn presets(day: NaiveDate) -> Vec<Repeat> {
    let mut list = vec![
        Repeat::Never,
        Repeat::EveryDay,
        Repeat::EveryWeekday,
        Repeat::EveryWeek,
        Repeat::EveryMonth,
        Repeat::MonthlyByDay(ordinal_of(day), day.weekday()),
    ];
    if in_last_week(day) {
        list.push(Repeat::MonthlyByDay(-1, day.weekday()));
    }
    list.push(Repeat::EveryYear);
    list
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
    /// What this choice becomes once the event's start date moves to
    /// `new_day`, for the Repeats row to carry the person's choice
    /// across a start date they change after picking it. Every choice
    /// but `MonthlyByDay` means the same thing whatever the date, so it
    /// carries over untouched (`EveryWeek`'s own weekday, for one,
    /// already follows the day it is asked to write a rule for, in
    /// [`Repeat::rule`]). A `MonthlyByDay` is a fact about a date, not a
    /// rule of its own: its ordinal and weekday are worked out again for
    /// `new_day` ([`ordinal_of`]), and its "last" ordinal (-1) falls
    /// back to the plain one when `new_day` no longer falls in its
    /// month's last week ([`in_last_week`]), since "the last Tuesday"
    /// would otherwise name a Tuesday that is not `new_day`'s own.
    pub fn carried(&self, new_day: NaiveDate) -> Repeat {
        match self {
            Repeat::MonthlyByDay(-1, _) if in_last_week(new_day) => {
                Repeat::MonthlyByDay(-1, new_day.weekday())
            }
            Repeat::MonthlyByDay(_, _) => Repeat::MonthlyByDay(ordinal_of(new_day), new_day.weekday()),
            other => other.clone(),
        }
    }

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
            Repeat::MonthlyByDay(ordinal, weekday) => {
                format!("RRULE:FREQ=MONTHLY;BYDAY={ordinal}{}", code(*weekday))
            }
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
        // A `BYDAY` that is not a plain list of weekday codes, such as
        // `2TU` or `-1FR`, is a monthly ordinal rather than a parse
        // failure; held here and read once every part is in, since `FREQ`
        // may come before or after it.
        let mut ordinal_day: Option<&str> = None;
        for part in body.split(';') {
            let (key, value) = part.split_once('=')?;
            match key {
                "FREQ" => frequency = Frequency::parse(value),
                "INTERVAL" => every = value.parse().ok()?,
                "BYDAY" => match value.split(',').map(day_of).collect::<Option<Vec<_>>>() {
                    Some(list) => days = list,
                    None if !value.contains(',') => ordinal_day = Some(value),
                    None => return None,
                },
                "COUNT" => ends = Ends::After(value.parse().ok()?),
                "UNTIL" => ends = Ends::On(until_day(value, zone)?),
                "WKST" => {}
                _ => return None,
            }
        }
        let frequency = frequency?;
        if let Some(raw) = ordinal_day {
            let plain = frequency == Frequency::Monthly && every == 1 && ends == Ends::Never;
            return plain
                .then(|| ordinal_byday(raw))
                .flatten()
                .map(|(ordinal, weekday)| Repeat::MonthlyByDay(ordinal, weekday));
        }
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

/// Whether `rules` name a weekday their own `BYDAY` has to follow the
/// series to another one: a plain weekly repeat, a custom weekly repeat
/// naming days, or a monthly ordinal ("the second Tuesday"). The rules
/// are read without their `COUNT` or `UNTIL`: a split hands a series an
/// end, and a series must move the same way before and after one.
pub(crate) fn names_weekdays(rules: &[String], day: NaiveDate, zone: Tz) -> bool {
    let endless: Vec<String> = rules.iter().map(|line| without_end(line)).collect();
    match Repeat::read(&endless, day, zone) {
        Repeat::EveryWeek | Repeat::MonthlyByDay(_, _) => true,
        Repeat::Custom(custom) => custom.frequency == Frequency::Weekly && !custom.days.is_empty(),
        _ => false,
    }
}

/// A rule line without its `COUNT` or `UNTIL`; any other line as it is.
fn without_end(line: &str) -> String {
    if !is_rule_line(line) {
        return line.to_string();
    }
    let Some((head, body)) = line.split_once(':') else {
        return line.to_string();
    };
    let parts: Vec<&str> = body
        .split(';')
        .filter(|p| {
            let p = p.to_ascii_uppercase();
            !p.starts_with("COUNT=") && !p.starts_with("UNTIL=")
        })
        .collect();
    format!("{head}:{}", parts.join(";"))
}

/// `line` with each weekday its `BYDAY` lists moved `days` later, and
/// every other part, such as `INTERVAL`, `COUNT` or `UNTIL`, as written.
/// A monthly ordinal's own number, such as the `2` in `2TU` or the `-1`
/// in `-1FR`, stays put; only the weekday code after it moves, so "the
/// second Tuesday" shifted a day becomes "the second Wednesday".
pub(crate) fn later_weekdays(line: &str, days: i64) -> String {
    let Some((head, body)) = line.split_once(':') else {
        return line.to_string();
    };
    let parts: Vec<String> = body
        .split(';')
        .map(|part| match part.split_once('=') {
            Some((key, value)) if key.eq_ignore_ascii_case("BYDAY") => {
                let moved: Option<Vec<String>> =
                    value.split(',').map(|c| shifted_byday(c, days)).collect();
                moved.map_or_else(|| part.to_string(), |m| format!("{key}={}", m.join(",")))
            }
            _ => part.to_string(),
        })
        .collect();
    format!("{head}:{}", parts.join(";"))
}

/// One `BYDAY` entry, such as `TU` or `2TU`, with its weekday code moved
/// `days` later and any ordinal prefix kept as it was.
fn shifted_byday(entry: &str, days: i64) -> Option<String> {
    let upper = entry.to_ascii_uppercase();
    let split = upper.len().checked_sub(2)?;
    let (prefix, code_part) = upper.split_at(split);
    let from = day_of(code_part)?;
    let to = (i64::from(from.num_days_from_monday()) + days).rem_euclid(7);
    let moved = code(Weekday::try_from(u8::try_from(to).ok()?).ok()?);
    Some(format!("{prefix}{moved}"))
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
    fn a_rule_the_menu_still_cannot_show_is_kept_as_it_is() {
        // Two monthly ordinal days at once is past what a single
        // `MonthlyByDay` choice can hold.
        let line = "RRULE:FREQ=MONTHLY;BYDAY=1MO,3MO";
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

    #[test]
    fn a_monthly_ordinal_choice_writes_its_rule() {
        let nth = Repeat::MonthlyByDay(4, Weekday::Wed);
        assert_eq!(rule(nth).as_deref(), Some("RRULE:FREQ=MONTHLY;BYDAY=4WE"));
        let last = Repeat::MonthlyByDay(-1, Weekday::Fri);
        assert_eq!(rule(last).as_deref(), Some("RRULE:FREQ=MONTHLY;BYDAY=-1FR"));
    }

    #[test]
    fn a_monthly_ordinal_rule_reads_back_as_editable() {
        let nth = "RRULE:FREQ=MONTHLY;BYDAY=2TU";
        assert_eq!(
            Repeat::read(&[nth.into()], wednesday(), Lisbon),
            Repeat::MonthlyByDay(2, Weekday::Tue)
        );
        let last = "RRULE:FREQ=MONTHLY;BYDAY=-1FR";
        assert_eq!(
            Repeat::read(&[last.into()], wednesday(), Lisbon),
            Repeat::MonthlyByDay(-1, Weekday::Fri)
        );
    }

    #[test]
    fn a_monthly_ordinal_rule_with_an_interval_or_an_end_is_kept_as_it_is() {
        let interval = "RRULE:FREQ=MONTHLY;INTERVAL=2;BYDAY=2TU";
        assert_eq!(
            Repeat::read(&[interval.into()], wednesday(), Lisbon),
            Repeat::Kept(interval.into())
        );
        let counted = "RRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=5";
        assert_eq!(
            Repeat::read(&[counted.into()], wednesday(), Lisbon),
            Repeat::Kept(counted.into())
        );
    }

    #[test]
    fn ordinal_of_counts_which_occurrence_of_the_weekday_a_date_is() {
        // September 2026's Wednesdays: 2, 9, 16, 23, 30.
        assert_eq!(ordinal_of(NaiveDate::from_ymd_opt(2026, 9, 2).unwrap()), 1);
        assert_eq!(ordinal_of(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap()), 2);
        assert_eq!(ordinal_of(wednesday()), 4);
        assert_eq!(ordinal_of(NaiveDate::from_ymd_opt(2026, 9, 30).unwrap()), 5);
    }

    #[test]
    fn in_last_week_reads_true_only_for_the_last_seven_days_of_the_month() {
        // September 2026 has 30 days.
        assert!(!in_last_week(NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()));
        assert!(in_last_week(NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()));
        assert!(in_last_week(NaiveDate::from_ymd_opt(2026, 9, 30).unwrap()));
        // February 2026 has 28 days.
        assert!(!in_last_week(NaiveDate::from_ymd_opt(2026, 2, 20).unwrap()));
        assert!(in_last_week(NaiveDate::from_ymd_opt(2026, 2, 22).unwrap()));
        // December rolls into the next year.
        assert!(in_last_week(NaiveDate::from_ymd_opt(2026, 12, 30).unwrap()));
    }

    #[test]
    fn presets_offers_the_monthly_ordinal_worked_out_from_the_date() {
        // 23 September 2026 is the fourth Wednesday, seven days from the
        // month's end, so no "last" choice.
        let choices = presets(wednesday());
        assert!(choices.contains(&Repeat::MonthlyByDay(4, Weekday::Wed)));
        assert!(!choices.iter().any(|r| matches!(r, Repeat::MonthlyByDay(-1, _))));
    }

    #[test]
    fn presets_offers_the_last_weekday_choice_only_in_the_last_week() {
        let last_wednesday = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        let choices = presets(last_wednesday);
        assert!(choices.contains(&Repeat::MonthlyByDay(5, Weekday::Wed)));
        assert!(choices.contains(&Repeat::MonthlyByDay(-1, Weekday::Wed)));
    }

    #[test]
    fn later_weekdays_shifts_a_monthly_ordinal_rule_s_own_weekday() {
        assert_eq!(
            later_weekdays("RRULE:FREQ=MONTHLY;BYDAY=2TU", 1),
            "RRULE:FREQ=MONTHLY;BYDAY=2WE"
        );
        assert_eq!(
            later_weekdays("RRULE:FREQ=MONTHLY;BYDAY=-1FR", -1),
            "RRULE:FREQ=MONTHLY;BYDAY=-1TH"
        );
    }

    #[test]
    fn names_weekdays_is_true_for_a_monthly_ordinal_rule() {
        let rules = vec!["RRULE:FREQ=MONTHLY;BYDAY=2TU".to_string()];
        let second_tuesday = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert!(names_weekdays(&rules, second_tuesday, Lisbon));
    }

    #[test]
    fn a_plain_choice_carries_to_a_new_date_unchanged() {
        let new_day = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        for repeat in [
            Repeat::Never,
            Repeat::EveryDay,
            Repeat::EveryWeekday,
            Repeat::EveryWeek,
            Repeat::EveryMonth,
            Repeat::EveryYear,
        ] {
            assert_eq!(repeat.carried(new_day), repeat);
        }
    }

    #[test]
    fn a_monthly_ordinal_choice_follows_the_new_date_s_own_position() {
        // 23 September 2026, the fourth Wednesday, moved to 5 October
        // 2026, the first Monday of its own month.
        let choice = Repeat::MonthlyByDay(4, Weekday::Wed);
        let new_day = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert_eq!(choice.carried(new_day), Repeat::MonthlyByDay(1, Weekday::Mon));
    }

    #[test]
    fn the_last_choice_stays_last_while_the_new_date_is_still_in_the_last_week() {
        // The last Wednesday of September moved to 30 October, the last
        // Friday of October: still in the last week, so "last" survives,
        // and the weekday follows the new date, not the old one.
        let choice = Repeat::MonthlyByDay(-1, Weekday::Wed);
        let new_day = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        assert_eq!(choice.carried(new_day), Repeat::MonthlyByDay(-1, Weekday::Fri));
    }

    #[test]
    fn the_last_choice_falls_back_to_the_plain_ordinal_off_the_last_week() {
        // The last Wednesday of September moved to 7 October, the first
        // Wednesday of October: "last" no longer applies to this date.
        let choice = Repeat::MonthlyByDay(-1, Weekday::Wed);
        let new_day = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        assert_eq!(choice.carried(new_day), Repeat::MonthlyByDay(1, Weekday::Wed));
    }

    #[test]
    fn custom_and_kept_choices_carry_to_a_new_date_unchanged() {
        let new_day = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let custom = custom(2, Frequency::Weekly, &[Weekday::Mon], Ends::Never);
        assert_eq!(custom.carried(new_day), custom);
        let kept = Repeat::Kept("RRULE:FREQ=MONTHLY;BYDAY=1MO,3MO".into());
        assert_eq!(kept.carried(new_day), kept);
    }
}
