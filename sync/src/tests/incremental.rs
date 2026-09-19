use mailrs_domain::ChangeEvent;
use mailrs_gmail::GmailError;

use super::harness;
use crate::fake::meta;
use crate::now_millis;

const DAY: i64 = 24 * 60 * 60 * 1000;

#[tokio::test]
async fn new_inbox_mail_is_stored_and_announced() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("old", "told", now - DAY, &["INBOX"]));
    h.bootstrap_all().await;
    h.fake
        .deliver(meta("new", "tnew", now, &["INBOX", "UNREAD"]));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("INBOX").await, ["tnew", "told"]);
    assert_eq!(h.cursor().await.history_id, Some(101));
    assert!(h.drain().contains(&ChangeEvent::NewMail {
        account_id: 1,
        message_ids: vec!["new".into()]
    }));
}

#[tokio::test]
async fn sent_mail_is_stored_without_an_announcement() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.deliver(meta("s", "ts", now_millis(), &["SENT"]));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("SENT").await, ["ts"]);
    assert!(
        !h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::NewMail { .. }))
    );
}

#[tokio::test]
async fn label_changes_and_deletions_apply() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "ta", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "tb", now - 1000, &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.remote_relabel("a", &[], &["INBOX", "UNREAD"]);
    h.fake.remote_delete("b");
    h.sync.incremental().await.unwrap();
    assert!(h.threads("INBOX").await.is_empty());
    assert!(h.labels_of("a").await.is_empty());
    assert!(!h.thread("ta").await.unwrap().unread);
    assert!(h.thread("tb").await.is_none());
}

#[tokio::test]
async fn old_mail_moved_into_the_inbox_is_fetched() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake
        .seed_outside_window(meta("old", "told", now_millis() - 90 * DAY, &[]));
    h.fake.remote_relabel("old", &["INBOX"], &[]);
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("INBOX").await, ["told"]);
}

#[tokio::test]
async fn every_history_page_is_applied() {
    let h = harness().await;
    h.bootstrap_all().await;
    let now = now_millis();
    for i in 0..5 {
        h.fake.deliver(meta(
            &format!("m{i}"),
            &format!("t{i}"),
            now - i,
            &["INBOX"],
        ));
    }
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("INBOX").await.len(), 5);
    assert_eq!(h.cursor().await.history_id, Some(105));
}

#[tokio::test]
async fn expired_history_bootstraps_again_and_sweeps_stale_mail() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("keep", "tkeep", now, &["INBOX"]));
    h.fake.seed(meta("gone", "tgone", now - 1000, &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.remote_delete_silently("gone");
    h.fake.expire_history();
    h.fake.with(|s| s.page_size = 1000);
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("INBOX").await, ["tkeep"]);
    let cursor = h.cursor().await;
    assert_eq!(cursor.history_id, Some(101));
    assert!(cursor.backfill_done);
}

#[tokio::test]
async fn incremental_without_a_cursor_bootstraps() {
    let h = harness().await;
    h.fake.seed(meta("a", "ta", now_millis(), &["INBOX"]));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("INBOX").await, ["ta"]);
}

#[tokio::test]
async fn a_gmail_failure_leaves_the_cursor_alone() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.deliver(meta("a", "ta", now_millis(), &["INBOX"]));
    h.fake.fail_next(GmailError::Network("down".into()));
    assert!(h.sync.incremental().await.is_err());
    assert_eq!(h.cursor().await.history_id, Some(100));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads("INBOX").await, ["ta"]);
}
