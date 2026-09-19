use mailrs_domain::ChangeEvent;
use mailrs_gmail::GmailError;

use super::harness;
use crate::TriageAction;
use crate::fake::meta;
use crate::now_millis;

#[tokio::test]
async fn archiving_applies_locally_and_remotely() {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 1000, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    h.sync
        .triage_thread("t1", &TriageAction::Archive)
        .await
        .unwrap();
    assert!(h.threads("INBOX").await.is_empty());
    assert_eq!(h.labels_of("a").await, ["UNREAD"]);
    assert_eq!(
        h.fake.with(|s| s.remote_writes.clone()),
        ["modify a + -INBOX", "modify b + -INBOX"]
    );
    assert!(!h.fake.with(|s| s.messages["a"].has_label("INBOX")));
}

#[tokio::test]
async fn a_refused_write_restores_the_thread() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.fail_next(GmailError::NeedsReauth);
    assert!(
        h.sync
            .triage_thread("t1", &TriageAction::Archive)
            .await
            .is_err()
    );
    assert_eq!(h.threads("INBOX").await, ["t1"]);
    assert!(
        h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::WriteFailed { .. }))
    );
}

#[tokio::test]
async fn transient_failures_are_retried() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.fail_next(GmailError::Network("down".into()));
    h.sync
        .triage_thread("t1", &TriageAction::Star)
        .await
        .unwrap();
    assert_eq!(h.labels_of("a").await, ["INBOX", "STARRED"]);
    assert_eq!(h.fake.with(|s| s.remote_writes.len()), 1);
}

#[tokio::test]
async fn trash_uses_the_trash_call() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.sync
        .triage_thread("t1", &TriageAction::Trash)
        .await
        .unwrap();
    assert_eq!(h.fake.with(|s| s.remote_writes.clone()), ["trash a"]);
    assert_eq!(h.labels_of("a").await, ["TRASH"]);
}
