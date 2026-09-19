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

#[tokio::test]
async fn one_message_can_be_triaged_alone() {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 1000, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    h.sync
        .triage_message("t1", "b", &TriageAction::MarkRead)
        .await
        .unwrap();
    assert_eq!(h.labels_of("a").await, ["INBOX", "UNREAD"]);
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
    assert_eq!(
        h.fake.with(|s| s.remote_writes.clone()),
        ["modify b + -UNREAD"]
    );
    assert!(
        h.thread("t1").await.unwrap().unread,
        "the thread still has an unread message"
    );
}

#[tokio::test]
async fn trash_and_junk_can_be_undone() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.sync
        .triage_thread("t1", &TriageAction::Trash)
        .await
        .unwrap();
    h.sync
        .triage_thread("t1", &TriageAction::Trash.inverse())
        .await
        .unwrap();
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
    h.sync
        .triage_thread("t1", &TriageAction::Junk)
        .await
        .unwrap();
    assert_eq!(h.labels_of("a").await, ["SPAM"]);
    h.sync
        .triage_thread("t1", &TriageAction::Junk.inverse())
        .await
        .unwrap();
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
    assert_eq!(
        h.fake.with(|s| s.remote_writes.clone()),
        [
            "trash a",
            "untrash a",
            "modify a +SPAM -INBOX",
            "modify a +INBOX -SPAM"
        ]
    );
}

#[test]
fn every_action_has_an_inverse_that_restores_labels() {
    let actions = [
        TriageAction::Archive,
        TriageAction::MarkRead,
        TriageAction::Star,
        TriageAction::AddLabel("L".into()),
        TriageAction::Trash,
        TriageAction::Junk,
        TriageAction::Relabel {
            add: vec!["A".into()],
            remove: vec!["B".into()],
        },
    ];
    for action in actions {
        let (add, remove) = action.label_delta();
        let (back_add, back_remove) = action.inverse().label_delta();
        assert_eq!((add, remove), (back_remove, back_add), "{action:?}");
    }
}

#[tokio::test]
async fn triage_fetches_a_thread_it_has_not_stored() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.seed_outside_window(meta(
        "old",
        "t9",
        now_millis() - 90 * 86_400_000,
        &["TRASH"],
    ));
    h.sync
        .triage_thread("t9", &TriageAction::Untrash)
        .await
        .unwrap();
    assert_eq!(h.labels_of("old").await, ["INBOX"]);
    assert_eq!(h.fake.with(|s| s.remote_writes.clone()), ["untrash old"]);
}
