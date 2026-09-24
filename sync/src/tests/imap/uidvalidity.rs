//! A mailbox the server renumbered, as a new UIDVALIDITY says: the engine
//! lists that mailbox again and keeps what it already held.

use mailrs_imap::ImapError;
use mailrs_store::bodies;

use super::{days_ago, message};
use crate::tests::imap_harness;

#[tokio::test]
async fn a_new_uidvalidity_relists_the_mailbox_and_keeps_ids_and_bodies() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(3));
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(2));
    h.imap.deliver_flagged("INBOX", &message("c", "Ferns", ""), &[], days_ago(1));
    h.imap.deliver_flagged("Sent", &message("s", "Re: Kites", ""), &["\\Seen"], days_ago(1));
    h.bootstrap().await;
    h.sync.body("INBOX/1001/1").await.unwrap();
    h.imap.remote_expunge("INBOX", 2);
    h.imap.reset_uidvalidity("INBOX");

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/3", "Sent/1002/1"]);
    assert_eq!(h.location("INBOX/1001/1").await.as_deref(), Some("INBOX/1007/1"));
    assert_eq!(h.location("INBOX/1001/3").await.as_deref(), Some("INBOX/1007/2"));
    assert_eq!(
        h.location("Sent/1002/1").await.as_deref(),
        Some("Sent/1002/1"),
        "other mailboxes keep their names"
    );
    let account_id = h.account_id;
    let cached = h
        .db
        .read(move |c| bodies::peek_body(c, account_id, "INBOX/1001/1"))
        .await
        .unwrap();
    assert!(cached.is_some(), "the cached body survives");

    h.imap.set_flags("INBOX", 1, &["\\Flagged"]);
    h.sync.incremental().await.unwrap();
    assert!(h.stored("INBOX/1001/1").await.unwrap().is_flagged());
}

#[tokio::test]
async fn a_message_without_a_message_id_comes_back_as_new() {
    let h = imap_harness().await;
    let bare = b"From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: Bare\r\n\r\nHi.\r\n";
    h.imap.deliver_flagged("INBOX", bare, &[], days_ago(1));
    h.bootstrap().await;
    h.imap.reset_uidvalidity("INBOX");

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1007/1"]);
}

/// A relisting that fails part-way leaves the sync state where it was, so
/// the next look lists the mailbox again and still keeps every id.
#[tokio::test]
async fn a_relisting_that_fails_runs_again_at_the_next_look() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(2));
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(1));
    h.bootstrap().await;
    h.imap.reset_uidvalidity("INBOX");
    h.imap.fail_on("headers", ImapError::Network("reset".into()));

    assert!(h.sync.incremental().await.is_err());
    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/2"]);
    assert_eq!(h.location("INBOX/1001/2").await.as_deref(), Some("INBOX/1007/2"));
}
