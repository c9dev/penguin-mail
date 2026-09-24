//! The structure path: a message's parts from Gmail's `format=full`, with
//! its text and without its files, read by the same rules as a raw
//! message.

use mailrs_domain::{Attachment, MessageBody};

use super::harness;
use crate::MailBackend;
use crate::fake::meta;

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
    h.fake.fail_next(mailrs_gmail::GmailError::NotFound);
    assert_eq!(mail.fetch_part("m1", &path).await.unwrap(), vec![1, 2, 3]);
    assert_eq!(h.fake.with(|s| s.structure_fetches), 2, "one retry, one fresh structure fetch");
}
