//! Deleting and moving part of a series that repeats on several weekdays,
//! a Stand-up from Monday to Friday, with each of the three repeat
//! scopes, against the in-memory Gmail and read back from it.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::series::RepeatScope;
use mailrs_domain::calendar::{Access, Calendar, Event, Occurrence};
use mailrs_store::calendar as store;

use super::{Connected, Harness, harness};
use crate::calendar_copy::{CalendarCopy, READ_EVERY_OPEN};
use crate::settings::Permitted;

/// Monday 21 September 2026, 14:13:20 UTC.
const NOW: i64 = 1_790_000_000_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

/// Days after `NOW` that fall Monday to Friday, over the first two weeks.
const WEEKDAYS: [i64; 10] = [0, 1, 2, 3, 4, 7, 8, 9, 10, 11];
/// Wednesday of the first week, the occurrence each test picks.
const PICKED: i64 = 2;

fn standup() -> Event {
    Event {
        calendar: "primary".into(),
        id: "standup".into(),
        title: "Stand-up".into(),
        zone: "Europe/Lisbon".into(),
        start: NOW,
        end: NOW + HOUR / 4,
        busy: true,
        rules: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into()],
        ..Event::default()
    }
}

async fn read_standup(h: &Harness) -> CalendarCopy<Connected> {
    h.fake.with(|s| {
        s.calendars = vec![Calendar {
            id: "primary".into(),
            name: "primary".into(),
            color: "#3584e4".into(),
            access: Access::Owner,
            zone: "Europe/Lisbon".into(),
            primary: true,
            shown: true,
            reminders: Vec::new(),
        }]
    });
    h.fake.put_calendar_event(standup());
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy
}

async fn on_day(h: &Harness, day: i64) -> Vec<Occurrence> {
    let account = h.account_id;
    let from = NOW + day * DAY;
    h.db.read(move |c| store::occurrences(c, &[account], from, from + DAY, store::CalendarScope::Shown))
        .await
        .unwrap()
}

/// When each weekday's stand-up starts, as hours after 14:13 UTC, or
/// `None` for a day with none.
async fn weekday_starts(h: &Harness) -> Vec<(i64, Option<i64>)> {
    let mut starts = Vec::new();
    for day in WEEKDAYS {
        let shown = on_day(h, day).await;
        assert!(shown.len() <= 1, "day {day} shows {} stand-ups", shown.len());
        starts.push((day, shown.first().map(|o| (o.start - NOW - day * DAY) / HOUR)));
    }
    starts
}

/// The same, once the change went out and the copy read Google back.
async fn after_sending(h: &Harness, copy: &CalendarCopy<Connected>) -> Vec<(i64, Option<i64>)> {
    let local = weekday_starts(h).await;
    assert!(copy.send(h.account_id).await.unwrap().is_empty(), "nothing turned down");
    let account = h.account_id;
    assert!(h.db.read(move |c| store::queued(c, account)).await.unwrap().is_empty());
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    let read_back = weekday_starts(h).await;
    assert_eq!(local, read_back, "the copy shows what Google holds");
    read_back
}

fn expect(show: impl Fn(i64) -> Option<i64>) -> Vec<(i64, Option<i64>)> {
    WEEKDAYS.iter().map(|&d| (d, show(d))).collect()
}

async fn delete(scope: RepeatScope) -> Vec<(i64, Option<i64>)> {
    let h = harness().await;
    let copy = read_standup(&h).await;
    assert_eq!(weekday_starts(&h).await, expect(|_| Some(0)), "one stand-up every weekday");
    let wednesday = on_day(&h, PICKED).await.remove(0);
    let steps = copy.delete_steps(h.account_id, &wednesday, Some(scope)).await.unwrap();
    assert!(matches!(copy.apply(h.account_id, steps).await.unwrap(), Permitted::Done(())));
    after_sending(&h, &copy).await
}

async fn move_an_hour(scope: RepeatScope) -> Vec<(i64, Option<i64>)> {
    let h = harness().await;
    let copy = read_standup(&h).await;
    let wednesday = on_day(&h, PICKED).await.remove(0);
    let edited = Event {
        start: wednesday.start + HOUR,
        end: wednesday.end + HOUR,
        ..Event::clone(&wednesday.event)
    };
    let steps = copy.change_steps(h.account_id, &wednesday, edited, Some(scope)).await.unwrap();
    assert!(matches!(copy.apply(h.account_id, steps).await.unwrap(), Permitted::Done(())));
    after_sending(&h, &copy).await
}

#[tokio::test]
async fn deleting_all_events_removes_every_weekday() {
    assert_eq!(delete(RepeatScope::All).await, expect(|_| None));
}

#[tokio::test]
async fn deleting_this_and_following_removes_every_weekday_from_it_on() {
    assert_eq!(delete(RepeatScope::Following).await, expect(|d| (d < PICKED).then_some(0)));
}

#[tokio::test]
async fn deleting_this_event_only_removes_that_day() {
    assert_eq!(delete(RepeatScope::This).await, expect(|d| (d != PICKED).then_some(0)));
}

#[tokio::test]
async fn moving_all_events_moves_every_weekday() {
    assert_eq!(move_an_hour(RepeatScope::All).await, expect(|_| Some(1)));
}

#[tokio::test]
async fn moving_this_and_following_moves_every_weekday_from_it_on() {
    assert_eq!(
        move_an_hour(RepeatScope::Following).await,
        expect(|d| Some(if d < PICKED { 0 } else { 1 }))
    );
}

#[tokio::test]
async fn moving_this_event_only_moves_that_day() {
    assert_eq!(
        move_an_hour(RepeatScope::This).await,
        expect(|d| Some(if d == PICKED { 1 } else { 0 }))
    );
}

/// The stand-up running six weeks, split on the first Wednesday by moving
/// it and every later one an hour, so the later part carries the end the
/// series had and reads back as a custom repeat rather than "Every
/// weekday". Answers the copy and the later part's id.
async fn split_six_weeks(h: &Harness) -> (CalendarCopy<Connected>, String) {
    h.fake.with(|s| {
        s.calendars = vec![Calendar {
            id: "primary".into(),
            name: "primary".into(),
            color: "#3584e4".into(),
            access: Access::Owner,
            zone: "Europe/Lisbon".into(),
            primary: true,
            shown: true,
            reminders: Vec::new(),
        }]
    });
    h.fake.put_calendar_event(Event {
        rules: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;UNTIL=20261101T235959Z".into()],
        ..standup()
    });
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
    copy.refresh(h.account_id, NOW).await.unwrap();
    let wednesday = on_day(h, PICKED).await.remove(0);
    let later = Event { start: wednesday.start + HOUR, end: wednesday.end + HOUR, ..Event::clone(&wednesday.event) };
    let steps = copy.change_steps(h.account_id, &wednesday, later, Some(RepeatScope::Following)).await.unwrap();
    assert!(matches!(copy.apply(h.account_id, steps).await.unwrap(), Permitted::Done(())));
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    let thursday = on_day(h, 10).await.remove(0);
    assert_ne!(thursday.event.id, "standup", "the second week belongs to the later part");
    (copy, thursday.event.id.clone())
}

#[tokio::test]
async fn moving_all_of_a_split_weekday_series_to_another_day_keeps_monday_to_friday() {
    let h = harness().await;
    let (copy, later) = split_six_weeks(&h).await;
    // Thursday of the second week, dragged to Friday.
    let thursday = on_day(&h, 10).await.remove(0);
    let edited = Event { start: thursday.start + DAY, end: thursday.end + DAY, ..Event::clone(&thursday.event) };
    let steps = copy.change_steps(h.account_id, &thursday, edited, Some(RepeatScope::All)).await.unwrap();
    assert!(matches!(copy.apply(h.account_id, steps).await.unwrap(), Permitted::Done(())));
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();

    let rule = h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == later).map(|e| e.rules.clone()));
    assert!(rule.unwrap()[0].contains("BYDAY=MO,TU,WE,TH,FR"), "the later part keeps its days");
    for week in [7, 14, 21] {
        for day in week..week + 7 {
            let shown = on_day(&h, day).await.len();
            let weekday = day - week < 5;
            assert_eq!(shown, usize::from(weekday), "day {day}");
        }
    }
}

/// A split leaves two series on Google, so "All events" on the later
/// part takes every weekday from the split on and leaves the days before
/// it, which belong to the earlier part.
#[tokio::test]
async fn deleting_all_of_the_later_part_of_a_split_leaves_the_earlier_part() {
    let h = harness().await;
    let (copy, _) = split_six_weeks(&h).await;
    let thursday = on_day(&h, 10).await.remove(0);
    let steps = copy.delete_steps(h.account_id, &thursday, Some(RepeatScope::All)).await.unwrap();
    assert!(matches!(copy.apply(h.account_id, steps).await.unwrap(), Permitted::Done(())));
    assert_eq!(after_sending(&h, &copy).await, expect(|d| (d < PICKED).then_some(0)));
}
