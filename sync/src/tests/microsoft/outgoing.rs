use mailrs_domain::query::{Query, Term};

use super::outlook;
use crate::fake::FakeMail;
use crate::{MailBackend, SearchQuery, now_millis};

const RAW: &[u8] = b"From: me@outlook.com\r\nTo: ann@example.com\r\nSubject: Hi\r\nMessage-ID: <abc@outlook.example>\r\n\r\nHello\r\n";

#[tokio::test]
async fn a_message_goes_out_as_written_and_graph_files_it() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let mail = &h.sync.services().mail;
    let id = mail.send(RAW, None).await.unwrap();
    assert_eq!(id, "abc@outlook.example");
    assert_eq!(h.fake.with(|s| s.sent.clone()), [RAW.to_vec()]);
    assert!(mail.find_sent("abc@outlook.example").await.unwrap().is_some());
}

#[tokio::test]
async fn saving_a_draft_twice_leaves_one() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let mail = &h.sync.services().mail;
    let first = mail.save_draft(None, RAW, None).await.unwrap();
    let second = mail.save_draft(Some(&first.draft_id), RAW, None).await.unwrap();
    assert_ne!(first.draft_id, second.draft_id);
    let drafts = mail.list_drafts().await.unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].draft_id, second.draft_id);
}

#[tokio::test]
async fn sending_a_draft_moves_it_to_sent() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let mail = &h.sync.services().mail;
    let saved = mail.save_draft(None, RAW, None).await.unwrap();
    let id = mail.send_draft(&saved.draft_id).await.unwrap();
    assert_eq!(id, "abc@outlook.example");
    assert!(mail.list_drafts().await.unwrap().is_empty());
    assert_eq!(h.fake.with(|s| s.sent.len()), 1);
}

#[tokio::test]
async fn a_search_goes_to_graph_as_kql() {
    let h = outlook().await;
    let inbox = h.fake.folder_id("inbox");
    let lunch = h.fake.deliver(&inbox, FakeMail { subject: "Lunch on Friday", at: now_millis(), ..FakeMail::default() });
    h.fake.deliver(&inbox, FakeMail { subject: "Invoice", at: now_millis(), ..FakeMail::default() });
    h.bootstrap_all().await;
    let found = h
        .sync
        .services()
        .mail
        .search(&SearchQuery::Tree(Query::term(Term::Subject("lunch".into()))), 10)
        .await
        .unwrap();
    assert_eq!(found.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), [lunch]);
}

fn large(protected: bool) -> Vec<u8> {
    let file = vec![b'x'; 3_500_000];
    let builder = mail_builder::MessageBuilder::new()
        .from("me@outlook.com")
        .to("ann@example.com")
        .subject("Photos")
        .message_id("big@outlook.example")
        .text_body("Here they are.")
        .attachment("image/jpeg", "beach.jpg", file);
    let raw = builder.write_to_vec().unwrap();
    match protected {
        false => raw,
        // A signed message: only its top-level type matters here.
        true => String::from_utf8_lossy(&raw)
            .replacen("Content-Type: multipart/mixed", "Content-Type: multipart/signed; protocol=\"application/pgp-signature\"", 1)
            .into_bytes(),
    }
}

#[tokio::test]
async fn a_large_message_goes_as_a_draft_with_its_file_uploaded() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let raw = large(false);
    let id = h.sync.services().mail.send(&raw, None).await.unwrap();
    assert_eq!(id, "big@outlook.example");
    let sent = h
        .fake
        .with(|s| s.messages.values().find(|m| m.folder == s.well_known["sentitems"]).cloned())
        .unwrap();
    assert_eq!(sent.message.internet_message_id.as_deref(), Some("<big@outlook.example>"));
    assert_eq!(sent.files.len(), 1);
    assert_eq!(sent.files[0].1.len(), 3_500_000);
    assert!(sent.raw.len() < 100_000, "the file went up as an upload, not inside the message");
    assert!(h.sync.services().mail.find_sent("big@outlook.example").await.unwrap().is_some());
}

#[tokio::test]
async fn a_large_signed_message_is_refused_with_a_reason() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let refused = h.sync.services().mail.send(&large(true), None).await;
    assert!(matches!(refused, Err(crate::BackendError::Refused(reason)) if reason.contains("3 MB")));
    assert!(h.fake.with(|s| s.sent.is_empty()));
}

#[tokio::test]
async fn an_unsayable_search_is_left_to_the_store() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let answer = h.sync.services().mail.search(&SearchQuery::Tree(Query::term(Term::Unread)), 10).await;
    assert!(matches!(answer, Err(crate::BackendError::Unsupported)));
}
