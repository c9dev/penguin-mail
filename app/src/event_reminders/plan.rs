//! When each event reminder goes up, worked out from data and the time
//! now, so a test can hand it any clock it likes.
//!
//! A reminder is due once its time has come, it has not gone up before,
//! and its event has not ended. The last rule covers a computer that
//! slept or an app that was not running: a reminder that passed meanwhile
//! goes up late while its event still runs, and never after. A snoozed
//! reminder is due again at its snooze time, under the same rule.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, TimeZone};
use mailrs_domain::calendar::{Access, Calendar, Occurrence, ReminderMethod, Status};
use mailrs_domain::invitation::Answer;
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::event_reminders::{Key, Logged};

pub const MINUTE: EpochMillis = 60_000;

/// How far ahead the scheduler reads the calendar. Google allows a
/// reminder at most four weeks before its event; one day more covers it.
pub const LOOK_AHEAD: EpochMillis = 29 * 24 * 60 * MINUTE;

/// How far back it reads. An all-day event ends at midnight where its
/// calendar is, which can be hours after the midnight UTC the store
/// keeps, and it may still be running.
pub const LOOK_BACK: EpochMillis = 24 * 60 * MINUTE;

/// The longest the scheduler sleeps. Its timer runs on the monotonic
/// clock, which stops while the computer sleeps, so a wake, a clock
/// change or a fresh read of the calendars shows within a minute.
pub const LONGEST_WAIT: EpochMillis = MINUTE;

pub const SNOOZE: EpochMillis = 5 * MINUTE;

/// One reminder whose time has come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub key: Key,
    pub occurrence: Occurrence,
    /// When the occurrence starts and ends. An all-day event's day runs
    /// from midnight to midnight in its calendar's zone.
    pub starts: EpochMillis,
    pub ends: EpochMillis,
}

/// What one check does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Reminders to put up now.
    pub post: Vec<Due>,
    /// Reminders that are due but stay down: a second one of an
    /// occurrence that already has one going up, or every one while
    /// Event Reminders is off. They are recorded as shown all the same,
    /// so turning the switch back on brings no backlog.
    pub quiet: Vec<Due>,
    /// The next time a reminder falls due, if any does within the range
    /// the scheduler read.
    pub next: Option<EpochMillis>,
}

pub fn plan(
    occurrences: &[Occurrence],
    calendars: &HashMap<(AccountId, String), Calendar>,
    log: &HashMap<Key, Logged>,
    now: EpochMillis,
    enabled: bool,
) -> Plan {
    let mut due: Vec<Due> = Vec::new();
    let mut next: Option<EpochMillis> = None;
    for occurrence in occurrences {
        let Some(calendar) = calendars.get(&(occurrence.account_id, occurrence.event.calendar.clone())) else {
            continue;
        };
        if !reminds(occurrence, calendar) {
            continue;
        }
        let (starts, ends) = span(occurrence, &calendar.zone);
        if ends <= now {
            continue;
        }
        let reminders = occurrence.event.reminders.as_ref().unwrap_or(&calendar.reminders);
        for reminder in reminders.iter().filter(|r| r.method == ReminderMethod::Notification) {
            let key = Key {
                account_id: occurrence.account_id,
                calendar: occurrence.event.calendar.clone(),
                event: occurrence.event.id.clone(),
                start: occurrence.start,
                minutes: reminder.minutes,
            };
            let at = match log.get(&key) {
                None => starts - EpochMillis::from(reminder.minutes) * MINUTE,
                Some(Logged { snoozed_until: Some(until), .. }) => *until,
                Some(_) => continue,
            };
            if at <= now {
                due.push(Due { key, occurrence: occurrence.clone(), starts, ends });
            } else if at < ends {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
    }
    // One notification per occurrence: the reminder nearest its start
    // goes up and the others of the same occurrence stay down.
    due.sort_by(|a, b| {
        (a.starts, a.key.account_id, &a.key.calendar, &a.key.event, a.key.minutes)
            .cmp(&(b.starts, b.key.account_id, &b.key.calendar, &b.key.event, b.key.minutes))
    });
    let mut plan = Plan { next, ..Plan::default() };
    let mut last: Option<(AccountId, String, String, EpochMillis)> = None;
    for one in due {
        let occurrence = (one.key.account_id, one.key.calendar.clone(), one.key.event.clone(), one.key.start);
        if enabled && last.as_ref() != Some(&occurrence) {
            plan.post.push(one);
        } else {
            plan.quiet.push(one);
        }
        last = Some(occurrence);
    }
    plan
}

/// How long to sleep before the next check: until `next`, never more
/// than [`LONGEST_WAIT`], and never less than a second, so a time
/// already past cannot spin the loop.
pub fn wait(next: Option<EpochMillis>, now: EpochMillis) -> Duration {
    let millis = next.map_or(LONGEST_WAIT, |at| (at - now).clamp(1_000, LONGEST_WAIT));
    Duration::from_millis(millis as u64)
}

/// The id a reminder's notification goes up under. Posting the same
/// reminder again, as a snooze does, replaces the one on screen.
pub fn notification_id(key: &Key) -> String {
    format!(
        "event-reminder/{}/{}/{}/{}/{}",
        key.account_id, key.calendar, key.event, key.start, key.minutes
    )
}

/// Whether an occurrence reminds at all. A calendar that shows only
/// free and busy times gives no title to remind of.
fn reminds(occurrence: &Occurrence, calendar: &Calendar) -> bool {
    occurrence.event.status != Status::Cancelled
        && occurrence.event.my_answer != Some(Answer::No)
        && calendar.access != Access::FreeBusy
}

/// When an occurrence starts and ends. The store keeps an all-day event
/// from midnight UTC of its first day; Google counts its reminders from
/// midnight in the calendar's zone, so the day moves there.
fn span(occurrence: &Occurrence, zone: &str) -> (EpochMillis, EpochMillis) {
    if !occurrence.event.all_day {
        return (occurrence.start, occurrence.end);
    }
    let tz: chrono_tz::Tz = zone.parse().unwrap_or(chrono_tz::UTC);
    let local_midnight = |at: EpochMillis| -> EpochMillis {
        DateTime::from_timestamp_millis(at)
            .and_then(|utc| utc.date_naive().and_hms_opt(0, 0, 0))
            .and_then(|midnight| tz.from_local_datetime(&midnight).earliest())
            .map_or(at, |local| local.timestamp_millis())
    };
    (local_midnight(occurrence.start), local_midnight(occurrence.end))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use mailrs_domain::calendar::{Event, Reminder};
    use mailrs_domain::invitation::Answer;

    const ACCOUNT: AccountId = 1;
    const MIN: EpochMillis = MINUTE;
    const HOUR: EpochMillis = 60 * MIN;
    const DAY: EpochMillis = 24 * HOUR;
    // Wednesday 23 September 2026, 14:00 in Lisbon (13:00 UTC).
    const TWO_PM: EpochMillis = 1_790_168_400_000;

    fn minutes(minutes: u32) -> Reminder {
        Reminder { minutes, method: ReminderMethod::Notification }
    }

    fn calendar(reminders: Vec<Reminder>) -> Calendar {
        Calendar {
            id: "work".into(),
            name: "Work".into(),
            color: "#e8660c".into(),
            access: Access::Owner,
            zone: "Europe/Lisbon".into(),
            primary: true,
            shown: true,
            hidden: false,
            reminders,
        }
    }

    fn meeting(id: &str, start: EpochMillis, end: EpochMillis, reminders: Option<Vec<Reminder>>) -> Occurrence {
        Occurrence {
            account_id: ACCOUNT,
            start,
            end,
            event: Arc::new(Event {
                calendar: "work".into(),
                id: id.into(),
                title: id.into(),
                start,
                end,
                zone: "Europe/Lisbon".into(),
                busy: true,
                reminders,
                ..Event::default()
            }),
        }
    }

    fn key(event: &str, start: EpochMillis, minutes: u32) -> Key {
        Key { account_id: ACCOUNT, calendar: "work".into(), event: event.into(), start, minutes }
    }

    fn run(occurrences: &[Occurrence], calendar: &Calendar, log: &HashMap<Key, Logged>, now: EpochMillis) -> Plan {
        let calendars = HashMap::from([((ACCOUNT, calendar.id.clone()), calendar.clone())]);
        plan(occurrences, &calendars, log, now, true)
    }

    fn posted(plan: &Plan) -> Vec<(String, u32)> {
        plan.post.iter().map(|d| (d.key.event.clone(), d.key.minutes)).collect()
    }

    fn nothing() -> HashMap<Key, Logged> {
        HashMap::new()
    }

    #[test]
    fn a_reminder_waits_for_its_time_then_comes_due() {
        let work = calendar(vec![minutes(10)]);
        let standup = [meeting("standup", TWO_PM, TWO_PM + HOUR, None)];
        let early = run(&standup, &work, &nothing(), TWO_PM - 20 * MIN);
        assert!(early.post.is_empty());
        assert_eq!(early.next, Some(TWO_PM - 10 * MIN));
        let due = run(&standup, &work, &nothing(), TWO_PM - 10 * MIN);
        assert_eq!(posted(&due), vec![("standup".to_string(), 10)]);
        assert_eq!(due.post[0].ends, TWO_PM + HOUR);
    }

    #[test]
    fn email_reminders_never_come_due() {
        let work = calendar(Vec::new());
        let email = Reminder { minutes: 10, method: ReminderMethod::Email };
        let plan = run(&[meeting("standup", TWO_PM, TWO_PM + HOUR, Some(vec![email]))], &work, &nothing(), TWO_PM - 5 * MIN);
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn the_events_own_reminders_replace_the_calendar_default() {
        let work = calendar(vec![minutes(10)]);
        let plan = run(&[meeting("standup", TWO_PM, TWO_PM + HOUR, Some(vec![minutes(30)]))], &work, &nothing(), TWO_PM - 5 * MIN);
        assert_eq!(posted(&plan), vec![("standup".to_string(), 30)]);
        assert!(plan.quiet.is_empty(), "the calendar's ten minutes never applied");
    }

    #[test]
    fn an_event_with_an_empty_list_overrides_the_calendar_default() {
        let work = calendar(vec![minutes(10)]);
        let plan = run(&[meeting("standup", TWO_PM, TWO_PM + HOUR, Some(Vec::new()))], &work, &nothing(), TWO_PM - 5 * MIN);
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn declined_cancelled_and_free_busy_events_never_remind() {
        let work = calendar(vec![minutes(10)]);
        let mut declined = meeting("declined", TWO_PM, TWO_PM + HOUR, None);
        declined.event = Arc::new(Event { my_answer: Some(Answer::No), ..(*declined.event).clone() });
        let mut cancelled = meeting("cancelled", TWO_PM, TWO_PM + HOUR, None);
        cancelled.event = Arc::new(Event { status: Status::Cancelled, ..(*cancelled.event).clone() });
        assert_eq!(run(&[declined, cancelled], &work, &nothing(), TWO_PM - 5 * MIN), Plan::default());
        let busy = Calendar { access: Access::FreeBusy, ..work };
        assert_eq!(run(&[meeting("busy", TWO_PM, TWO_PM + HOUR, None)], &busy, &nothing(), TWO_PM - 5 * MIN), Plan::default());
    }

    #[test]
    fn a_shown_reminder_stays_quiet() {
        let work = calendar(vec![minutes(10)]);
        let log = HashMap::from([(key("standup", TWO_PM, 10), Logged { shown_at: TWO_PM - 10 * MIN, snoozed_until: None })]);
        let plan = run(&[meeting("standup", TWO_PM, TWO_PM + HOUR, None)], &work, &log, TWO_PM - 5 * MIN);
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn a_snooze_brings_it_back_five_minutes_later() {
        let work = calendar(vec![minutes(10)]);
        let standup = [meeting("standup", TWO_PM, TWO_PM + HOUR, None)];
        let log = HashMap::from([(
            key("standup", TWO_PM, 10),
            Logged { shown_at: TWO_PM - 10 * MIN, snoozed_until: Some(TWO_PM - 5 * MIN) },
        )]);
        let early = run(&standup, &work, &log, TWO_PM - 8 * MIN);
        assert!(early.post.is_empty());
        assert_eq!(early.next, Some(TWO_PM - 5 * MIN));
        let back = run(&standup, &work, &log, TWO_PM - 5 * MIN);
        assert_eq!(posted(&back), vec![("standup".to_string(), 10)]);
    }

    #[test]
    fn a_reminder_missed_in_suspend_fires_only_while_the_event_runs() {
        let work = calendar(vec![minutes(10)]);
        let over = meeting("over", TWO_PM - HOUR, TWO_PM - 30 * MIN, None);
        let running = meeting("running", TWO_PM, TWO_PM + HOUR, None);
        // Shut at 13:40, opened at 14:20.
        let plan = run(&[over, running], &work, &nothing(), TWO_PM + 20 * MIN);
        assert_eq!(posted(&plan), vec![("running".to_string(), 10)]);
        assert!(plan.quiet.is_empty());
    }

    #[test]
    fn two_missed_reminders_of_one_event_post_once() {
        let work = calendar(Vec::new());
        let plan = run(&[meeting("standup", TWO_PM, TWO_PM + HOUR, Some(vec![minutes(30), minutes(10)]))], &work, &nothing(), TWO_PM - 5 * MIN);
        assert_eq!(posted(&plan), vec![("standup".to_string(), 10)]);
        assert_eq!(plan.quiet.iter().map(|d| d.key.minutes).collect::<Vec<_>>(), vec![30]);
    }

    #[test]
    fn an_all_day_reminder_counts_from_local_midnight() {
        let work = calendar(Vec::new());
        // Thursday 24 September, stored from midnight UTC as stage 1 keeps all-day events.
        let thursday: EpochMillis = 1_790_208_000_000;
        let mut offsite = meeting("offsite", thursday, thursday + DAY, Some(vec![minutes(420)]));
        offsite.event = Arc::new(Event {
            all_day: true,
            zone: "UTC".into(),
            ..(*offsite.event).clone()
        });
        // 17:00 in Lisbon on Wednesday is 16:00 UTC.
        let five_pm: EpochMillis = 1_790_179_200_000;
        let before = run(std::slice::from_ref(&offsite), &work, &nothing(), five_pm - MIN);
        assert!(before.post.is_empty());
        assert_eq!(before.next, Some(five_pm));
        let due = run(&[offsite], &work, &nothing(), five_pm);
        assert_eq!(posted(&due), vec![("offsite".to_string(), 420)]);
        assert_eq!(due.post[0].starts, five_pm + 7 * HOUR, "the day starts at midnight in Lisbon");
    }

    #[test]
    fn with_reminders_off_due_ones_are_settled_quietly() {
        let work = calendar(vec![minutes(10)]);
        let calendars = HashMap::from([((ACCOUNT, work.id.clone()), work)]);
        let plan = plan(&[meeting("standup", TWO_PM, TWO_PM + HOUR, None)], &calendars, &nothing(), TWO_PM - 5 * MIN, false);
        assert!(plan.post.is_empty());
        assert_eq!(plan.quiet.len(), 1);
    }

    #[test]
    fn a_snooze_that_lands_after_the_end_stays_quiet() {
        let work = calendar(vec![minutes(10)]);
        let short = [meeting("short", TWO_PM, TWO_PM + 15 * MIN, None)];
        let log = HashMap::from([(
            key("short", TWO_PM, 10),
            Logged { shown_at: TWO_PM + 10 * MIN, snoozed_until: Some(TWO_PM + 17 * MIN) },
        )]);
        let during = run(&short, &work, &log, TWO_PM + 11 * MIN);
        assert!(during.post.is_empty());
        assert_eq!(during.next, None, "nothing to wake for");
        assert_eq!(run(&short, &work, &log, TWO_PM + 17 * MIN), Plan::default());
    }

    #[test]
    fn the_scheduler_wakes_at_least_once_a_minute() {
        use std::time::Duration;
        assert_eq!(wait(None, TWO_PM), Duration::from_secs(60));
        assert_eq!(wait(Some(TWO_PM + 10_000), TWO_PM), Duration::from_secs(10));
        assert_eq!(wait(Some(TWO_PM + 3 * HOUR), TWO_PM), Duration::from_secs(60));
        assert_eq!(wait(Some(TWO_PM - 5), TWO_PM), Duration::from_secs(1));
    }

    #[test]
    fn a_notification_id_names_the_occurrence_and_the_minutes() {
        let ten = notification_id(&key("standup", TWO_PM, 10));
        assert_eq!(ten, notification_id(&key("standup", TWO_PM, 10)));
        assert_ne!(ten, notification_id(&key("standup", TWO_PM, 30)));
        assert_ne!(ten, notification_id(&key("standup", TWO_PM + DAY, 10)));
    }
}
