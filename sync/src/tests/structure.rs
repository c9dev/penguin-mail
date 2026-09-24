//! The structure path: a message's parts from Gmail's `format=full`, with
//! its text and without its files, read by the same rules as a raw
//! message.

use mailrs_domain::{Attachment, MessageBody, Protection};
use mailrs_gmail::GmailError;

use super::harness;
use crate::api::GmailApi;
use crate::fake::meta;
use crate::{BackendError, MailBackend};

fn with_a_pdf() -> MessageBody {
    MessageBody {
        text: Some("See the plan".into()),
        html: Some("<p>See the plan</p>".into()),
        attachments: vec![Attachment {
            part_id: "x".into(),
            filename: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 3,
            attachment_id: Some("h1".into()),
            content_id: None,
        }],
        ..MessageBody::default()
    }
}

#[tokio::test]
async fn the_structure_gives_the_body_the_raw_message_gives() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let mail = &h.sync.services().mail;
    let from_structure = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let raw = mail.fetch_raw(&["m1".into()]).await.unwrap().remove(0).bytes;
    assert_eq!(from_structure, mailrs_mime::read(&raw));
    assert_eq!(h.fake.with(|s| s.usage.calls_to("users.messages.attachments.get")), 0);
}

#[tokio::test]
async fn an_invitation_sent_by_reference_arrives_with_its_calendar() {
    let h = harness().await;
    let ics = super::invitations::invite(0, "20260310T090000Z");
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.raws.insert("m1".into(), super::invitations::google_invitation(&ics));
    });
    let body = mailrs_mime::body(&h.sync.services().mail.fetch_structure("m1").await.unwrap());
    assert_eq!(body.calendar.as_deref(), Some(ics.as_str()));
    let files: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(files, ["invite.ics"]);
    // The calendar text, and not the file beside it.
    assert_eq!(h.fake.with(|s| s.usage.calls_to("users.messages.attachments.get")), 1);
}

#[tokio::test]
async fn a_file_fetched_after_its_structure_costs_one_call() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let mail = &h.sync.services().mail;
    let body = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let path = body.attachments[0].part_id.clone();
    assert_eq!(mail.fetch_part("m1", &path).await.unwrap(), vec![1, 2, 3]);
    assert_eq!(h.fake.with(|s| (s.structure_fetches, s.raw_fetches)), (1, 0));
    assert_eq!(h.fake.with(|s| s.usage.calls_to("users.messages.attachments.get")), 1);
}

#[tokio::test]
async fn a_file_fetched_cold_reads_the_structure_first() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let bytes = h
        .sync
        .services()
        .mail
        .fetch_part("m1", &crate::fake::attachment_path(0))
        .await
        .unwrap();
    assert_eq!(bytes, vec![1, 2, 3]);
    assert_eq!(h.fake.with(|s| (s.structure_fetches, s.raw_fetches)), (1, 0));
}

/// Gmail's attachment ids can change between fetches; a remembered one
/// that Gmail now refuses is retried once through a fresh structure
/// fetch rather than failing the whole read.
#[tokio::test]
async fn a_stale_handle_is_retried_through_a_fresh_structure_fetch() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let mail = &h.sync.services().mail;
    let body = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let path = body.attachments[0].part_id.clone();
    h.fake.fail_next(GmailError::NotFound);
    assert_eq!(mail.fetch_part("m1", &path).await.unwrap(), vec![1, 2, 3]);
    assert_eq!(h.fake.with(|s| s.structure_fetches), 2, "one retry, one fresh structure fetch");
}

/// A second refusal, right after the retry's own fresh structure fetch,
/// still surfaces as an error: the retry happens once, not in a loop.
#[tokio::test]
async fn a_second_refusal_after_the_retry_gives_up_rather_than_looping() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let mail = &h.sync.services().mail;
    let body = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let path = body.attachments[0].part_id.clone();
    h.fake.fail_next(GmailError::NotFound);
    h.fake.fail_next(GmailError::NotFound);
    let calls_before = h.fake.with(|s| s.usage.calls);
    assert!(matches!(mail.fetch_part("m1", &path).await, Err(BackendError::NotFound)));
    // The stale handle's own call, and the retry's fresh structure
    // fetch, which also fails: two calls, not a third attempt after it.
    assert_eq!(h.fake.with(|s| s.usage.calls), calls_before + 2, "one retry, not a loop");
}

/// A rate limit or a network error is not a stale handle: retrying it
/// through a fresh structure fetch would not help and only spends more
/// of the account's budget.
#[tokio::test]
async fn a_rate_limit_is_not_treated_as_a_stale_handle() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let mail = &h.sync.services().mail;
    let body = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let path = body.attachments[0].part_id.clone();
    h.fake.fail_next(GmailError::RateLimited { retry_after: None });
    assert!(matches!(mail.fetch_part("m1", &path).await, Err(BackendError::RateLimited(_))));
    assert_eq!(h.fake.with(|s| s.structure_fetches), 1, "no retry structure fetch");
}

#[tokio::test]
async fn a_network_error_is_not_treated_as_a_stale_handle() {
    let h = harness().await;
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.bodies.insert("m1".into(), with_a_pdf());
        s.attachments.insert(("m1".into(), "h1".into()), vec![1, 2, 3]);
    });
    let mail = &h.sync.services().mail;
    let body = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let path = body.attachments[0].part_id.clone();
    h.fake.fail_next(GmailError::Network("offline".into()));
    assert!(matches!(mail.fetch_part("m1", &path).await, Err(BackendError::Offline(_))));
    assert_eq!(h.fake.with(|s| s.structure_fetches), 1, "no retry structure fetch");
}

/// A forwarded message arrives as `message/rfc822`, which Gmail expands
/// with its own child parts rather than sending as one blob. The
/// structure path has to walk into it the way the raw path does, or a
/// file inside a forwarded message goes missing from the structure
/// path alone.
#[tokio::test]
async fn a_file_inside_a_forwarded_message_reads_the_same_on_both_paths() {
    let h = harness().await;
    let pdf = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b"PDF-BYTES")
    };
    let raw = format!(
        "Subject: Fwd: Report\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
         \r\n\
         --outer\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         See the forwarded message below.\r\n\
         --outer\r\n\
         Content-Type: text/html\r\n\
         \r\n\
         <p>See the forwarded message below.</p>\r\n\
         --outer\r\n\
         Content-Type: message/rfc822\r\n\
         \r\n\
         Subject: Report\r\n\
         Content-Type: application/pdf\r\n\
         Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {pdf}\r\n\
         --outer--\r\n"
    )
    .into_bytes();
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.raws.insert("m1".into(), raw);
    });
    let mail = &h.sync.services().mail;
    let from_structure = mailrs_mime::body(&mail.fetch_structure("m1").await.unwrap());
    let raw = mail.fetch_raw(&["m1".into()]).await.unwrap().remove(0).bytes;
    let from_raw = mailrs_mime::read(&raw);
    assert_eq!(from_structure, from_raw);
    let names: Vec<(&str, &str)> = from_raw
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.part_id.as_str()))
        .collect();
    assert_eq!(names, [("report.pdf", "3.1")]);
    assert_eq!(mail.fetch_part("m1", "3.1").await.unwrap(), b"PDF-BYTES");
}

/// Outlook sends an invitation's calendar object as `application/ics`,
/// not `text/calendar`. The structure path still has to fetch it by
/// reference, the way it already does for Gmail's own `text/calendar`
/// shape, or the invitation card never shows on such a message.
#[tokio::test]
async fn an_outlook_style_invitation_is_fetched_from_its_ics_file() {
    let h = harness().await;
    let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:abc@outlook.com\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let raw = format!(
        "Subject: Meeting\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
         \r\n\
         --outer\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         Please join\r\n\
         --outer\r\n\
         Content-Type: application/ics\r\n\
         Content-Disposition: attachment; filename=\"invite.ics\"\r\n\
         \r\n\
         {ics}\r\n\
         --outer--\r\n"
    )
    .into_bytes();
    h.fake.with(|s| {
        s.messages.insert("m1".into(), meta("m1", "t1", 1, &["INBOX"]));
        s.raws.insert("m1".into(), raw);
    });
    let body = mailrs_mime::body(&h.sync.services().mail.fetch_structure("m1").await.unwrap());
    assert!(body.calendar.as_deref().is_some_and(|c| c.contains("UID:abc@outlook.com")));
    let files: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(files, ["invite.ics"]);
}

/// A smoke test for the fake's protection wrappers: seeding a body's
/// `protection` and reading the raw message back shows the same wrapper.
#[tokio::test]
async fn a_seeded_protection_reads_back_on_the_raw_message() {
    let h = harness().await;
    for protection in Protection::ALL {
        h.fake.with(|s| {
            s.messages.insert("p1".into(), meta("p1", "t1", 1, &["INBOX"]));
            s.bodies.insert(
                "p1".into(),
                MessageBody {
                    text: Some("Meet at six.".into()),
                    protection: Some(protection),
                    ..MessageBody::default()
                },
            );
        });
        let raw = h.fake.raw_message("p1").await.unwrap();
        assert_eq!(mailrs_mime::read(&raw).protection, Some(protection), "{protection:?}");
    }
}
