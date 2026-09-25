//! The events on the primary calendar, against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::{Access, Calendar as Cal, Event as Ev};
use mailrs_gmail::{EventFields, EventTime, GmailError};

use super::{Connected, Harness, harness};
use crate::CalendarService;
use crate::Permitted;
use crate::calendar::{Calendar, at, free_slots, instant};
use crate::calendar_copy::CalendarCopy;

const HOUR: i64 = 60 * 60 * 1000;
const MINUTE: i64 = 60 * 1000;

/// 2026-03-10 at 09:00 UTC.
const NINE: i64 = 1_773_133_200_000;

/// A `Calendar` and the `CalendarCopy` behind it, over the same
/// `Connected` accounts and store, as `copy()` does for `CalendarCopy`'s
/// own tests. A test that never calls `copy.refresh` stays on the live
/// path; the copy is here so one that wants it can.
fn calendar_with_copy(h: &Harness) -> (Calendar<Connected>, Arc<CalendarCopy<Connected>>) {
    let connected = Arc::new(Connected(HashMap::from([(h.account_id, Arc::clone(&h.sync))])));
    let copy = Arc::new(CalendarCopy::new(Arc::clone(&connected), h.db.clone()));
    (Calendar::new(connected, h.db.clone(), Arc::clone(&copy)), copy)
}

fn calendar(h: &Harness) -> Calendar<Connected> {
    let connected = Arc::new(Connected(HashMap::from([(h.account_id, Arc::clone(&h.sync))])));
    let copy = Arc::new(CalendarCopy::new(Arc::clone(&connected), h.db.clone()));
    Calendar::new(connected, h.db.clone(), copy)
}

fn event(summary: &str, from: i64, to: i64) -> EventFields {
    EventFields {
        summary: Some(summary.into()),
        start: at(from),
        end: at(to),
        ..EventFields::default()
    }
}

#[test]
fn an_instant_goes_out_and_comes_back_the_same() {
    assert_eq!(
        at(NINE),
        Some(EventTime::At("2026-03-10T09:00:00+00:00".into()))
    );
    assert_eq!(instant(&at(NINE).unwrap()), Some(NINE));
    assert_eq!(
        instant(&EventTime::At("2026-03-10T10:00:00+01:00".into())),
        Some(NINE),
        "an offset is honoured"
    );
    assert_eq!(
        instant(&EventTime::Day("2026-03-10".into())),
        Some(NINE - 9 * HOUR)
    );
}

#[test]
fn free_time_is_what_no_busy_span_touches_inside_the_windows() {
    let day = (NINE, NINE + 8 * HOUR);
    let busy = [
        (NINE + HOUR, NINE + 2 * HOUR),
        // Overlaps the one before and runs on past it.
        (NINE + HOUR + 30 * MINUTE, NINE + 3 * HOUR),
        // Leaves a gap too short for the meeting.
        (NINE + 3 * HOUR + 20 * MINUTE, NINE + 4 * HOUR),
        // Runs past the end of the day.
        (NINE + 7 * HOUR, NINE + 10 * HOUR),
    ];
    assert_eq!(
        free_slots(&busy, &[day], 30 * MINUTE),
        vec![(NINE, NINE + HOUR), (NINE + 4 * HOUR, NINE + 7 * HOUR)]
    );
    assert_eq!(
        free_slots(&[], &[day, (NINE + 24 * HOUR, NINE + 25 * HOUR)], HOUR),
        vec![day, (NINE + 24 * HOUR, NINE + 25 * HOUR)],
        "an empty calendar leaves every window whole"
    );
    assert!(free_slots(&busy, &[day], 5 * HOUR).is_empty());
}

#[tokio::test]
async fn an_event_is_made_listed_moved_and_deleted() {
    let h = harness().await;
    let calendar = calendar(&h);

    let made = calendar
        .create(h.account_id, &event("Kite day", NINE, NINE + HOUR))
        .await
        .unwrap()
        .done()
        .expect("the permission is there");
    assert_eq!(made.title, "Kite day");

    let listed = calendar
        .events(h.account_id, NINE - HOUR, NINE + 2 * HOUR)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(*listed[0].event, made);
    let later = calendar
        .events(h.account_id, NINE + 2 * HOUR, NINE + 3 * HOUR)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert!(later.is_empty(), "the event ends before the window opens");

    let moved = calendar
        .update(
            h.account_id,
            &made.id,
            &EventFields {
                start: at(NINE + 2 * HOUR),
                end: at(NINE + 3 * HOUR),
                guests: Some(vec!["ann@example.com".into()]),
                ..EventFields::default()
            },
        )
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(
        moved.title, "Kite day",
        "what the change left alone stays"
    );
    assert_eq!(moved.start, NINE + 2 * HOUR);
    assert_eq!(moved.guests[0].email, "ann@example.com");

    assert_eq!(
        calendar.delete(h.account_id, &made.id).await.unwrap(),
        Permitted::Done(())
    );
    assert!(h.fake.with(|s| s.events.is_empty()));
    assert!(
        calendar.delete(h.account_id, &made.id).await.is_err(),
        "a second delete finds nothing"
    );
}

#[tokio::test]
async fn free_time_reads_the_calendar_once_for_every_window() {
    let h = harness().await;
    let calendar = calendar(&h);
    calendar
        .create(
            h.account_id,
            &event("Design crit", NINE + HOUR, NINE + 2 * HOUR),
        )
        .await
        .unwrap();
    h.fake.reset_usage();

    let windows = [
        (NINE, NINE + 3 * HOUR),
        (NINE + 24 * HOUR, NINE + 25 * HOUR),
    ];
    let free = calendar
        .free(h.account_id, &windows, HOUR)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(
        free,
        vec![
            (NINE, NINE + HOUR),
            (NINE + 2 * HOUR, NINE + 3 * HOUR),
            (NINE + 24 * HOUR, NINE + 25 * HOUR),
        ]
    );
    assert_eq!(h.fake.usage().calls_to("calendar.events.list"), 1);
}

#[tokio::test]
async fn a_missing_permission_is_an_answer_and_a_switched_off_api_an_error() {
    let h = harness().await;
    let calendar = calendar(&h);

    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        calendar
            .events(h.account_id, NINE, NINE + HOUR)
            .await
            .unwrap(),
        Permitted::NeedsPermission
    );
    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        calendar
            .free(h.account_id, &[(NINE, NINE + HOUR)], HOUR)
            .await
            .unwrap(),
        Permitted::NeedsPermission
    );

    h.fake.fail_next(GmailError::ApiDisabled {
        service: "Google Calendar API".into(),
        enable_url: "https://console.example/calendar".into(),
    });
    let err = calendar
        .create(h.account_id, &event("Kite day", NINE, NINE + HOUR))
        .await
        .unwrap_err();
    assert!(
        matches!(err, crate::SyncError::Backend(crate::BackendError::ApiDisabled { .. })),
        "{err}"
    );
    assert!(h.fake.with(|s| s.events.is_empty()));
}

fn primary() -> Cal {
    Cal {
        id: "primary".into(),
        name: "Personal".into(),
        color: "#e8660c".into(),
        access: Access::Owner,
        zone: "UTC".into(),
        primary: true,
        shown: true,
        reminders: Vec::new(),
    }
}

#[tokio::test]
async fn once_the_copy_is_read_listing_events_asks_google_nothing() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    h.fake.put_calendar_event(Ev {
        calendar: "primary".into(),
        id: "a".into(),
        title: "Lunch".into(),
        zone: "UTC".into(),
        start: 1_790_000_000_000,
        end: 1_790_003_600_000,
        busy: true,
        ..Ev::default()
    });
    let (calendar, copy) = calendar_with_copy(&h);
    copy.refresh(h.account_id, 1_790_000_000_000).await.unwrap();
    let before = h.fake.usage().calls_to("calendar.events.list");
    let found = calendar
        .events(h.account_id, 1_789_990_000_000, 1_790_010_000_000)
        .await
        .unwrap();
    assert!(matches!(found, Permitted::Done(ref list) if list.len() == 1 && list[0].event.title == "Lunch"));
    assert_eq!(h.fake.usage().calls_to("calendar.events.list"), before);
}

#[tokio::test]
async fn an_event_the_assistant_makes_waits_in_the_queue() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let (calendar, copy) = calendar_with_copy(&h);
    copy.refresh(h.account_id, 1_790_000_000_000).await.unwrap();
    let made = calendar
        .create(
            h.account_id,
            &EventFields {
                summary: Some("Dentist".into()),
                start: crate::calendar::at(1_790_000_000_000),
                end: crate::calendar::at(1_790_003_600_000),
                ..EventFields::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(made, Permitted::Done(ref e) if e.pending));
    assert!(
        h.fake.with(|s| s.calendar_events.is_empty()),
        "nothing reached Google before the send"
    );
}

#[tokio::test]
async fn the_fake_hands_back_changes_since_a_token() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    h.fake.put_calendar_event(Ev {
        calendar: "primary".into(),
        id: "a".into(),
        title: "One".into(),
        zone: "UTC".into(),
        ..Ev::default()
    });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let first = calendar.event_changes("primary", None, None, 0).await.unwrap();
    assert_eq!(first.events.len(), 1);
    let token = first.next_sync.unwrap();
    h.fake.put_calendar_event(Ev {
        calendar: "primary".into(),
        id: "b".into(),
        title: "Two".into(),
        zone: "UTC".into(),
        ..Ev::default()
    });
    h.fake.drop_calendar_event("primary", "a");
    let next = calendar.event_changes("primary", Some(&token), None, 0).await.unwrap();
    assert_eq!(next.events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
    assert_eq!(next.removed, vec!["a".to_string()]);
}

#[tokio::test]
async fn the_calendar_list_needs_the_list_permission() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_SCOPE);
    let calendar = h.sync.services().calendar.clone().unwrap();
    assert!(matches!(calendar.calendars().await, Err(crate::BackendError::NeedsPermission)));
}

#[tokio::test]
async fn the_fake_refuses_a_write_against_an_old_version() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    h.fake.put_calendar_event(Ev { calendar: "primary".into(), id: "a".into(), zone: "UTC".into(), ..Ev::default() });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let stale = Ev { calendar: "primary".into(), id: "a".into(), zone: "UTC".into(), ..Ev::default() };
    let err = calendar.put_event(&stale, Some("\"0\""), false).await.unwrap_err();
    assert!(matches!(err, crate::BackendError::Changed), "{err}");
}

/// Monday 19 October 2026, 00:00 UTC.
const MONDAY: i64 = 1_792_368_000_000;
const DAY: i64 = 24 * HOUR;

/// A daily stand-up at 09:00 UTC, Monday to Friday of that week, on
/// Google and read into the copy.
async fn synced_standup(h: &Harness) -> Calendar<Connected> {
    h.fake.with(|s| s.calendars = vec![primary()]);
    h.fake.put_calendar_event(Ev {
        calendar: "primary".into(),
        id: "standup".into(),
        uid: "standup@google.com".into(),
        title: "Stand-up".into(),
        zone: "UTC".into(),
        start: MONDAY + 9 * HOUR,
        end: MONDAY + 9 * HOUR + 15 * MINUTE,
        busy: true,
        rules: vec!["RRULE:FREQ=DAILY;COUNT=5".into()],
        ..Ev::default()
    });
    let (calendar, copy) = calendar_with_copy(h);
    copy.refresh(h.account_id, MONDAY).await.unwrap();
    calendar
}

async fn week_starts(calendar: &Calendar<Connected>, h: &Harness) -> Vec<i64> {
    let week = calendar.events(h.account_id, MONDAY, MONDAY + 5 * DAY).await.unwrap().done().unwrap();
    week.iter().map(|o| o.start).collect()
}

async fn thursday_id(calendar: &Calendar<Connected>, h: &Harness) -> String {
    let week = calendar.events(h.account_id, MONDAY, MONDAY + 5 * DAY).await.unwrap().done().unwrap();
    week.iter().find(|o| o.start == MONDAY + 3 * DAY + 9 * HOUR).expect("Thursday's stand-up").id()
}

/// Google's own copy of the series, which a change to one occurrence
/// must leave as it was.
fn series_on_google(h: &Harness) -> Ev {
    h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == "standup").cloned()).expect("the series")
}

#[tokio::test]
async fn moving_one_occurrence_of_a_series_moves_only_that_one() {
    let h = harness().await;
    let calendar = synced_standup(&h).await;
    let before = series_on_google(&h);
    let thursday = thursday_id(&calendar, &h).await;
    assert_eq!(thursday, "standup_20261022T090000Z");

    let ten = MONDAY + 3 * DAY + 10 * HOUR;
    calendar.update(h.account_id, &thursday, &event("Stand-up", ten, ten + 15 * MINUTE)).await.unwrap().done().unwrap();

    assert_eq!(
        week_starts(&calendar, &h).await,
        vec![MONDAY + 9 * HOUR, MONDAY + DAY + 9 * HOUR, MONDAY + 2 * DAY + 9 * HOUR, ten, MONDAY + 4 * DAY + 9 * HOUR]
    );
    assert_eq!(series_on_google(&h), before, "the series itself is untouched");
    let moved = h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == thursday).cloned()).expect("an exception");
    assert_eq!((moved.series.as_deref(), moved.start), (Some("standup"), ten));
}

#[tokio::test]
async fn cancelling_one_occurrence_of_a_series_cancels_only_that_one() {
    let h = harness().await;
    let calendar = synced_standup(&h).await;
    let before = series_on_google(&h);
    let thursday = thursday_id(&calendar, &h).await;

    calendar.delete(h.account_id, &thursday).await.unwrap().done().unwrap();

    assert_eq!(
        week_starts(&calendar, &h).await,
        vec![MONDAY + 9 * HOUR, MONDAY + DAY + 9 * HOUR, MONDAY + 2 * DAY + 9 * HOUR, MONDAY + 4 * DAY + 9 * HOUR]
    );
    assert_eq!(series_on_google(&h), before, "the series itself is untouched");
}

/// Free time counts what the clash line counts: an event the account
/// declined, one marked free and an all-day one leave the time open, as
/// the live path did before the copy.
#[tokio::test]
async fn free_time_from_the_copy_leaves_declined_free_and_all_day_events_open() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let at = |id: &str, from: i64, to: i64| Ev {
        calendar: "primary".into(),
        id: id.into(),
        title: id.into(),
        zone: "UTC".into(),
        start: from,
        end: to,
        busy: true,
        ..Ev::default()
    };
    h.fake.put_calendar_event(Ev { my_answer: Some(mailrs_domain::invitation::Answer::No), ..at("declined", NINE, NINE + HOUR) });
    h.fake.put_calendar_event(Ev { busy: false, ..at("free", NINE + HOUR, NINE + 2 * HOUR) });
    let midnight = NINE - 9 * HOUR;
    h.fake.put_calendar_event(Ev { all_day: true, ..at("holiday", midnight, midnight + 24 * HOUR) });
    let (calendar, copy) = calendar_with_copy(&h);
    copy.refresh(h.account_id, NINE).await.unwrap();

    let free = calendar.free(h.account_id, &[(NINE, NINE + 3 * HOUR)], HOUR).await.unwrap().done().unwrap();

    assert_eq!(free, vec![(NINE, NINE + 3 * HOUR)]);
}
