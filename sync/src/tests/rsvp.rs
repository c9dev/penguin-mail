//! Answering an invitation as a guest from the calendar or from the mail
//! card: the answer goes into the calendar's queue, survives the network
//! going and a restart, and reaches Google on the occurrence or the series
//! the person picked, with the note they wrote.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::Address;
use mailrs_domain::calendar::series::RepeatScope;
use mailrs_domain::calendar::{Access, Calendar, Event, Guest, Occurrence, occurrence_id};
use mailrs_domain::invitation::{Answer, Scope};
use mailrs_store::calendar as store;

use super::{Connected, Harness, harness};
use crate::calendar_copy::{CalendarCopy, READ_EVERY_OPEN};
use crate::invitations::{Invitations, Told};
use crate::settings::Permitted;

/// Monday 21 September 2026, 14:13:20 UTC.
const NOW: i64 = 1_790_000_000_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const UID: &str = "standup@google.com";

/// A daily stand-up Priya runs, with the account as a guest who has not
/// answered yet.
fn standup() -> Event {
    Event {
        calendar: "primary".into(),
        id: "standup".into(),
        uid: UID.into(),
        title: "Stand-up".into(),
        zone: "UTC".into(),
        start: NOW,
        end: NOW + HOUR / 4,
        busy: true,
        rules: vec!["RRULE:FREQ=DAILY;COUNT=5".into()],
        guests: vec![
            Guest { email: "priya@example.com".into(), organizer: true, ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ],
        ..Event::default()
    }
}

fn connected(h: &Harness) -> Arc<Connected> {
    Arc::new(Connected(HashMap::from([(h.account_id, Arc::clone(&h.sync))])))
}

fn invitations(h: &Harness) -> Invitations<Connected> {
    Invitations::new(connected(h), h.db.clone())
}

fn copy(h: &Harness) -> CalendarCopy<Connected> {
    CalendarCopy::new(connected(h), h.db.clone())
}

async fn read_standup(h: &Harness) -> CalendarCopy<Connected> {
    h.fake.with(|s| {
        s.calendars = vec![Calendar {
            id: "primary".into(),
            name: "primary".into(),
            color: "#3584e4".into(),
            access: Access::Owner,
            zone: "UTC".into(),
            primary: true,
            shown: true,
            reminders: Vec::new(),
        }]
    });
    h.fake.put_calendar_event(standup());
    let copy = copy(h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy
}

async fn on_day(h: &Harness, day: i64) -> Occurrence {
    let account = h.account_id;
    let from = NOW + day * DAY;
    let mut found = h
        .db
        .read(move |c| store::occurrences(c, &[account], from, from + DAY / 2, store::CalendarScope::Shown))
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "one stand-up on day {day}");
    found.remove(0)
}

/// The account's answer on each of the five days, as the copy shows it.
async fn answers(h: &Harness) -> Vec<Option<Answer>> {
    let mut all = Vec::new();
    for day in 0..5 {
        all.push(on_day(h, day).await.event.my_answer);
    }
    all
}

async fn queued(h: &Harness) -> Vec<store::QueuedChange> {
    let account = h.account_id;
    h.db.read(move |c| store::queued(c, account)).await.unwrap()
}

#[tokio::test]
async fn answering_this_event_reaches_google_on_that_occurrence_alone() {
    let h = harness().await;
    let copy = read_standup(&h).await;
    let wednesday = on_day(&h, 2).await;

    let done = invitations(&h)
        .answer_event(h.account_id, &wednesday, Answer::No, RepeatScope::This, Some("On leave".into()))
        .await
        .unwrap();
    assert_eq!(done, Permitted::Done(()));
    assert_eq!(answers(&h).await, [None, None, Some(Answer::No), None, None], "the copy shows it at once");
    assert_eq!(queued(&h).await.len(), 1, "the answer waits in the queue");

    assert!(copy.send(h.account_id).await.unwrap().is_empty(), "nothing turned down");
    let instance = occurrence_id(&standup(), NOW + 2 * DAY);
    assert_eq!(
        h.fake.with(|s| s.answered_events.clone()),
        [("primary".to_string(), instance, Answer::No, Some("On leave".to_string()))]
    );
    assert!(queued(&h).await.is_empty());

    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert_eq!(answers(&h).await, [None, None, Some(Answer::No), None, None], "Google holds the same");
}

#[tokio::test]
async fn answering_all_events_answers_the_series() {
    let h = harness().await;
    let copy = read_standup(&h).await;
    let wednesday = on_day(&h, 2).await;

    invitations(&h)
        .answer_event(h.account_id, &wednesday, Answer::Yes, RepeatScope::All, None)
        .await
        .unwrap();
    assert_eq!(answers(&h).await, [Some(Answer::Yes); 5]);

    copy.send(h.account_id).await.unwrap();
    assert_eq!(
        h.fake.with(|s| s.answered_events.clone()),
        [("primary".to_string(), "standup".to_string(), Answer::Yes, None)]
    );
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert_eq!(answers(&h).await, [Some(Answer::Yes); 5]);
}

#[tokio::test]
async fn an_answer_given_offline_goes_out_after_a_restart() {
    let h = harness().await;
    let copy = read_standup(&h).await;
    let wednesday = on_day(&h, 2).await;
    h.fake.with(|s| s.offline = true);

    invitations(&h)
        .answer_event(h.account_id, &wednesday, Answer::Maybe, RepeatScope::This, Some("Might be late".into()))
        .await
        .unwrap();
    assert!(copy.send(h.account_id).await.is_err(), "the send waits for the network");
    assert_eq!(queued(&h).await.len(), 1, "the answer stays queued");
    drop(copy);

    // A new run: nothing but the store carries the answer over.
    h.fake.with(|s| s.offline = false);
    let restarted = self::copy(&h);
    assert!(restarted.send(h.account_id).await.unwrap().is_empty());
    let instance = occurrence_id(&standup(), NOW + 2 * DAY);
    assert_eq!(
        h.fake.with(|s| s.answered_events.clone()),
        [("primary".to_string(), instance, Answer::Maybe, Some("Might be late".to_string()))]
    );
}

#[tokio::test]
async fn the_mail_card_answers_through_the_same_queue() {
    let h = harness().await;
    let copy = read_standup(&h).await;
    let invitation = mailrs_domain::invitation::read(&format!(
        "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{UID}\r\nSEQUENCE:0\r\n\
         SUMMARY:Stand-up\r\nDTSTART:20260921T141320Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r\n\
         ORGANIZER:mailto:priya@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    ))
    .unwrap();
    let me = Address { name: None, email: "me@example.com".into() };

    let sent = invitations(&h)
        .answer(h.account_id, &invitation, &me, Answer::Yes, Scope::Series, NOW)
        .await
        .unwrap();
    assert_eq!(sent.told, Told::Calendar);
    assert!(
        h.fake.with(|s| s.answered_occurrences.is_empty()),
        "no answer went around the queue"
    );
    assert_eq!(queued(&h).await.len(), 1);

    copy.send(h.account_id).await.unwrap();
    assert_eq!(
        h.fake.with(|s| s.answered_events.clone()),
        [("primary".to_string(), "standup".to_string(), Answer::Yes, None)]
    );
}
