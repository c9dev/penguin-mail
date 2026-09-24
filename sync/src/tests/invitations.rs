//! Opening an invitation, telling one version of an event from the next,
//! and answering one against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use mailrs_domain::Address;
use mailrs_domain::invitation::{Answer, Invitation, Scope, When};
use mailrs_gmail::GmailError;

use super::{Connected, Harness, harness};
use crate::invitations::{Change, Invitations, Told};

const UID: &str = "demo-event@google.com";

fn me() -> Address {
    Address {
        name: Some("Me".into()),
        email: "me@example.com".into(),
    }
}

fn read(ics: &str) -> Invitation {
    mailrs_domain::invitation::read(ics).expect("the part holds an event")
}

/// The one message the fake was asked to send, as text, with the base64
/// parts decoded so a test can read the calendar object in it.
fn sent_message(h: &Harness) -> String {
    let raw = h.fake.with(|s| {
        assert_eq!(s.sent.len(), 1, "one message went out");
        s.sent[0].0.clone()
    });
    let text = String::from_utf8(raw).expect("the message is text");
    let mut out = String::new();
    for block in text.split("\r\n\r\n") {
        out.push_str(block);
        out.push_str("\r\n\r\n");
        let packed: String = block.split("\r\n").collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&packed)
            && let Ok(decoded) = String::from_utf8(bytes)
        {
            out.push_str(&decoded);
        }
    }
    out
}

fn invitations(h: &Harness) -> Invitations<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Invitations::new(Arc::new(Connected(connected)), h.db.clone())
}

/// An invitation to one event, at `start` in UTC, numbered `sequence`.
pub(super) fn invite(sequence: i64, start: &str) -> String {
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
    let invitation = read(&invite(0, "20260310T090000Z"));
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();

    let sent = invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::Maybe,
            Scope::Series,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(sent.told, Told::Calendar);
    assert!(!sent.needs_permission);
    assert_eq!(
        h.fake.with(|s| s.calendar[UID]),
        Some(Answer::Maybe),
        "Google Calendar holds the answer"
    );
    assert!(
        h.fake.with(|s| s.sent.is_empty()),
        "Google tells the organizer, so no mail goes out"
    );

    let reopened = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.answer, Some(Answer::Maybe));
}

#[tokio::test]
async fn a_message_is_answered_from_the_invitation_it_carries() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.with(|s| {
        s.calendar.insert(UID.into(), None);
        s.bodies.insert(
            "m1".into(),
            mailrs_domain::MessageBody {
                calendar: Some(invite(0, "20260310T090000Z")),
                ..Default::default()
            },
        );
        s.bodies.insert(
            "m2".into(),
            mailrs_domain::MessageBody {
                calendar: Some(cancellation(1)),
                ..Default::default()
            },
        );
        s.bodies
            .insert("m3".into(), mailrs_domain::MessageBody::default());
    });

    let (invitation, sent) = invitations
        .answer_message(h.account_id, "m1", "me@example.com", Answer::Yes, 1_000)
        .await
        .unwrap()
        .expect("the message holds an invitation");
    assert_eq!(invitation.summary, "Design review");
    assert_eq!(sent.told, Told::Calendar);
    assert_eq!(h.fake.with(|s| s.calendar[UID]), Some(Answer::Yes));
    let reopened = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.answer, Some(Answer::Yes), "the card shows it too");

    for nothing in ["m2", "m3"] {
        assert!(
            invitations
                .answer_message(h.account_id, nothing, "me@example.com", Answer::Yes, 3_000)
                .await
                .unwrap()
                .is_none(),
            "{nothing} waits on no answer"
        );
    }
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
        .answer(
            h.account_id,
            &read(&invite(0, "20260310T090000Z")),
            &me(),
            Answer::Yes,
            Scope::Series,
            1_000,
        )
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
async fn an_event_on_no_calendar_is_answered_by_mail_to_the_organizer() {
    let h = harness().await;
    let invitations = invitations(&h);
    let invitation = read(&invite(0, "20260310T090000Z"));
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();

    let sent = invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::Yes,
            Scope::Series,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(sent.told, Told::Organizer);

    let message = sent_message(&h);
    assert!(
        message.contains("To: \"Priya\" <priya@example.com>"),
        "{message}"
    );
    assert!(
        message.contains("Subject: Accepted: Design review"),
        "{message}"
    );
    // RFC 6047 wants the method on the part as well as in the object.
    assert!(message.contains("method=\"REPLY\""), "{message}");
    assert!(message.contains("METHOD:REPLY\r\n"), "{message}");
    assert!(message.contains(&format!("UID:{UID}\r\n")), "{message}");
    assert!(
        message.contains("ATTENDEE;PARTSTAT=ACCEPTED;CN=Me:mailto:me@example.com\r\n"),
        "{message}"
    );

    let reopened = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.answer, Some(Answer::Yes), "the answer stands");
}

#[tokio::test]
async fn an_invitation_with_no_organizer_has_nobody_to_answer() {
    let h = harness().await;
    let invitations = invitations(&h);
    let invitation = read(
        &[
            "BEGIN:VCALENDAR",
            "METHOD:REQUEST",
            "BEGIN:VEVENT",
            &format!("UID:{UID}"),
            "SUMMARY:Design review",
            "DTSTART:20260310T090000Z",
            "END:VEVENT",
            "END:VCALENDAR",
            "",
        ]
        .join("\r\n"),
    );
    invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 1_000)
        .await
        .unwrap();

    let sent = invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::Yes,
            Scope::Series,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(sent.told, Told::Nobody);
    assert!(h.fake.with(|s| s.sent.is_empty()));

    let reopened = invitations
        .open(h.account_id, "m1", &invite(0, "20260310T090000Z"), 2_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.answer, None, "nothing was answered");
}

/// A project with the Calendar API switched off refuses outright. The
/// answer still goes by mail, and the window says what to turn on.
#[tokio::test]
async fn a_calendar_api_switched_off_still_reaches_the_organizer() {
    let h = harness().await;
    let invitations = invitations(&h);
    let invitation = read(&invite(0, "20260310T090000Z"));
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    h.fake.fail_next(GmailError::ApiDisabled {
        service: "Google Calendar API".into(),
        enable_url: "https://console.developers.google.com/apis/api/calendar-json.googleapis.com"
            .into(),
    });

    let sent = invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::Yes,
            Scope::Series,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(sent.told, Told::Organizer);
    assert!(!sent.needs_permission, "a permission would not help");
    let off = sent.api_off.expect("the window says what to turn on");
    assert_eq!(off.service, "Google Calendar API");
    assert!(sent_message(&h).contains("PARTSTAT=ACCEPTED"));
}

#[tokio::test]
async fn a_missing_calendar_permission_still_reaches_the_organizer() {
    let h = harness().await;
    let invitations = invitations(&h);
    let invitation = read(&invite(0, "20260310T090000Z"));
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    h.fake.fail_next(GmailError::MissingScope);

    let sent = invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::No,
            Scope::Series,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(sent.told, Told::Organizer);
    assert!(sent.needs_permission, "the window offers to ask for it");
    assert!(sent_message(&h).contains("PARTSTAT=DECLINED"));

    h.fake.fail_next(GmailError::Network("offline".into()));
    let err = invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::Yes,
            Scope::Series,
            2_000,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("offline"), "{err}");
}

/// An invitation at a fixed hour, for the questions about what else the
/// user has on then.
fn at_ten() -> String {
    [
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        &format!("UID:{UID}"),
        "SUMMARY:Design review",
        "DTSTART:20260310T100000Z",
        "DTEND:20260310T110000Z",
        "ORGANIZER;CN=Priya:mailto:priya@example.com",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ]
    .join("\r\n")
}

/// 10:30 to 11:30 on the day `at_ten` runs, in milliseconds.
const CLASH: (i64, i64) = (1_773_138_600_000, 1_773_142_200_000);

#[tokio::test]
async fn an_invitation_over_something_else_says_what() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake
        .with(|s| s.busy.push((CLASH.0, CLASH.1, "Design crit".into())));
    let invitation = read(&at_ten());

    assert_eq!(
        invitations.busy(h.account_id, &invitation).await.unwrap(),
        vec!["Design crit".to_string()]
    );

    // The window opens the same message again and again; Google hears
    // about it once.
    invitations.busy(h.account_id, &invitation).await.unwrap();
    invitations.busy(h.account_id, &invitation).await.unwrap();
    assert_eq!(h.fake.with(|s| s.usage.calls_to("calendar.events.list")), 1);
}

#[tokio::test]
async fn an_hour_with_nothing_in_it_says_nothing() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.with(|s| {
        s.busy.push((
            CLASH.0 + 86_400_000,
            CLASH.1 + 86_400_000,
            "Design crit".into(),
        ))
    });
    assert!(
        invitations
            .busy(h.account_id, &read(&at_ten()))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_calendar_nobody_may_read_says_nothing_about_clashes() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.fail_next(GmailError::MissingScope);
    assert!(
        invitations
            .busy(h.account_id, &read(&at_ten()))
            .await
            .unwrap()
            .is_empty()
    );
}

/// One occurrence of a weekly event, as an organizer sends when they move
/// or re-ask about a single Tuesday.
fn one_of_a_series() -> String {
    [
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        &format!("UID:{UID}"),
        "SEQUENCE:2",
        "RECURRENCE-ID:20260310T090000Z",
        "SUMMARY:Stand-up",
        "DTSTART:20260310T090000Z",
        "DTEND:20260310T093000Z",
        "ORGANIZER;CN=Priya:mailto:priya@example.com",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ]
    .join("\r\n")
}

/// 10 March 2026 at 09:00 UTC, the occurrence `one_of_a_series` names.
const OCCURRENCE: i64 = 1_773_133_200_000;

#[tokio::test]
async fn google_hears_which_occurrence_an_answer_is_for() {
    let h = harness().await;
    let invitations = invitations(&h);
    h.fake.with(|s| s.calendar.insert(UID.into(), None));
    let invitation = read(&one_of_a_series());

    for (scope, named) in [(Scope::Occurrence, Some(OCCURRENCE)), (Scope::Series, None)] {
        invitations
            .answer(h.account_id, &invitation, &me(), Answer::Yes, scope, 1_000)
            .await
            .unwrap();
        assert_eq!(
            h.fake.with(|s| s.answered_occurrences.last().copied()),
            Some(named)
        );
    }
}

#[tokio::test]
async fn a_mailed_reply_names_the_occurrence_it_answers() {
    let h = harness().await;
    let invitations = invitations(&h);
    let invitation = read(&one_of_a_series());

    invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::No,
            Scope::Occurrence,
            1_000,
        )
        .await
        .unwrap();
    assert!(sent_message(&h).contains("RECURRENCE-ID:20260310T090000Z\r\n"));

    h.fake.with(|s| s.sent.clear());
    invitations
        .answer(
            h.account_id,
            &invitation,
            &me(),
            Answer::No,
            Scope::Series,
            2_000,
        )
        .await
        .unwrap();
    assert!(!sent_message(&h).contains("RECURRENCE-ID"));
}

#[tokio::test]
async fn a_proposal_asks_the_organizer_for_another_time() {
    let h = harness().await;
    let invitations = invitations(&h);
    // The same meeting, four hours later.
    let when = When::At {
        starts_at: 1_773_151_200_000,
        ends_at: Some(1_773_154_800_000),
    };

    let told = invitations
        .propose(
            h.account_id,
            &read(&at_ten()),
            &me(),
            &when,
            Scope::Series,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(told, Told::Organizer);

    let message = sent_message(&h);
    assert!(
        message.contains("Subject: New Time Proposed: Design review"),
        "{message}"
    );
    assert!(message.contains("method=\"COUNTER\""), "{message}");
    assert!(message.contains("METHOD:COUNTER\r\n"), "{message}");
    assert!(
        message.contains("DTSTART:20260310T140000Z\r\n"),
        "{message}"
    );
    assert!(message.contains("DTEND:20260310T150000Z\r\n"), "{message}");
    assert!(message.contains(&format!("UID:{UID}\r\n")), "{message}");
    // A proposal settles nothing, so no calendar hears about it.
    assert!(h.fake.with(|s| s.answered_occurrences.is_empty()));
}

#[tokio::test]
async fn one_occurrence_says_how_many_of_its_series_are_left() {
    let h = harness().await;
    let invitations = invitations(&h);
    let week = 7 * 86_400_000;
    h.fake.with(|s| {
        let starts = (0..10).map(|n| OCCURRENCE + n * week).collect();
        s.series
            .insert(UID.into(), ("FREQ=WEEKLY;BYDAY=TU;COUNT=10".into(), starts));
    });
    let invitation = read(&one_of_a_series());

    // Asked a moment after the fourth Tuesday, six are still to come.
    let now = OCCURRENCE + 3 * week + 1;
    assert_eq!(
        invitations
            .series(h.account_id, &invitation, now)
            .await
            .unwrap()
            .as_deref(),
        Some("Every Tuesday, 6 left")
    );
}

#[tokio::test]
async fn a_series_the_calendar_cannot_give_leaves_the_card_alone() {
    let h = harness().await;
    let invitations = invitations(&h);
    let invitation = read(&one_of_a_series());

    // Not on the calendar.
    assert_eq!(
        invitations
            .series(h.account_id, &invitation, OCCURRENCE)
            .await
            .unwrap(),
        None
    );
    // On it, but the account never granted the calendar.
    h.fake.with(|s| {
        s.series
            .insert(UID.into(), ("FREQ=WEEKLY".into(), vec![OCCURRENCE]))
    });
    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        invitations
            .series(h.account_id, &invitation, OCCURRENCE)
            .await
            .unwrap(),
        None
    );
    // Readable, and open-ended.
    assert_eq!(
        invitations
            .series(h.account_id, &invitation, OCCURRENCE)
            .await
            .unwrap()
            .as_deref(),
        Some("Every week, no end date")
    );
}

#[tokio::test]
async fn an_invitation_to_a_whole_event_asks_the_calendar_nothing() {
    let h = harness().await;
    let invitations = invitations(&h);
    assert_eq!(
        invitations
            .series(h.account_id, &read(&at_ten()), OCCURRENCE)
            .await
            .unwrap(),
        None
    );
    assert_eq!(h.fake.with(|s| s.usage.calls_to("calendar.events.list")), 0);
}

/// Gmail's API sends Google Calendar's invitation part by attachment id,
/// with no text inline, so the card had nothing to read on a real
/// account. Fetching a body fetches that part too.
#[tokio::test]
async fn a_calendar_part_sent_by_attachment_id_is_fetched_with_the_body() {
    use crate::MailBackend;
    let h = harness().await;
    let ics = invite(0, "20260310T090000Z");
    h.fake.with(|s| {
        s.bodies.insert(
            "m1".into(),
            mailrs_domain::MessageBody {
                html: Some("<p>Invitation</p>".into()),
                attachments: vec![mailrs_domain::Attachment {
                    part_id: "1".into(),
                    filename: "invite.ics".into(),
                    mime_type: "application/ics".into(),
                    size: ics.len() as i64,
                    attachment_id: Some("file-ics".into()),
                    content_id: None,
                }],
                ..Default::default()
            },
        );
        s.attachments
            .insert(("m1".into(), "file-ics".into()), ics.clone().into_bytes());
    });
    let body = h.sync.services().mail.message_body("m1").await.unwrap();
    assert_eq!(body.calendar.as_deref(), Some(ics.as_str()));
}

/// Google Calendar's invitation as Gmail's `format=full` sends it: every
/// named part by reference, the calendar text among them. Twin of
/// `mime::tests::read::google_invitation`, which builds the same
/// message for the raw-message tests; keep the two in step.
pub(super) fn google_invitation(ics: &str) -> Vec<u8> {
    format!(
        "From: Ann <ann@example.com>\r\n\
         To: me@example.com\r\n\
         Subject: Invitation: Design crit\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
         \r\n\
         --outer\r\n\
         Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
         \r\n\
         --inner\r\n\
         Content-Type: text/plain; charset=\"UTF-8\"\r\n\
         \r\n\
         You have been invited\r\n\
         --inner\r\n\
         Content-Type: text/html; charset=\"UTF-8\"\r\n\
         \r\n\
         <p>You have been invited</p>\r\n\
         --inner\r\n\
         Content-Type: text/calendar; charset=\"UTF-8\"; method=REQUEST\r\n\
         Content-Disposition: inline; filename=\"invite.ics\"\r\n\
         \r\n\
         {ics}\r\n\
         --inner--\r\n\
         --outer\r\n\
         Content-Type: application/ics; name=\"invite.ics\"\r\n\
         Content-Disposition: attachment; filename=\"invite.ics\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {b64}\r\n\
         --outer--\r\n",
        b64 = base64::engine::general_purpose::STANDARD.encode(ics),
    )
    .into_bytes()
}
