//! Changing one occurrence of a repeating event, the ones from it on, or
//! all of them, the three ways Google offers. Each comes down to plain
//! writes of whole events, [`Step`]s, which the change queue sends in
//! order: a changed occurrence under its own id, a series cut short with
//! `UNTIL` and a new series from the split, or the series itself.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use super::{Event, Status, expand};
use crate::invitation::Answer;
use crate::EpochMillis;

/// Which occurrences of a series a change covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatScope {
    This,
    Following,
    All,
}

/// The occurrence a person picked: the start it has in the series, and
/// the start it shows, which differ once someone moved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picked {
    pub original_start: EpochMillis,
    pub start: EpochMillis,
}

/// One write to the copy and the queue. Held changes persist a `Vec<Step>`
/// as JSON (`mailrs_store::calendar::save_holding`), so a crash or a quit
/// before the Undo toast closes still queues it at the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    /// Store the event and send it whole.
    Save(Event),
    /// Store a cancelled occurrence, so the grid leaves a gap, and ask the
    /// provider to delete that occurrence.
    Cancel(Event),
    /// Take the event, and a series' changed occurrence, off the copy and
    /// the provider.
    Remove { calendar: String, id: String },
    /// Move the event, a series with its changed occurrences, from one of
    /// the account's calendars to another. The steps after it write the
    /// event on `to`.
    Move { from: String, to: String, id: String },
}

impl Step {
    /// The calendar and event id the step writes.
    pub fn key(&self) -> (String, String) {
        match self {
            Step::Save(e) | Step::Cancel(e) => (e.calendar.clone(), e.id.clone()),
            Step::Remove { calendar, id } | Step::Move { to: calendar, id, .. } => (calendar.clone(), id.clone()),
        }
    }
}

/// `steps`, written on `to` instead of `from`, after a move of the event
/// `id` there. Staying on `from` leaves them as they are. Google moves a
/// series whole, so `id` is the series' own id even when the person
/// opened one occurrence.
pub fn to_calendar(steps: Vec<Step>, from: &str, to: &str, id: &str) -> Vec<Step> {
    if from == to {
        return steps;
    }
    let there = |event: Event| {
        if event.calendar == from { Event { calendar: to.to_string(), ..event } } else { event }
    };
    let mut moved = vec![Step::Move { from: from.to_string(), to: to.to_string(), id: id.to_string() }];
    moved.extend(steps.into_iter().map(|step| match step {
        Step::Save(event) => Step::Save(there(event)),
        Step::Cancel(event) => Step::Cancel(there(event)),
        Step::Remove { calendar, id } if calendar == from => Step::Remove { calendar: to.to_string(), id },
        other => other,
    }));
    moved
}

/// A series, or a changed occurrence of one.
pub fn in_series(event: &Event) -> bool {
    !event.rules.is_empty() || event.series.is_some()
}

/// The answers the repeat question offers, empty for a one-off event. A
/// new rule cannot cover one occurrence, so "This event only" drops out
/// when the rule changed.
pub fn scopes(event: &Event, rule_changed: bool) -> Vec<RepeatScope> {
    match (in_series(event), rule_changed) {
        (false, _) => Vec::new(),
        (true, true) => vec![RepeatScope::Following, RepeatScope::All],
        (true, false) => vec![RepeatScope::This, RepeatScope::Following, RepeatScope::All],
    }
}

/// What a guest's Yes, Maybe or No may cover, as Google Calendar asks it:
/// "This event" or "All events" for an occurrence of a series, and
/// nothing to ask for a one-off event. "This and following" would start a
/// series of the guest's own, so it is never offered.
pub fn answer_scopes(event: &Event) -> Vec<RepeatScope> {
    match in_series(event) {
        true => vec![RepeatScope::This, RepeatScope::All],
        false => Vec::new(),
    }
}

/// The row a guest's answer writes to the copy. `This` answers the picked
/// occurrence alone, as a changed occurrence of its own, which is how
/// Google keeps it; any other scope answers the series.
pub fn answered(series: &Event, changed: &[Event], picked: Picked, scope: RepeatScope, answer: Answer) -> Event {
    let mut row = match scope {
        RepeatScope::This => one(series, changed, picked, shown(series, changed, picked)),
        RepeatScope::Following | RepeatScope::All => series.clone(),
    };
    row.my_answer = Some(answer);
    for guest in row.guests.iter_mut().filter(|g| g.me) {
        guest.answer = Some(answer);
    }
    row
}

/// The writes that make `edited` true of the picked occurrence and the
/// ones `scope` adds. `changed` are the series' own changed occurrences;
/// `new_id` names the second half when the series splits.
pub fn change(
    series: &Event,
    changed: &[Event],
    picked: Picked,
    edited: Event,
    scope: RepeatScope,
    new_id: &str,
) -> Vec<Step> {
    if series.limited() {
        return vec![Step::Save(own_fields(series, changed, picked, &edited, scope))];
    }
    match scope {
        RepeatScope::This => vec![Step::Save(one(series, changed, picked, edited))],
        RepeatScope::Following if picked.original_start > series.start => {
            split(series, changed, picked, edited, new_id)
        }
        RepeatScope::Following | RepeatScope::All => {
            vec![Step::Save(whole(series, picked, edited))]
        }
    }
}

/// The writes that delete the picked occurrence and the ones `scope` adds.
pub fn delete(series: &Event, changed: &[Event], picked: Picked, scope: RepeatScope) -> Vec<Step> {
    match scope {
        RepeatScope::This => {
            let mut gone = one(series, changed, picked, shown(series, changed, picked));
            gone.status = Status::Cancelled;
            vec![Step::Cancel(gone)]
        }
        RepeatScope::Following if picked.original_start > series.start => {
            let mut steps = vec![Step::Save(Event {
                rules: before(series, picked.original_start),
                ..series.clone()
            })];
            steps.extend(later(changed, picked.original_start));
            steps
        }
        RepeatScope::Following | RepeatScope::All => vec![Step::Remove {
            calendar: series.calendar.clone(),
            id: series.id.clone(),
        }],
    }
}

/// A guest's change: their reminders, colour and busy on the picked
/// occurrence or on the series, every other field as Google holds it. The
/// editor hands a guest the event it opened, which for an occurrence
/// nobody changed is the series with the first occurrence's times, so
/// nothing else is taken from `edited`. A guest is never offered "This
/// and following", which would start a series of their own; it covers
/// the series here, as All does.
fn own_fields(
    series: &Event,
    changed: &[Event],
    picked: Picked,
    edited: &Event,
    scope: RepeatScope,
) -> Event {
    let target = match scope {
        RepeatScope::This => one(series, changed, picked, shown(series, changed, picked)),
        RepeatScope::Following | RepeatScope::All => series.clone(),
    };
    Event {
        reminders: edited.reminders.clone(),
        color: edited.color.clone(),
        busy: edited.busy,
        ..target
    }
}

/// The picked occurrence as it shows: its changed occurrence, or the
/// series at the picked time.
fn shown(series: &Event, changed: &[Event], picked: Picked) -> Event {
    held(changed, picked.original_start)
        .cloned()
        .unwrap_or_else(|| Event {
            start: picked.start,
            end: picked.start + (series.end - series.start),
            ..series.clone()
        })
}

fn held(changed: &[Event], original_start: EpochMillis) -> Option<&Event> {
    changed
        .iter()
        .find(|e| e.original_start == Some(original_start))
}

/// The picked occurrence as a changed occurrence of its own.
fn one(series: &Event, changed: &[Event], picked: Picked, edited: Event) -> Event {
    let held = held(changed, picked.original_start);
    Event {
        calendar: series.calendar.clone(),
        id: held.map_or_else(
            || super::occurrence_id(series, picked.original_start),
            |e| e.id.clone(),
        ),
        uid: series.uid.clone(),
        etag: held.map(|e| e.etag.clone()).unwrap_or_default(),
        rules: Vec::new(),
        series: Some(series.id.clone()),
        original_start: Some(picked.original_start),
        ..edited
    }
}

/// The series with the edit, moved by as much as the picked occurrence
/// moved, and its excluded and added dates moved the same way. An
/// `EXDATE` or `RDATE` names a wall time, so a series moved by an hour or
/// a day must carry its skipped and added occurrences the same distance,
/// or the old one comes back on the new schedule.
fn whole(series: &Event, picked: Picked, edited: Event) -> Event {
    let length = edited.end - edited.start;
    let delta = edited.start - picked.start;
    let start = series.start + delta;
    let same_rule = edited.rules == series.rules;
    Event {
        calendar: series.calendar.clone(),
        id: series.id.clone(),
        uid: series.uid.clone(),
        etag: series.etag.clone(),
        start,
        end: start + length,
        series: None,
        original_start: None,
        rules: shift_dates(
            &if same_rule {
                follow_weekday(&edited.rules, series, picked.start, edited.start)
            } else {
                edited.rules.clone()
            },
            series,
            delta,
        ),
        ..edited
    }
}

fn split(
    series: &Event,
    changed: &[Event],
    picked: Picked,
    edited: Event,
    new_id: &str,
) -> Vec<Step> {
    let cut = picked.original_start;
    let old = Event {
        rules: before(series, cut),
        ..series.clone()
    };
    let rules = if edited.rules == series.rules {
        follow_weekday(&from(series, cut), series, picked.start, edited.start)
    } else {
        edited.rules.clone()
    };
    let new = Event {
        id: new_id.to_string(),
        uid: String::new(),
        etag: String::new(),
        series: None,
        original_start: None,
        rules,
        ..edited
    };
    let mut steps = vec![Step::Save(old), Step::Save(new)];
    steps.extend(later(changed, cut));
    steps
}

/// `rules` moved to the weekday `to` falls on, for a weekly series
/// whose `BYDAY` names the day `from` falls on. The editor rebuilds a
/// plain weekly rule itself; one with an interval, a count or an end,
/// which every series a split ended carries, comes here unchanged and
/// would keep the old day. A move by whole weeks changes nothing.
fn follow_weekday(rules: &[String], series: &Event, from: EpochMillis, to: EpochMillis) -> Vec<String> {
    let zone: Tz = if series.all_day {
        chrono_tz::UTC
    } else {
        series.zone.parse().unwrap_or(chrono_tz::UTC)
    };
    let day = |at: EpochMillis| {
        DateTime::<Utc>::from_timestamp_millis(at)
            .unwrap_or_default()
            .with_timezone(&zone)
            .date_naive()
    };
    let days = (day(to) - day(from)).num_days().rem_euclid(7);
    if days == 0 || !super::repeat::names_weekdays(rules, day(series.start), zone) {
        return rules.to_vec();
    }
    rules
        .iter()
        .map(|line| {
            if super::is_rule_line(line) {
                super::repeat::later_weekdays(line, days)
            } else {
                line.clone()
            }
        })
        .collect()
}

/// Removals for the changed occurrences at or after `cut`, which would
/// otherwise stand beside the new series' own.
fn later(changed: &[Event], cut: EpochMillis) -> impl Iterator<Item = Step> + '_ {
    changed
        .iter()
        .filter(move |e| e.original_start.is_some_and(|s| s >= cut))
        .map(|e| Step::Remove {
            calendar: e.calendar.clone(),
            id: e.id.clone(),
        })
}

/// The series' rules ending before `cut`, with the dates it lists before
/// the cut.
fn before(series: &Event, cut: EpochMillis) -> Vec<String> {
    let end = if series.all_day {
        let day = DateTime::<Utc>::from_timestamp_millis(cut - 86_400_000).unwrap_or_default();
        format!("UNTIL={}", day.format("%Y%m%d"))
    } else {
        let last = DateTime::<Utc>::from_timestamp_millis(cut - 1000).unwrap_or_default();
        format!("UNTIL={}", last.format("%Y%m%dT%H%M%SZ"))
    };
    series
        .rules
        .iter()
        .filter_map(|line| {
            if super::is_rule_line(line) {
                Some(with_end(line, Some(end.as_str())))
            } else if super::is_date_line(line) {
                keep_dates(line, series, |at| at.is_none_or(|at| at < cut))
            } else {
                Some(line.clone())
            }
        })
        .collect()
}

/// The series' rules from `cut` on: what is left of a `COUNT`, and the
/// dates it lists from the cut.
fn from(series: &Event, cut: EpochMillis) -> Vec<String> {
    let used = || {
        let bare = Event {
            rules: series
                .rules
                .iter()
                .filter(|l| super::is_rule_line(l))
                .cloned()
                .collect(),
            ..series.clone()
        };
        expand(&bare, series.start, cut).len() as u32
    };
    series
        .rules
        .iter()
        .filter_map(|line| {
            if super::is_rule_line(line) {
                Some(match count(line) {
                    Some(total) => {
                        let used = used();
                        if used >= u32::from(super::MOST_OCCURRENCES) {
                            // expand stopped at its cap, so `used` may fall
                            // short of what the series actually spent: keep
                            // the rule open-ended on the new half rather
                            // than hand it a remainder that runs long.
                            tracing::warn!(rule = %line, "a series past the expansion cap kept its count on the split");
                            line.clone()
                        } else {
                            with_end(line, Some(format!("COUNT={}", total.saturating_sub(used)).as_str()))
                        }
                    }
                    None => line.clone(),
                })
            } else if super::is_date_line(line) {
                keep_dates(line, series, |at| at.is_some_and(|at| at >= cut))
            } else {
                Some(line.clone())
            }
        })
        .collect()
}

fn count(rule: &str) -> Option<u32> {
    rule.split([':', ';']).find_map(|part| {
        part.to_ascii_uppercase()
            .strip_prefix("COUNT=")?
            .parse()
            .ok()
    })
}

/// `rule` with its `COUNT` and `UNTIL` replaced by `end`.
fn with_end(rule: &str, end: Option<&str>) -> String {
    let body = rule.split_once(':').map_or(rule, |(_, body)| body);
    let mut parts: Vec<&str> = body
        .split(';')
        .filter(|p| {
            let p = p.to_ascii_uppercase();
            !p.starts_with("COUNT=") && !p.starts_with("UNTIL=")
        })
        .collect();
    parts.extend(end);
    format!("RRULE:{}", parts.join(";"))
}

/// An `EXDATE` or `RDATE` line with only the dates `keep` accepts, or
/// `None` when none is left. `keep` sees `None` for a value that does not
/// parse.
fn keep_dates(
    line: &str,
    series: &Event,
    keep: impl Fn(Option<EpochMillis>) -> bool,
) -> Option<String> {
    let (head, values) = line.split_once(':')?;
    let zone = date_zone(head, series);
    let kept: Vec<&str> = values
        .split(',')
        .filter(|v| keep(date_value(v, zone)))
        .collect();
    (!kept.is_empty()).then(|| format!("{head}:{}", kept.join(",")))
}

/// The event's excluded and added dates, moved by `delta`.
fn shift_dates(rules: &[String], series: &Event, delta: EpochMillis) -> Vec<String> {
    if delta == 0 {
        return rules.to_vec();
    }
    rules
        .iter()
        .map(|line| {
            if super::is_date_line(line) {
                shift_date_line(line, series, delta).unwrap_or_else(|| line.clone())
            } else {
                line.clone()
            }
        })
        .collect()
}

fn shift_date_line(line: &str, series: &Event, delta: EpochMillis) -> Option<String> {
    let (head, values) = line.split_once(':')?;
    let zone = date_zone(head, series);
    let shifted: Vec<String> = values
        .split(',')
        .map(|v| shift_date_value(v, zone, delta))
        .collect();
    Some(format!("{head}:{}", shifted.join(",")))
}

fn date_zone(head: &str, series: &Event) -> Tz {
    head.split(';')
        .find_map(|p| p.strip_prefix("TZID="))
        .unwrap_or(&series.zone)
        .parse()
        .unwrap_or(chrono_tz::UTC)
}

fn date_value(value: &str, zone: Tz) -> Option<EpochMillis> {
    if value.len() == 8 {
        let day = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some(day.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    if let Some(utc) = value.strip_suffix('Z') {
        return Some(
            NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
                .ok()?
                .and_utc()
                .timestamp_millis(),
        );
    }
    let local = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    zone.from_local_datetime(&local)
        .earliest()
        .map(|at| at.timestamp_millis())
}

/// `value`, moved by `delta`, in the form it was written: a bare date, a
/// UTC stamp, or a local one under `zone`. A value that does not parse is
/// left as it was, so a rule this module cannot read is not corrupted.
fn shift_date_value(value: &str, zone: Tz, delta: EpochMillis) -> String {
    let Some(at) = date_value(value, zone) else {
        return value.to_string();
    };
    let Some(shifted) = DateTime::<Utc>::from_timestamp_millis(at + delta) else {
        return value.to_string();
    };
    if value.len() == 8 {
        return shifted.format("%Y%m%d").to_string();
    }
    if value.ends_with('Z') {
        return format!("{}Z", shifted.format("%Y%m%dT%H%M%S"));
    }
    shifted
        .with_timezone(&zone)
        .format("%Y%m%dT%H%M%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::{Status, occurrence_id, split_occurrence_id};
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Europe::Lisbon;

    fn lisbon(m: u32, d: u32, h: u32, min: u32) -> EpochMillis {
        Lisbon
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, m, d)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .single()
            .unwrap()
            .timestamp_millis()
    }

    /// A daily stand-up from Monday 21 September 2026, 09:00 in Lisbon.
    fn standup(rules: &[&str]) -> Event {
        Event {
            calendar: "work".into(),
            id: "standup".into(),
            uid: "standup@google.com".into(),
            etag: "\"7\"".into(),
            start: lisbon(9, 21, 9, 0),
            end: lisbon(9, 21, 9, 15),
            zone: "Europe/Lisbon".into(),
            title: "Stand-up".into(),
            busy: true,
            rules: rules.iter().map(|r| r.to_string()).collect(),
            ..Event::default()
        }
    }

    /// Thursday's occurrence, as shown.
    fn thursday() -> Picked {
        Picked {
            original_start: lisbon(9, 24, 9, 0),
            start: lisbon(9, 24, 9, 0),
        }
    }

    fn moved(series: &Event, picked: Picked, hours: i64) -> Event {
        let shift = hours * 3_600_000;
        Event {
            start: picked.start + shift,
            end: picked.start + shift + (series.end - series.start),
            ..series.clone()
        }
    }

    fn saved(steps: &[Step]) -> Vec<&Event> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Save(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_timed_occurrence_is_named_by_its_start_in_utc() {
        let series = standup(&["RRULE:FREQ=DAILY"]);
        // 09:00 in Lisbon in September is 08:00 UTC.
        assert_eq!(
            occurrence_id(&series, lisbon(9, 23, 9, 0)),
            "standup_20260923T080000Z"
        );
        assert_eq!(
            split_occurrence_id("standup_20260923T080000Z"),
            Some(("standup", lisbon(9, 23, 9, 0)))
        );
    }

    #[test]
    fn an_all_day_occurrence_is_named_by_its_date() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 23)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let series = Event {
            all_day: true,
            ..standup(&["RRULE:FREQ=WEEKLY"])
        };
        assert_eq!(occurrence_id(&series, day), "standup_20260923");
        assert_eq!(
            split_occurrence_id("standup_20260923"),
            Some(("standup", day))
        );
        assert_eq!(split_occurrence_id("pm0123abcd"), None);
    }

    #[test]
    fn only_an_occurrence_of_a_series_asks_the_question() {
        assert!(scopes(&standup(&[]), false).is_empty());
        assert_eq!(
            scopes(&standup(&["RRULE:FREQ=DAILY"]), false),
            vec![RepeatScope::This, RepeatScope::Following, RepeatScope::All]
        );
        let changed = Event {
            series: Some("standup".into()),
            ..standup(&[])
        };
        assert_eq!(scopes(&changed, false).len(), 3);
        // A new repeat rule cannot apply to one occurrence alone.
        assert_eq!(
            scopes(&standup(&["RRULE:FREQ=DAILY"]), true),
            vec![RepeatScope::Following, RepeatScope::All]
        );
    }

    fn invited(rules: &[&str]) -> Event {
        Event {
            guests: vec![
                super::super::Guest { email: "priya@example.com".into(), organizer: true, ..Default::default() },
                super::super::Guest { email: "me@example.com".into(), me: true, ..Default::default() },
            ],
            ..standup(rules)
        }
    }

    #[test]
    fn an_answer_to_a_series_offers_this_event_or_all_events() {
        assert!(answer_scopes(&invited(&[])).is_empty());
        assert_eq!(
            answer_scopes(&invited(&["RRULE:FREQ=DAILY"])),
            vec![RepeatScope::This, RepeatScope::All]
        );
        let changed = Event { series: Some("standup".into()), ..invited(&[]) };
        assert_eq!(answer_scopes(&changed), vec![RepeatScope::This, RepeatScope::All]);
    }

    #[test]
    fn answering_this_event_writes_the_occurrence_as_a_change_of_its_own() {
        let series = invited(&["RRULE:FREQ=DAILY"]);
        let one = answered(&series, &[], thursday(), RepeatScope::This, Answer::No);
        assert_eq!(
            (one.id.as_str(), one.series.as_deref(), one.original_start, one.start, one.etag.as_str()),
            ("standup_20260924T080000Z", Some("standup"), Some(thursday().original_start), thursday().start, "")
        );
        assert!(one.rules.is_empty());
        assert_eq!(one.my_answer, Some(Answer::No));
        let me = one.guests.iter().find(|g| g.me).unwrap();
        assert_eq!(me.answer, Some(Answer::No));
        let priya = one.guests.iter().find(|g| g.organizer).unwrap();
        assert_eq!(priya.answer, None);
    }

    #[test]
    fn answering_a_changed_occurrence_keeps_its_id_and_version() {
        let series = invited(&["RRULE:FREQ=DAILY"]);
        let changed = Event {
            id: "standup_20260924T080000Z".into(),
            etag: "\"9\"".into(),
            series: Some("standup".into()),
            original_start: Some(thursday().original_start),
            start: thursday().start + 3_600_000,
            end: thursday().start + 4_500_000,
            rules: Vec::new(),
            ..series.clone()
        };
        let one = answered(&series, std::slice::from_ref(&changed), thursday(), RepeatScope::This, Answer::Maybe);
        assert_eq!((one.etag.as_str(), one.start), ("\"9\"", changed.start));
        assert_eq!(one.my_answer, Some(Answer::Maybe));
    }

    #[test]
    fn answering_all_events_answers_the_series() {
        let series = invited(&["RRULE:FREQ=DAILY"]);
        let whole = answered(&series, &[], thursday(), RepeatScope::All, Answer::Yes);
        assert_eq!((whole.id.as_str(), whole.series.as_deref(), whole.start), ("standup", None, series.start));
        assert_eq!(whole.my_answer, Some(Answer::Yes));
    }

    #[test]
    fn this_event_only_writes_a_changed_occurrence() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let steps = change(
            &series,
            &[],
            thursday(),
            moved(&series, thursday(), 1),
            RepeatScope::This,
            "pmnew",
        );
        let [Step::Save(one)] = steps.as_slice() else {
            panic!("{steps:?}")
        };
        assert_eq!(one.id, "standup_20260924T080000Z");
        assert_eq!(one.series.as_deref(), Some("standup"));
        assert_eq!(one.original_start, Some(lisbon(9, 24, 9, 0)));
        assert_eq!(one.start, lisbon(9, 24, 10, 0));
        assert!(one.rules.is_empty());
        assert_eq!(one.uid, series.uid);
        assert_eq!(one.etag, "", "Google has never seen this occurrence");
    }

    #[test]
    fn this_event_only_on_a_changed_occurrence_edits_it_in_place() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let held = Event {
            id: "standup_20260924T080000Z".into(),
            etag: "\"2\"".into(),
            series: Some("standup".into()),
            original_start: Some(lisbon(9, 24, 9, 0)),
            start: lisbon(9, 24, 11, 0),
            end: lisbon(9, 24, 11, 15),
            rules: Vec::new(),
            ..series.clone()
        };
        let picked = Picked {
            original_start: lisbon(9, 24, 9, 0),
            start: lisbon(9, 24, 11, 0),
        };
        let edited = Event {
            title: "Moved stand-up".into(),
            ..held.clone()
        };
        let steps = change(&series, &[held], picked, edited, RepeatScope::This, "pmnew");
        let [Step::Save(one)] = steps.as_slice() else {
            panic!("{steps:?}")
        };
        assert_eq!(
            (one.id.as_str(), one.etag.as_str()),
            ("standup_20260924T080000Z", "\"2\"")
        );
        assert_eq!(one.title, "Moved stand-up");
    }

    #[test]
    fn all_events_shifts_the_series_by_the_move() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let steps = change(
            &series,
            &[],
            thursday(),
            moved(&series, thursday(), 1),
            RepeatScope::All,
            "pmnew",
        );
        let [Step::Save(all)] = steps.as_slice() else {
            panic!("{steps:?}")
        };
        assert_eq!((all.id.as_str(), all.etag.as_str()), ("standup", "\"7\""));
        assert_eq!(all.start, lisbon(9, 21, 10, 0));
        assert_eq!(all.end, lisbon(9, 21, 10, 15));
        assert_eq!(all.rules, series.rules);
    }

    #[test]
    fn moving_all_events_moves_their_excluded_dates() {
        let series = standup(&[
            "RRULE:FREQ=DAILY;COUNT=10",
            "EXDATE;TZID=Europe/Lisbon:20260922T090000",
        ]);
        let steps = change(
            &series,
            &[],
            thursday(),
            moved(&series, thursday(), 1),
            RepeatScope::All,
            "pmnew",
        );
        let [Step::Save(all)] = steps.as_slice() else {
            panic!("{steps:?}")
        };
        // The exclusion follows the hour the series moved to, or the
        // excluded stand-up would come back on the new schedule.
        assert_eq!(all.rules[1], "EXDATE;TZID=Europe/Lisbon:20260922T100000");
    }

    #[test]
    fn following_splits_a_counted_series() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let edited = Event {
            title: "Longer stand-up".into(),
            ..moved(&series, thursday(), 0)
        };
        let steps = change(
            &series,
            &[],
            thursday(),
            edited,
            RepeatScope::Following,
            "pmnew",
        );
        let saves = saved(&steps);
        assert_eq!(saves.len(), 2);
        let (old, new) = (saves[0], saves[1]);
        assert_eq!(old.id, "standup");
        // One second before Thursday's 08:00 UTC; COUNT goes, since a rule
        // cannot hold both.
        assert_eq!(
            old.rules,
            vec!["RRULE:FREQ=DAILY;UNTIL=20260924T075959Z".to_string()]
        );
        assert_eq!(old.title, "Stand-up");
        assert_eq!(new.id, "pmnew");
        assert_eq!((new.uid.as_str(), new.etag.as_str()), ("", ""));
        assert_eq!(new.start, lisbon(9, 24, 9, 0));
        // Monday to Wednesday used three of the ten.
        assert_eq!(new.rules, vec!["RRULE:FREQ=DAILY;COUNT=7".to_string()]);
        assert_eq!(new.title, "Longer stand-up");
        assert_eq!(new.series, None);
    }

    #[test]
    fn excluded_dates_follow_their_half() {
        let series = standup(&[
            "RRULE:FREQ=DAILY;COUNT=10",
            "EXDATE;TZID=Europe/Lisbon:20260922T090000,20260925T090000",
        ]);
        let steps = change(
            &series,
            &[],
            thursday(),
            moved(&series, thursday(), 0),
            RepeatScope::Following,
            "pmnew",
        );
        let saves = saved(&steps);
        assert_eq!(
            saves[0].rules[1],
            "EXDATE;TZID=Europe/Lisbon:20260922T090000"
        );
        assert_eq!(
            saves[1].rules[1],
            "EXDATE;TZID=Europe/Lisbon:20260925T090000"
        );
        // COUNT counts excluded dates too, as RFC 5545 has it.
        assert_eq!(saves[1].rules[0], "RRULE:FREQ=DAILY;COUNT=7");
    }

    #[test]
    fn following_removes_changed_occurrences_after_the_split() {
        let series = standup(&["RRULE:FREQ=DAILY"]);
        let before = Event {
            id: "standup_20260922T080000Z".into(),
            series: Some("standup".into()),
            original_start: Some(lisbon(9, 22, 9, 0)),
            rules: Vec::new(),
            ..series.clone()
        };
        let after = Event {
            id: "standup_20260925T080000Z".into(),
            original_start: Some(lisbon(9, 25, 9, 0)),
            ..before.clone()
        };
        let steps = change(
            &series,
            &[before, after],
            thursday(),
            moved(&series, thursday(), 0),
            RepeatScope::Following,
            "pmnew",
        );
        assert_eq!(
            steps.last(),
            Some(&Step::Remove {
                calendar: "work".into(),
                id: "standup_20260925T080000Z".into()
            })
        );
        assert_eq!(steps.len(), 3);
        // A series without an end keeps its rule on the new half.
        assert_eq!(saved(&steps)[1].rules, vec!["RRULE:FREQ=DAILY".to_string()]);
    }

    #[test]
    fn following_from_the_first_occurrence_changes_the_whole_series() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let first = Picked {
            original_start: series.start,
            start: series.start,
        };
        let steps = change(
            &series,
            &[],
            first,
            moved(&series, first, 1),
            RepeatScope::Following,
            "pmnew",
        );
        let [Step::Save(all)] = steps.as_slice() else {
            panic!("{steps:?}")
        };
        assert_eq!(all.id, "standup");
    }

    #[test]
    fn an_all_day_series_ends_the_day_before() {
        let midnight = |d| {
            NaiveDate::from_ymd_opt(2026, 9, d)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        };
        let series = Event {
            all_day: true,
            zone: "UTC".into(),
            start: midnight(21),
            end: midnight(22),
            ..standup(&["RRULE:FREQ=DAILY"])
        };
        let picked = Picked {
            original_start: midnight(24),
            start: midnight(24),
        };
        let steps = change(
            &series,
            &[],
            picked,
            Event {
                start: midnight(24),
                end: midnight(25),
                ..series.clone()
            },
            RepeatScope::Following,
            "pmnew",
        );
        assert_eq!(
            saved(&steps)[0].rules,
            vec!["RRULE:FREQ=DAILY;UNTIL=20260923".to_string()]
        );
    }

    #[test]
    fn deleting_one_occurrence_cancels_it() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let steps = delete(&series, &[], thursday(), RepeatScope::This);
        let [Step::Cancel(gone)] = steps.as_slice() else {
            panic!("{steps:?}")
        };
        assert_eq!(gone.id, "standup_20260924T080000Z");
        assert_eq!(gone.status, Status::Cancelled);
        assert_eq!(gone.original_start, Some(lisbon(9, 24, 9, 0)));
    }

    #[test]
    fn deleting_following_ends_the_series_and_all_removes_it() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let steps = delete(&series, &[], thursday(), RepeatScope::Following);
        assert_eq!(
            saved(&steps)[0].rules,
            vec!["RRULE:FREQ=DAILY;UNTIL=20260924T075959Z".to_string()]
        );
        assert_eq!(
            delete(&series, &[], thursday(), RepeatScope::All),
            vec![Step::Remove {
                calendar: "work".into(),
                id: "standup".into()
            }]
        );
    }

    /// A weekly series from Monday 21 September, with its fourth
    /// occurrence, Monday 12 October, dragged to Tuesday 13.
    fn mondays_moved_to_tuesday(rule: &str) -> (Event, Picked, Event) {
        let series = standup(&[rule]);
        let picked = Picked {
            original_start: lisbon(10, 12, 9, 0),
            start: lisbon(10, 12, 9, 0),
        };
        let edited = moved(&series, picked, 24);
        (series, picked, edited)
    }

    #[test]
    fn all_events_moves_a_counted_weekly_rule_to_the_new_weekday() {
        let (series, picked, edited) =
            mondays_moved_to_tuesday("RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=10");
        let steps = change(&series, &[], picked, edited, RepeatScope::All, "new");
        let saved = saved(&steps);
        assert_eq!(saved[0].start, lisbon(9, 22, 9, 0));
        assert_eq!(
            saved[0].rules,
            vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=10".to_string()]
        );
    }

    #[test]
    fn following_moves_a_counted_weekly_rule_to_the_new_weekday() {
        let (series, picked, edited) =
            mondays_moved_to_tuesday("RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=10");
        let steps = change(&series, &[], picked, edited, RepeatScope::Following, "new");
        let saved = saved(&steps);
        assert_eq!(
            saved[0].rules,
            vec!["RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20261012T075959Z".to_string()]
        );
        // Three Mondays went before the cut, so seven are left.
        assert_eq!(
            saved[1].rules,
            vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=7".to_string()]
        );
    }

    #[test]
    fn all_events_moves_a_series_an_earlier_split_ended() {
        let (series, picked, edited) =
            mondays_moved_to_tuesday("RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20261102T075959Z");
        let steps = change(&series, &[], picked, edited, RepeatScope::All, "new");
        assert_eq!(
            saved(&steps)[0].rules,
            vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL=20261102T075959Z".to_string()]
        );
    }

    #[test]
    fn all_events_moves_every_listed_day_of_a_weekly_rule_with_an_interval() {
        let (series, picked, edited) =
            mondays_moved_to_tuesday("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,SU");
        let steps = change(&series, &[], picked, edited, RepeatScope::All, "new");
        assert_eq!(
            saved(&steps)[0].rules,
            vec!["RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,MO".to_string()]
        );
    }

    /// The standup starts Monday 21 September 2026, the third Monday of
    /// its month, on `RRULE:FREQ=MONTHLY;BYDAY=3MO`. All events, dragged
    /// one day later, must keep the ordinal and follow the weekday.
    #[test]
    fn all_events_moves_a_monthly_ordinal_rule_to_the_new_weekday() {
        let series = standup(&["RRULE:FREQ=MONTHLY;BYDAY=3MO"]);
        let picked = Picked {
            original_start: lisbon(9, 21, 9, 0),
            start: lisbon(9, 21, 9, 0),
        };
        let edited = moved(&series, picked, 24);
        let steps = change(&series, &[], picked, edited, RepeatScope::All, "new");
        assert_eq!(
            saved(&steps)[0].rules,
            vec!["RRULE:FREQ=MONTHLY;BYDAY=3TU".to_string()]
        );
    }

    #[test]
    fn a_weekly_rule_moved_by_a_whole_week_keeps_its_days() {
        let series = standup(&["RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=10"]);
        let picked = Picked {
            original_start: lisbon(10, 12, 9, 0),
            start: lisbon(10, 12, 9, 0),
        };
        let edited = moved(&series, picked, 7 * 24);
        let steps = change(&series, &[], picked, edited, RepeatScope::All, "new");
        assert_eq!(saved(&steps)[0].rules, series.rules);
    }

    /// A split hands a series an end, and the end must not change how its
    /// days move: moved a day under All events, each day set lands on the
    /// same days with an `UNTIL` as without one.
    #[test]
    fn an_end_does_not_change_how_a_series_days_move() {
        let until = ";UNTIL=20261231T235959Z";
        // Each day set, and the day of September 2026 its series starts on.
        for (days, first) in [("MO,TU,WE,TH,FR", 21), ("MO,WE,FR", 21), ("TU", 22), ("SA,SU", 26)] {
            let moved_rule = |end: &str| {
                let series = Event {
                    start: lisbon(9, first, 9, 0),
                    end: lisbon(9, first, 9, 15),
                    ..standup(&[&format!("RRULE:FREQ=WEEKLY;BYDAY={days}{end}")])
                };
                // The series' fourth week, on the day it starts on.
                let picked = Picked {
                    original_start: lisbon(10, first - 9, 9, 0),
                    start: lisbon(10, first - 9, 9, 0),
                };
                let steps = change(&series, &[], picked, moved(&series, picked, 24), RepeatScope::All, "new");
                with_end(&saved(&steps)[0].rules[0], None)
            };
            assert_eq!(moved_rule(until), moved_rule(""), "BYDAY={days}");
        }
    }

    /// Stand-up as Rita organizes it and this account attends, skipping
    /// Wednesday and adding a Saturday.
    fn attended() -> Event {
        use crate::calendar::Guest;
        Event {
            guests: vec![
                Guest { email: "rita@example.com".into(), organizer: true, ..Guest::default() },
                Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
            ],
            ..standup(&[
                "RRULE:FREQ=DAILY;COUNT=10",
                "EXDATE;TZID=Europe/Lisbon:20260923T090000",
                "RDATE;TZID=Europe/Lisbon:20261003T090000",
            ])
        }
    }

    /// What the limited editor hands over: the event it opened with, and
    /// only the guest's own fields changed.
    fn own_fields_changed(opened: &Event) -> Event {
        use crate::calendar::{Reminder, ReminderMethod};
        Event {
            reminders: Some(vec![Reminder { minutes: 30, method: ReminderMethod::Notification }]),
            color: Some("#f4511e".into()),
            busy: false,
            ..opened.clone()
        }
    }

    fn keeps_only_own_fields_changed(saved: &Event, before: &Event) {
        assert_eq!(saved.color.as_deref(), Some("#f4511e"));
        assert!(!saved.busy);
        assert_eq!(saved.reminders.as_ref().map(Vec::len), Some(1));
        let restored = Event {
            reminders: before.reminders.clone(),
            color: before.color.clone(),
            busy: before.busy,
            ..saved.clone()
        };
        assert_eq!(&restored, before, "only reminders, colour and busy change");
    }

    #[test]
    fn a_guest_changing_a_middle_occurrence_keeps_its_time_and_id() {
        let series = attended();
        // The editor opens Thursday on the series itself, whose start is
        // Monday's.
        let steps = change(&series, &[], thursday(), own_fields_changed(&series), RepeatScope::This, "new");
        let [Step::Save(saved)] = steps.as_slice() else { panic!("one save: {steps:?}") };
        let expected = Event {
            id: occurrence_id(&series, thursday().original_start),
            etag: String::new(),
            start: thursday().start,
            end: thursday().start + (series.end - series.start),
            rules: Vec::new(),
            series: Some("standup".into()),
            original_start: Some(thursday().original_start),
            ..series.clone()
        };
        keeps_only_own_fields_changed(saved, &expected);
    }

    #[test]
    fn a_guest_changing_all_events_keeps_the_series_times_and_dates() {
        let series = attended();
        let steps = change(&series, &[], thursday(), own_fields_changed(&series), RepeatScope::All, "new");
        let [Step::Save(saved)] = steps.as_slice() else { panic!("one save: {steps:?}") };
        keeps_only_own_fields_changed(saved, &series);
    }

    #[test]
    fn a_guest_changing_an_occurrence_someone_moved_keeps_it_where_it_went() {
        let series = attended();
        let moved = Event {
            id: occurrence_id(&series, thursday().original_start),
            etag: "\"3\"".into(),
            title: "Stand-up, late".into(),
            start: lisbon(9, 24, 11, 0),
            end: lisbon(9, 24, 11, 15),
            rules: Vec::new(),
            series: Some("standup".into()),
            original_start: Some(thursday().original_start),
            ..series.clone()
        };
        let picked = Picked { original_start: thursday().original_start, start: moved.start };
        let changed = [moved.clone()];
        let this = change(&series, &changed, picked, own_fields_changed(&moved), RepeatScope::This, "new");
        let [Step::Save(saved)] = this.as_slice() else { panic!("one save: {this:?}") };
        keeps_only_own_fields_changed(saved, &moved);
        // All events, opened on the moved occurrence, leaves the series'
        // rules and times alone.
        let all = change(&series, &changed, picked, own_fields_changed(&moved), RepeatScope::All, "new");
        let [Step::Save(saved)] = all.as_slice() else { panic!("one save: {all:?}") };
        keeps_only_own_fields_changed(saved, &series);
    }

    #[test]
    fn a_move_to_another_calendar_goes_first_and_the_edit_follows_it_there() {
        let series = standup(&["RRULE:FREQ=DAILY;COUNT=5"]);
        let edited = Event { title: "Stand-up, renamed".into(), ..series.clone() };
        let steps = change(&series, &[], thursday(), edited, RepeatScope::All, "new");
        let moved = to_calendar(steps, &series.calendar, "home", &series.id);
        let [Step::Move { from, to, id }, Step::Save(saved)] = moved.as_slice() else {
            panic!("a move, then the save: {moved:?}")
        };
        assert_eq!((from.as_str(), to.as_str(), id.as_str()), (series.calendar.as_str(), "home", "standup"));
        assert_eq!((saved.calendar.as_str(), saved.title.as_str()), ("home", "Stand-up, renamed"));
    }

    #[test]
    fn a_move_writes_where_the_event_lands() {
        let step = Step::Move { from: "work".into(), to: "home".into(), id: "standup".into() };
        assert_eq!(step.key(), ("home".to_string(), "standup".to_string()));
    }

    #[test]
    fn staying_on_the_same_calendar_moves_nothing() {
        let series = standup(&[]);
        let steps = vec![Step::Save(series.clone())];
        assert_eq!(to_calendar(steps.clone(), &series.calendar, &series.calendar, &series.id), steps);
    }
}
