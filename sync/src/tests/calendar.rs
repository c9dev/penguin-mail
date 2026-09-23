//! The events on the primary calendar, against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_gmail::{EventFields, EventTime, GmailError};

use super::{Connected, Harness, harness};
use crate::Permitted;
use crate::calendar::{Calendar, at, free_slots, instant};

const HOUR: i64 = 60 * 60 * 1000;
const MINUTE: i64 = 60 * 1000;

/// 2026-03-10 at 09:00 UTC.
const NINE: i64 = 1_773_133_200_000;

fn calendar(h: &Harness) -> Calendar<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Calendar::new(Arc::new(Connected(connected)))
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
    assert_eq!(made.summary, "Kite day");

    let listed = calendar
        .events(h.account_id, NINE - HOUR, NINE + 2 * HOUR)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(listed, vec![made.clone()]);
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
        moved.summary, "Kite day",
        "what the change left alone stays"
    );
    assert_eq!(moved.start, at(NINE + 2 * HOUR));
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
        matches!(err, crate::SyncError::Backend(crate::BackendError::Gmail(GmailError::ApiDisabled { .. }))),
        "{err}"
    );
    assert!(h.fake.with(|s| s.events.is_empty()));
}
