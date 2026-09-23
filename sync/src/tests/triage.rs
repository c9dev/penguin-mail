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
async fn a_write_that_fails_part_way_keeps_what_gmail_took() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 1000, &["INBOX"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let mut first = h.fake.hold("users.messages.modify");
    let archiving = h.sync.triage_thread("t1", &TriageAction::Archive);
    // Gmail archives the first message and refuses the second.
    let refusing = async {
        first.entered().await;
        let mut second = h.fake.hold("users.messages.modify");
        first.release();
        second.entered().await;
        h.fake.fail_next(GmailError::NeedsReauth);
        second.release();
    };
    let (archived, ()) = tokio::join!(archiving, refusing);
    assert!(archived.is_err());
    assert!(!h.fake.with(|s| s.messages["a"].has_label("INBOX")));
    assert_eq!(h.labels_of("a").await, Vec::<String>::new());
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
}

#[tokio::test]
async fn a_failed_write_undoes_only_its_own_change() {
    let h = harness().await;
    h.fake
        .seed(meta("a", "t1", now_millis(), &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let mut held = h.fake.hold("users.messages.modify");
    let reading = h.sync.triage_thread("t1", &TriageAction::MarkRead);
    // While the write waits, the message is archived elsewhere and a
    // replay stores that. Then Gmail refuses the write.
    let meanwhile = async {
        held.entered().await;
        h.fake.remote_relabel("a", &[], &["INBOX"]);
        h.sync.incremental().await.unwrap();
        h.fake.fail_next(GmailError::NeedsReauth);
        held.release();
    };
    let (read, ()) = tokio::join!(reading, meanwhile);
    assert!(read.is_err());
    assert_eq!(h.labels_of("a").await, ["UNREAD"]);
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

#[tokio::test(start_paused = true)]
async fn a_rate_limited_write_waits_out_gmails_retry_after() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.fail_next(GmailError::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(20)),
    });
    let started = tokio::time::Instant::now();

    h.sync
        .triage_thread("t1", &TriageAction::Archive)
        .await
        .unwrap();

    // Gmail asked for 20 seconds and got them, give or take the jitter
    // that keeps several accounts from returning on the same tick. The
    // plain backoff would have waited one second.
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(16),
        "waited {waited:?}"
    );
    assert!(
        waited <= std::time::Duration::from_secs(24),
        "waited {waited:?}"
    );
    assert_eq!(h.threads("INBOX").await, Vec::<String>::new());
}

#[tokio::test]
async fn a_run_of_rate_limits_is_waited_out_rather_than_failed() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    // Three 429s in a row, which used to be one more than the action had
    // attempts for, and it gave the user a failure to repeat by hand.
    for _ in 0..3 {
        h.fake
            .fail_next(GmailError::RateLimited { retry_after: None });
    }

    h.sync
        .triage_thread("t1", &TriageAction::Trash)
        .await
        .unwrap();

    assert!(h.threads("INBOX").await.is_empty());
    let told: Vec<String> = h
        .drain()
        .into_iter()
        .filter_map(|e| match e {
            ChangeEvent::WaitingOnGmail { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        told,
        ["Gmail is busy. Still working on 1 conversation."],
        "the window is told once, not once per retry"
    );
}

#[tokio::test]
async fn a_rate_limit_that_outlasts_the_ceiling_reports_plainly() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let sync = h.sync_with(std::time::Duration::from_millis(60));
    for _ in 0..10 {
        h.fake.fail_next(GmailError::RateLimited {
            retry_after: Some(std::time::Duration::from_millis(50)),
        });
    }

    let err = sync
        .triage_thread("t1", &TriageAction::Trash)
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        crate::SyncError::Backend(crate::BackendError::RateLimited(_))
    ));
    let told: Vec<String> = h
        .drain()
        .into_iter()
        .filter_map(|e| match e {
            ChangeEvent::WriteFailed { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        told,
        ["Gmail stayed busy for a moment, so move to trash did not go through for 1 conversation."]
    );
    assert_eq!(h.threads("INBOX").await, ["t1"], "the thread comes back");
}

/// A few messages go to Gmail a call each and many in one batch. Either
/// way Gmail must end up with the labels the store shows, so taking mail
/// out of the trash puts it back in the inbox whatever the count.
#[tokio::test]
async fn a_few_messages_and_many_leave_the_trash_the_same_way() {
    for count in [3, 12] {
        let h = harness().await;
        let old = now_millis() - 90 * 86_400_000;
        let targets: Vec<mailrs_domain::Target> = (0..count)
            .map(|i| {
                let (id, thread) = (format!("m{i}"), format!("t{i}"));
                h.fake.seed(meta(&id, &thread, old, &["TRASH"]));
                mailrs_domain::Target::thread(h.account_id, thread)
            })
            .collect();
        h.bootstrap_all().await;

        h.sync
            .triage_all(&targets, &TriageAction::Untrash)
            .await
            .unwrap();

        for i in 0..count {
            let id = format!("m{i}");
            assert_eq!(h.labels_of(&id).await, ["INBOX"], "{count}: the store");
            assert_eq!(
                h.fake.with(|s| s.messages[&id].label_ids.clone()),
                ["INBOX"],
                "{count}: Gmail"
            );
        }
    }
}

#[tokio::test]
async fn trash_sends_its_labels_as_a_batch_would() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.sync
        .triage_thread("t1", &TriageAction::Trash)
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.remote_writes.clone()),
        ["modify a +TRASH -INBOX"]
    );
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
    let target = mailrs_domain::Target {
        message_id: Some("b".into()),
        ..mailrs_domain::Target::thread(h.account_id, "t1")
    };
    let changed = h
        .sync
        .triage_all(&[target], &TriageAction::MarkRead)
        .await
        .unwrap();
    assert_eq!(
        changed,
        [crate::Relabelled {
            thread_id: "t1".into(),
            message_id: "b".into(),
            added: vec![],
            removed: vec!["UNREAD".into()],
        }]
    );
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
async fn triage_fetches_a_thread_it_has_not_stored() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.seed(meta(
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
    assert_eq!(
        h.fake.with(|s| s.remote_writes.clone()),
        ["modify old +INBOX -TRASH"]
    );
}
