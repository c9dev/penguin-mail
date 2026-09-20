//! Opening an invitation, telling one version of an event from the next,
//! and answering one against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::invitation::Answer;
use mailrs_gmail::{Answered, GmailError};

use super::{Connected, Harness, harness};
use crate::invitations::{Change, Invitations};
use crate::settings::Permitted;

const UID: &str = "demo-event@google.com";

fn invitations(h: &Harness) -> Invitations<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Invitations::new(Arc::new(Connected(connected)), h.db.clone())
}

/// An invitation to one event, at `start` in UTC, numbered `sequence`.
fn invite(sequence: i64, start: &str) -> String {
    [
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        &format!("UID:{UID}"),
        &format!("SEQUENCE:{sequence}"),
        "SUMMARY:Design review",
        &format!("DTSTART:{start}"),
        "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Me:mailto:me@example.com",
        "ORGANIZER;CN=Priya:mailto:priya@example.com",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ]
    .join("\r\n")
}

fn cancellation(sequence: i64) -> String {
    [
        "BEGIN:VCALENDAR",
        "METHOD:CANCEL",
        "BEGIN:VEVENT",
        &format!("UID:{UID}"),
        &format!("SEQUENCE:{sequence}"),
        "STATUS:CANCELLED",
        "SUMMARY:Design review",
        "DTSTART:20260310T090000Z",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ]
    .join("\r\n")
}

#[tokio::test]
async fn the_first_invitation_changes_nothing_and_has_no_answer() {
    let h = harness().await;
    let opened = invitations(&h)
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap()
        .expect("the part holds an event");
    assert_eq!(opened.invitation.summary, "Design review");
    assert_eq!(opened.change, None);
    assert_eq!(opened.answer, None);
}

#[tokio::test]
async fn a_part_with_no_event_gives_nothing_back() {
    let h = harness().await;
    assert_eq!(
        invitations(&h)
            .open(h.account_id, "m1", "not a calendar", 1_000)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_later_version_says_the_meeting_moved() {
    let h = harness().await;
    let invitations = invitations(&h);
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();
    let was = mailrs_domain::invitation::read(&invite(0, "20260310T090000Z"))
        .unwrap()
        .when
        .unwrap()
        .starts_at()
        .unwrap();

    let opened = invitations
        .open(h.account_id, "m2", &invite(2, "20260311T140000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        opened.change,
        Some(Change::Moved {
            was,
            all_day: false
        })
    );

    // Reading the update again says the same thing. The window reads a
    // message twice on the way in, once from the store and once when the
    // body lands, and the second reading must not wipe the line.
    let reread = invitations
        .open(h.account_id, "m2", &invite(2, "20260311T140000Z"), 2_500)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reread.change, opened.change);

    // Opening the older message again says nothing new: what changed after
    // it arrived is not its doing.
    let again = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 3_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.change, None);
}

#[tokio::test]
async fn a_cancellation_says_so_however_often_it_is_read() {
    let h = harness().await;
    let invitations = invitations(&h);
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();
    for at in [2_000, 2_500, 3_000] {
        let opened = invitations
            .open(h.account_id, "m2", &cancellation(3), at)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(opened.change, Some(Change::Cancelled), "read at {at}");
    }
}

#[tokio::test]
async fn a_later_version_at_the_same_time_only_says_it_changed() {
    let h = harness().await;
    let invitations = invitations(&h);
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();
    let opened = invitations
        .open(h.account_id, "m2", &invite(1, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(opened.change, Some(Change::Updated));
}

#[tokio::test]
async fn a_cancellation_of_a_known_event_says_so() {
    let h = harness().await;
    let invitations = invitations(&h);
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();
    let opened = invitations
        .open(h.account_id, "m2", &cancellation(3), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(opened.change, Some(Change::Cancelled));
    assert!(opened.invitation.cancelled());
}

#[tokio::test]
async fn an_answer_reaches_the_calendar_and_comes_back_on_reopening() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();

    let sent = invitations
        .answer(h.account_id, UID, "me@example.com", Answer::Maybe)
        .await
        .unwrap();
    assert_eq!(sent, Permitted::Done(Answered::Done));
    assert_eq!(
        h.fake.with(|s| s.calendar[UID]),
        Some(Answer::Maybe),
        "Google Calendar holds the answer"
    );

    let reopened = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.answer, Some(Answer::Maybe));
}

#[tokio::test]
async fn a_newer_version_asks_again() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();
    invitations
        .answer(h.account_id, UID, "me@example.com", Answer::Yes)
        .await
        .unwrap();

    let updated = invitations
        .open(h.account_id, "m2", &invite(4, "20260311T140000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.answer, None, "the organizer changed the meeting");
}

#[tokio::test]
async fn an_event_google_never_put_on_the_calendar_is_not_answered() {
    let h = harness().await;
    let invitations = invitations(&h);
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();
    assert_eq!(
        invitations
            .answer(h.account_id, UID, "me@example.com", Answer::Yes)
            .await
            .unwrap(),
        Permitted::Done(Answered::NotOnCalendar)
    );
    let reopened = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.answer, None, "nothing was answered");
}

#[tokio::test]
async fn a_missing_calendar_permission_is_its_own_answer() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        invitations
            .answer(h.account_id, UID, "me@example.com", Answer::Yes)
            .await
            .unwrap(),
        Permitted::NeedsPermission
    );

    h.fake.fail_next(GmailError::Network("offline".into()));
    let err = invitations
        .answer(h.account_id, UID, "me@example.com", Answer::Yes)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("offline"), "{err}");
}
