//! Calendars and events as Penguin Mail keeps them, whatever the
//! provider. The Google adapter maps Google's JSON to these; CalDAV and
//! Microsoft Graph will map theirs. A repeating event is kept as its
//! rule, and [`expand`] turns it into occurrences for the range on
//! screen, in the event's own time zone, so a 09:00 meeting stays at
//! 09:00 when the clocks change.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rrule::RRuleSet;
use serde::{Deserialize, Serialize};

use crate::invitation;
use crate::{AccountId, EpochMillis};

/// Most occurrences one expansion returns. A daily series over a month
/// view is 42; this is far above any range the window asks for, and it
/// stops a rule with no end from running on.
const MOST_OCCURRENCES: u16 = 1000;

/// What the account may do with a calendar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Owner,
    Writer,
    #[default]
    Reader,
    /// Sees when the owner is busy, and nothing about what.
    FreeBusy,
}

impl Access {
    pub fn can_write(self) -> bool {
        matches!(self, Access::Owner | Access::Writer)
    }

    /// Google's word for it, which the store keeps too.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Owner => "owner",
            Access::Writer => "writer",
            Access::Reader => "reader",
            Access::FreeBusy => "freeBusyReader",
        }
    }

    pub fn parse(word: &str) -> Access {
        match word {
            "owner" => Access::Owner,
            "writer" => Access::Writer,
            "freeBusyReader" => Access::FreeBusy,
            _ => Access::Reader,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderMethod {
    /// A notification on this computer.
    Notification,
    /// An email the provider sends. Penguin Mail shows it and sends
    /// nothing itself.
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub minutes: u32,
    pub method: ReminderMethod,
}

/// One calendar on an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    /// The provider's id, such as Google's `primary`-calendar address.
    pub id: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
    pub access: Access,
    /// The IANA zone new events on this calendar are written in.
    pub zone: String,
    pub primary: bool,
    /// Whether the view shows it. Kept on this computer only.
    pub shown: bool,
    /// What an event on this calendar reminds of when it names none.
    pub reminders: Vec<Reminder>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guest {
    pub email: String,
    pub name: Option<String>,
    /// This guest's answer to the invitation. `None` until they answer.
    pub answer: Option<invitation::Answer>,
    pub organizer: bool,
    /// This guest is the account itself.
    pub me: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Confirmed,
    Tentative,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Confirmed => "confirmed",
            Status::Tentative => "tentative",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(word: &str) -> Status {
        match word {
            "tentative" => Status::Tentative,
            "cancelled" => Status::Cancelled,
            _ => Status::Confirmed,
        }
    }
}

/// One event, or one changed occurrence of a series.
///
/// An all-day event runs from midnight UTC of its first day to midnight
/// UTC of the day after its last, the way iCalendar ends one, and its
/// zone is `UTC`: a date means the same date wherever the reader is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub calendar: String,
    pub id: String,
    /// The iCalendar UID, shared with any invitation for the event.
    pub uid: String,
    /// The provider's version tag. A write sends it back so the provider
    /// can refuse a change made against an older version.
    pub etag: String,
    pub start: EpochMillis,
    pub end: EpochMillis,
    pub zone: String,
    pub all_day: bool,
    pub title: String,
    pub place: String,
    pub description: String,
    /// `#rrggbb` when the event has its own colour; `None` takes the
    /// calendar's.
    pub color: Option<String>,
    /// The event blocks time: Google's `transparency` is not
    /// `transparent`. Whether it clashes also depends on the status, the
    /// account's answer and `all_day`.
    pub busy: bool,
    pub status: Status,
    pub private: bool,
    pub organizer: Option<String>,
    pub guests: Vec<Guest>,
    /// The account's own answer, when it is a guest. `None` until it
    /// answers.
    pub my_answer: Option<invitation::Answer>,
    /// `None` takes the calendar's reminders.
    pub reminders: Option<Vec<Reminder>>,
    /// A video call link, such as Google Meet's.
    pub conference: Option<String>,
    /// The series' `RRULE`, `EXDATE` and `RDATE` lines, whole. Empty for
    /// an event that does not repeat.
    pub rules: Vec<String>,
    /// For a changed occurrence: the id of the series it belongs to.
    pub series: Option<String>,
    /// For a changed occurrence: the start of the occurrence it replaces.
    pub original_start: Option<EpochMillis>,
    /// A change made here waits in the queue for the provider.
    pub pending: bool,
}

impl Event {
    /// Whether the event takes the account's time, for free time and the
    /// clash line. `busy` alone is what Google holds and what a change
    /// sends back; a cancelled event, one the account declined and one
    /// lasting all day leave the time open as well.
    pub fn blocks_time(&self) -> bool {
        self.busy
            && !self.all_day
            && self.status != Status::Cancelled
            && self.my_answer != Some(invitation::Answer::No)
    }
}

/// One showing of an event on the grid. `event` is shared rather than
/// cloned, so a series with many occurrences in view costs one
/// allocation of it however many times it repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub account_id: AccountId,
    pub event: Arc<Event>,
    pub start: EpochMillis,
    pub end: EpochMillis,
}

impl Occurrence {
    /// The id the assistant's tools name this occurrence by. A one-off event,
    /// or an occurrence someone already changed, has a row and an id of
    /// its own. An occurrence a series expands to takes Google's instance
    /// form, the series id, an underscore and the original start in UTC
    /// (`_20261022T090000Z`, or `_20261022` for a whole day), so a change
    /// to it reaches that one occurrence and never the series.
    pub fn id(&self) -> String {
        if self.event.rules.is_empty() {
            return self.event.id.clone();
        }
        let Some(start) = DateTime::<Utc>::from_timestamp_millis(self.start) else {
            return self.event.id.clone();
        };
        let form = if self.event.all_day { "%Y%m%d" } else { "%Y%m%dT%H%M%SZ" };
        format!("{}_{}", self.event.id, start.format(form))
    }
}

/// Splits an occurrence id, as [`Occurrence::id`] writes one, into the
/// series id and the occurrence's original start, or `None` for any other
/// id.
pub fn split_occurrence_id(id: &str) -> Option<(&str, EpochMillis)> {
    let (series, stamp) = id.rsplit_once('_')?;
    if series.is_empty() {
        return None;
    }
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let at = match stamp.len() {
        8 if digits(stamp) => chrono::NaiveDate::parse_from_str(stamp, "%Y%m%d").ok()?.and_hms_opt(0, 0, 0)?,
        16 if stamp.ends_with('Z') && digits(&stamp[..8]) && digits(&stamp[9..15]) => {
            chrono::NaiveDateTime::parse_from_str(&stamp[..15], "%Y%m%dT%H%M%S").ok()?
        }
        _ => return None,
    };
    Some((series, at.and_utc().timestamp_millis()))
}

/// One page of changes to one calendar, provider-neutral so a CalDAV
/// adapter answers the same shape a Google one does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventPage {
    pub events: Vec<Event>,
    /// Ids of events deleted or cancelled since the token. A cancelled
    /// occurrence of a series is not here: it comes as an event with
    /// `series` set, since it says which occurrence to leave out.
    pub removed: Vec<String>,
    pub next_page: Option<String>,
    /// On the last page, the token for the next read.
    pub next_sync: Option<String>,
}

/// The starts and ends of `event`'s occurrences that overlap `from` to
/// `to`, earliest first. A one-off event gives itself or nothing. A rule
/// the `rrule` crate cannot read gives the first occurrence, so the
/// event stays visible, and the log names the rule.
pub fn expand(event: &Event, from: EpochMillis, to: EpochMillis) -> Vec<(EpochMillis, EpochMillis)> {
    let length = event.end - event.start;
    let overlaps = |start: EpochMillis| start < to && start + length > from;
    if event.rules.is_empty() {
        return if overlaps(event.start) || (length == 0 && event.start >= from && event.start < to) {
            vec![(event.start, event.end)]
        } else {
            Vec::new()
        };
    }
    let Some(set) = rule_set(event) else {
        return if overlaps(event.start) { vec![(event.start, event.end)] } else { Vec::new() };
    };
    let tz = zone(event);
    let (Some(after), Some(before)) = (at(from - length, tz), at(to, tz)) else {
        return Vec::new();
    };
    set.after(after)
        .before(before)
        .all(MOST_OCCURRENCES)
        .dates
        .into_iter()
        .map(|start| start.timestamp_millis())
        .filter(|start| overlaps(*start))
        .map(|start| (start, start + length))
        .collect()
}

/// When the last occurrence of a series ends, or `None` for a series with
/// no end. The store keeps it so a range query can skip series that
/// finished long ago. A rule nobody can read ends with its first
/// occurrence, since that is all [`expand`] shows of it.
pub fn series_end(event: &Event) -> Option<EpochMillis> {
    if event.rules.is_empty() {
        return Some(event.end);
    }
    let Some(set) = rule_set(event) else {
        return Some(event.end);
    };
    let rule = event.rules.iter().find(|line| line.to_ascii_uppercase().starts_with("RRULE"))?;
    let upper = rule.to_ascii_uppercase();
    if !upper.contains("COUNT=") && !upper.contains("UNTIL=") {
        return None;
    }
    let length = event.end - event.start;
    let result = set.all(MOST_OCCURRENCES);
    // A series longer than the cap is treated as endless rather than
    // cut short.
    if result.limited {
        return None;
    }
    result.dates.last().map(|start| start.timestamp_millis() + length)
}

fn zone(event: &Event) -> rrule::Tz {
    let tz: chrono_tz::Tz = event.zone.parse().unwrap_or(chrono_tz::UTC);
    rrule::Tz::Tz(tz)
}

fn at(instant: EpochMillis, tz: rrule::Tz) -> Option<DateTime<rrule::Tz>> {
    DateTime::<Utc>::from_timestamp_millis(instant).map(|at| at.with_timezone(&tz))
}

/// The event's rules with its start in front, as `rrule` reads a set.
fn rule_set(event: &Event) -> Option<RRuleSet> {
    let tz = zone(event);
    let start = at(event.start, tz)?;
    let name = match tz {
        rrule::Tz::Tz(tz) => tz.name().to_string(),
        rrule::Tz::Local(_) => "UTC".to_string(),
    };
    let rules: Vec<String> = event.rules.iter().map(|rule| dates_in_utc(&until_in_utc(rule))).collect();
    let text = format!(
        "DTSTART;TZID={name}:{}\n{}",
        start.format("%Y%m%dT%H%M%S"),
        rules.join("\n")
    );
    match text.parse::<RRuleSet>() {
        Ok(set) => Some(set),
        Err(err) => {
            tracing::warn!(rules = ?event.rules, %err, "could not read a repeat rule");
            None
        }
    }
}

/// Google writes an all-day series' `UNTIL` as a bare date
/// (`UNTIL=20261102`, RFC 5545's `DATE` form). `rrule` reads a value with
/// no `T` and no `Z` in the machine's own time zone rather than in the
/// `DTSTART`'s, so it never matches the `TZID=UTC` this module always
/// writes and the whole rule fails to parse. Widening it to UTC midnight
/// names the same day and keeps the rule readable.
fn until_in_utc(rule: &str) -> String {
    let Some(at) = rule.to_ascii_uppercase().find("UNTIL=") else {
        return rule.to_string();
    };
    let start = at + "UNTIL=".len();
    let end = rule[start..]
        .find(';')
        .map_or(rule.len(), |offset| start + offset);
    let value = &rule[start..end];
    if value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}{value}T000000Z{}", &rule[..start], &rule[end..])
    } else {
        rule.to_string()
    }
}

/// Google writes an all-day series' skipped and added days as bare dates
/// (`EXDATE;VALUE=DATE:20260713`). `rrule` ignores `VALUE=DATE` and reads
/// the bare value at midnight in the machine's own zone, so outside UTC it
/// names no occurrence of a series this module starts at UTC midnight.
/// Widening each bare date to UTC midnight, as [`until_in_utc`] does for
/// `UNTIL`, gives the same day in every zone.
fn dates_in_utc(line: &str) -> String {
    let upper = line.to_ascii_uppercase();
    if !upper.starts_with("EXDATE") && !upper.starts_with("RDATE") {
        return line.to_string();
    }
    let Some((head, values)) = line.split_once(':') else {
        return line.to_string();
    };
    let bare = |value: &str| value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit());
    if !values.split(',').any(bare) {
        return line.to_string();
    }
    let params: Vec<&str> = head.split(';').filter(|p| !p.eq_ignore_ascii_case("VALUE=DATE")).collect();
    let values: Vec<String> = values
        .split(',')
        .map(|value| if bare(value) { format!("{value}T000000Z") } else { value.to_string() })
        .collect();
    format!("{}:{}", params.join(";"), values.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Europe::Lisbon;

    fn lisbon(y: i32, m: u32, d: u32, h: u32, min: u32) -> EpochMillis {
        Lisbon
            .from_local_datetime(&NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn standup(rules: &[&str]) -> Event {
        Event {
            calendar: "work".into(),
            id: "standup".into(),
            start: lisbon(2026, 10, 19, 9, 0),
            end: lisbon(2026, 10, 19, 9, 15),
            zone: "Europe/Lisbon".into(),
            title: "Stand-up".into(),
            busy: true,
            rules: rules.iter().map(|r| r.to_string()).collect(),
            ..Event::default()
        }
    }

    #[test]
    fn a_one_off_event_shows_once_when_it_overlaps_the_range() {
        let event = standup(&[]);
        let day = (lisbon(2026, 10, 19, 0, 0), lisbon(2026, 10, 20, 0, 0));
        assert_eq!(expand(&event, day.0, day.1), vec![(event.start, event.end)]);
        assert!(expand(&event, lisbon(2026, 10, 20, 0, 0), lisbon(2026, 10, 21, 0, 0)).is_empty());
    }

    #[test]
    fn a_series_keeps_its_local_hour_across_the_clock_change() {
        // Portugal leaves summer time on 25 October 2026.
        let event = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let got = expand(&event, lisbon(2026, 10, 24, 0, 0), lisbon(2026, 10, 27, 0, 0));
        assert_eq!(
            got,
            vec![
                (lisbon(2026, 10, 24, 9, 0), lisbon(2026, 10, 24, 9, 15)),
                (lisbon(2026, 10, 25, 9, 0), lisbon(2026, 10, 25, 9, 15)),
                (lisbon(2026, 10, 26, 9, 0), lisbon(2026, 10, 26, 9, 15)),
            ]
        );
    }

    #[test]
    fn a_count_stops_the_series() {
        let event = standup(&["RRULE:FREQ=DAILY;COUNT=3"]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(got.len(), 3);
        assert_eq!(series_end(&event), Some(lisbon(2026, 10, 21, 9, 15)));
    }

    #[test]
    fn an_until_stops_the_series() {
        let event = standup(&["RRULE:FREQ=DAILY;UNTIL=20261021T235959Z"]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn a_series_with_no_end_has_no_series_end() {
        assert_eq!(series_end(&standup(&["RRULE:FREQ=WEEKLY"])), None);
    }

    #[test]
    fn an_excluded_date_leaves_a_gap() {
        let event = standup(&[
            "RRULE:FREQ=DAILY;COUNT=3",
            "EXDATE;TZID=Europe/Lisbon:20261020T090000",
        ]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(
            got.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![lisbon(2026, 10, 19, 9, 0), lisbon(2026, 10, 21, 9, 0)]
        );
    }

    #[test]
    fn an_all_day_series_repeats_by_date() {
        let midnight = |d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(19),
            end: midnight(20),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=2".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(1), midnight(31));
        assert_eq!(got, vec![(midnight(19), midnight(20)), (midnight(26), midnight(27))]);
    }

    #[test]
    fn an_all_day_series_stops_on_a_bare_until_date() {
        // Google writes UNTIL for an all-day series as a bare date, with no
        // time and no Z: it never carries a UTC offset of its own.
        let midnight = |d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(19),
            end: midnight(20),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;UNTIL=20261102".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(1), midnight(31));
        assert_eq!(got, vec![(midnight(19), midnight(20)), (midnight(26), midnight(27))]);
    }

    /// Google writes an all-day series' skipped days as bare dates. The
    /// week skipped must stay skipped in any time zone the machine runs
    /// in: run this under `TZ=Europe/Lisbon` as well as `TZ=UTC`.
    #[test]
    fn an_all_day_series_skips_a_bare_excluded_date() {
        let midnight = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(7, 6),
            end: midnight(7, 7),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=3".into(), "EXDATE;VALUE=DATE:20260713".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(7, 1), midnight(8, 1));
        assert_eq!(got, vec![(midnight(7, 6), midnight(7, 7)), (midnight(7, 20), midnight(7, 21))]);
    }

    #[test]
    fn an_all_day_series_adds_a_bare_extra_date_on_its_own_day() {
        let midnight = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(7, 6),
            end: midnight(7, 7),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=1".into(), "RDATE;VALUE=DATE:20260709,20260710".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(7, 1), midnight(8, 1));
        assert_eq!(
            got,
            vec![(midnight(7, 6), midnight(7, 7)), (midnight(7, 9), midnight(7, 10)), (midnight(7, 10), midnight(7, 11))]
        );
    }

    #[test]
    fn a_rule_nobody_can_read_shows_the_first_occurrence() {
        let event = standup(&["RRULE:FREQ=SOMETIMES"]);
        let got = expand(&event, lisbon(2026, 10, 1, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(got, vec![(event.start, event.end)]);
        assert_eq!(series_end(&event), Some(event.end));
    }

    #[test]
    fn an_occurrence_that_began_before_the_range_still_shows() {
        let mut event = standup(&["RRULE:FREQ=DAILY;COUNT=2"]);
        event.end = event.start + 3 * 60 * 60 * 1000;
        // The range opens an hour into the first occurrence.
        let got = expand(&event, lisbon(2026, 10, 19, 10, 0), lisbon(2026, 10, 19, 11, 0));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_occurrence_of_a_timed_series_is_named_by_its_utc_start() {
        let event = Arc::new(standup(&["RRULE:FREQ=DAILY;COUNT=5"]));
        let thursday = lisbon(2026, 10, 22, 9, 0);
        let one = Occurrence { account_id: 1, event, start: thursday, end: thursday + 15 * 60_000 };
        // 09:00 in Lisbon is 08:00 UTC in October's summer time.
        assert_eq!(one.id(), "standup_20261022T080000Z");
        assert_eq!(split_occurrence_id("standup_20261022T080000Z"), Some(("standup", thursday)));
    }

    #[test]
    fn an_occurrence_of_an_all_day_series_is_named_by_its_date() {
        let midnight = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Arc::new(Event { all_day: true, zone: "UTC".into(), ..standup(&["RRULE:FREQ=WEEKLY"]) });
        let one = Occurrence { account_id: 1, event, start: midnight, end: midnight + 24 * 60 * 60_000 };
        assert_eq!(one.id(), "standup_20261026");
        assert_eq!(split_occurrence_id("standup_20261026"), Some(("standup", midnight)));
    }

    #[test]
    fn an_event_that_does_not_repeat_keeps_its_own_id() {
        let event = Arc::new(standup(&[]));
        let one = Occurrence { account_id: 1, start: event.start, end: event.end, event };
        assert_eq!(one.id(), "standup");
        assert_eq!(split_occurrence_id("standup"), None);
        assert_eq!(split_occurrence_id("team_lunch"), None);
    }

    #[test]
    fn only_an_event_the_account_attends_in_hours_blocks_time() {
        let meeting = standup(&[]);
        assert!(meeting.blocks_time());
        assert!(!Event { busy: false, ..meeting.clone() }.blocks_time(), "marked free");
        assert!(!Event { all_day: true, ..meeting.clone() }.blocks_time(), "all day");
        assert!(!Event { status: Status::Cancelled, ..meeting.clone() }.blocks_time(), "cancelled");
        assert!(!Event { my_answer: Some(invitation::Answer::No), ..meeting.clone() }.blocks_time(), "declined");
        assert!(Event { my_answer: Some(invitation::Answer::Maybe), ..meeting }.blocks_time(), "a maybe still counts");
    }

    #[test]
    fn access_says_who_can_write() {
        assert!(Access::Owner.can_write());
        assert!(Access::Writer.can_write());
        assert!(!Access::Reader.can_write());
        assert!(!Access::FreeBusy.can_write());
        assert_eq!(Access::parse("freeBusyReader"), Access::FreeBusy);
    }
}
