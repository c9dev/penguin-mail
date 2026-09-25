use mailrs_imap::{ImapError, UidSet};

use super::{days_ago, message};
use crate::fake::FakeImap;
use crate::services::ImapApi;
use crate::tests::{ImapHarness, fake_settings, imap_harness, imap_harness_on};
use crate::ImapSettings;

/// A message from one of the account's aliases, with a copy and a blind
/// copy.
fn outgoing() -> Vec<u8> {
    b"From: Me <alias@example.com>\r\nTo: Ann <ann@example.com>\r\nCc: bo@example.com\r\n\
      Bcc: cy@example.com\r\nSubject: Kites\r\nMessage-ID: <out@example.com>\r\n\r\nSunday?\r\n"
        .to_vec()
}

/// A draft to Ann that says `text`.
fn draft(text: &str) -> Vec<u8> {
    format!(
        "From: me@example.com\r\nTo: ann@example.com\r\nSubject: Plans\r\n\
         Message-ID: <draft-{text}@example.com>\r\n\r\n{text}\r\n"
    )
    .into_bytes()
}

/// The UIDs in `mailbox` on the server that are not marked deleted.
async fn undeleted(h: &ImapHarness, mailbox: &str) -> Vec<u32> {
    let mut uids = h.imap.search(mailbox, "UNDELETED").await.unwrap();
    uids.sort_unstable();
    uids
}

#[tokio::test]
async fn a_send_goes_from_the_chosen_identity_and_a_copy_is_filed_in_sent() {
    let h = imap_harness().await;
    h.bootstrap().await;

    let id = h.sync.send(outgoing(), None, None).await.unwrap();

    let submitted = h.smtp.sent();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].from, "alias@example.com");
    assert_eq!(
        submitted[0].to,
        ["ann@example.com", "bo@example.com", "cy@example.com"]
    );
    assert!(
        !String::from_utf8_lossy(&submitted[0].raw).contains("cy@example.com"),
        "no recipient sees the blind copy"
    );
    assert_eq!(id, "Sent/1002/1");
    let copy = h.imap.body("Sent", 1, "").await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&copy).contains("Bcc: cy@example.com"));
    let flags = h.imap.flags("Sent", &UidSet::from_uids([1]), None).await.unwrap();
    assert!(flags[0].flags.iter().any(|f| f == "\\Seen"));
}

#[tokio::test]
async fn a_server_that_files_what_it_sends_gets_no_copy() {
    let settings = ImapSettings {
        files_sent_mail: true,
        ..fake_settings()
    };
    let h = imap_harness_on(FakeImap::new(), settings).await;
    h.bootstrap().await;

    h.sync.send(outgoing(), None, None).await.unwrap();

    assert_eq!(h.smtp.sent().len(), 1);
    assert!(h.imap.search("Sent", "ALL").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_copy_that_fails_to_file_does_not_fail_the_send() {
    let h = imap_harness().await;
    h.bootstrap().await;
    h.imap.fail_next(ImapError::Network("the server went away".into()));

    let id = h.sync.send(outgoing(), None, None).await.unwrap();

    assert_eq!(id, "<out@example.com>");
    assert_eq!(h.smtp.sent().len(), 1, "sent once, and the outbox will not send again");
}

#[tokio::test]
async fn a_retry_finds_the_copy_already_in_sent() {
    let h = imap_harness().await;
    h.bootstrap().await;
    h.sync.send(outgoing(), None, None).await.unwrap();

    assert_eq!(
        h.sync.sent_copy(&outgoing()).await.unwrap().as_deref(),
        Some("Sent/1002/1")
    );
}

#[tokio::test]
async fn a_draft_is_saved_replaced_and_sent_from_the_drafts_mailbox() {
    let h = imap_harness().await;
    h.bootstrap().await;

    let first = h.sync.save_draft(draft("one"), None, None).await.unwrap();
    let second = h
        .sync
        .save_draft(draft("two"), None, Some(first.draft_id.clone()))
        .await
        .unwrap();

    assert_eq!(first.draft_id, "Drafts/1003/1");
    assert_eq!((second.draft_id.as_str(), second.message_id.as_str()), ("Drafts/1003/2", "Drafts/1003/2"));
    assert_eq!(undeleted(&h, "Drafts").await, [2]);
    let flags = h.imap.flags("Drafts", &UidSet::from_uids([2]), None).await.unwrap();
    assert!(flags[0].flags.iter().any(|f| f == "\\Draft"));

    h.sync.send_draft(&second.draft_id).await.unwrap();

    assert!(undeleted(&h, "Drafts").await.is_empty());
    let submitted = h.smtp.sent();
    assert!(String::from_utf8_lossy(&submitted[0].raw).contains("two"));
    assert_eq!(undeleted(&h, "Sent").await, [1], "the sent draft is filed in Sent");
}

#[tokio::test]
async fn a_draft_saved_elsewhere_is_found_by_its_id_and_deleted() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("Drafts", &message("d", "Plans", ""), &["\\Draft", "\\Seen"], days_ago(1));
    h.bootstrap().await;

    let draft_id = h.sync.draft_id_for("Drafts/1003/1").await.unwrap();
    assert_eq!(draft_id.as_deref(), Some("Drafts/1003/1"));
    h.sync.delete_draft("Drafts/1003/1").await.unwrap();

    assert!(undeleted(&h, "Drafts").await.is_empty());
}

/// Dovecot's first APPEND into a listed but unmade mailbox answers an
/// APPENDUID one UIDVALIDITY ahead of what SELECT reports, so the id the
/// caller gets must come from re-checking with a SELECT, not from taking
/// APPENDUID's number as given.
#[tokio::test]
async fn a_draft_saved_into_an_unmade_mailbox_keeps_the_uidvalidity_select_reports() {
    let imap = FakeImap::new();
    imap.with(|s| s.mailbox_mut("Drafts").unmade = true);
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;

    let saved = h.sync.save_draft(draft("one"), None, None).await.unwrap();

    let selected = h.imap.select("Drafts", None).await.unwrap().uidvalidity;
    assert_eq!(saved.draft_id, format!("Drafts/{selected}/1"));
    assert_eq!(undeleted(&h, "Drafts").await, [1]);
}

#[tokio::test]
async fn a_message_with_a_non_ascii_address_is_refused_when_the_server_cannot_carry_it() {
    let h = imap_harness().await;
    h.bootstrap().await;
    h.smtp.with(|s| s.smtp_utf8 = false);

    let raw = b"From: Me <alias@example.com>\r\nTo: Jos\xc3\xa9 <jos\xc3\xa9@example.com>\r\n\
        Subject: Kites\r\nMessage-ID: <accent@example.com>\r\n\r\nSunday?\r\n".to_vec();

    let err = h.sync.send(raw, None, None).await.unwrap_err();
    assert!(
        err.to_string().contains("cannot do that"),
        "the person sees a plain refusal, not the server's own words: {err}"
    );
    assert!(h.smtp.sent().is_empty(), "nothing went out");
}
