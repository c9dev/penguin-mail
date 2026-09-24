use mailrs_domain::ChangeEvent;
use mailrs_imap::ImapError;

use super::{days_ago, message, offering};
use crate::fake::FakeImap;
use crate::tests::{ImapHarness, fake_settings, imap_harness_on};

/// An account on `imap` holding one unread message, with its window
/// loaded.
async fn synced_on(imap: FakeImap) -> ImapHarness {
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    h
}

/// New mail, flags set and cleared, and an expunge, all made by another
/// client, reach the store.
async fn changes_elsewhere_reach_the_store(h: ImapHarness) {
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/2"]);
    assert!(h.drain().iter().any(|e| matches!(
        e,
        ChangeEvent::NewMail { message_ids, .. } if message_ids == &["INBOX/1001/2".to_string()]
    )));

    h.imap.set_flags("INBOX", 1, &["\\Seen", "\\Flagged"]);
    h.sync.incremental().await.unwrap();
    let a = h.stored("INBOX/1001/1").await.unwrap();
    assert!(!a.is_unread() && a.is_flagged());

    h.imap.set_flags("INBOX", 1, &[]);
    h.sync.incremental().await.unwrap();
    let a = h.stored("INBOX/1001/1").await.unwrap();
    assert!(a.is_unread() && !a.is_flagged());

    h.imap.remote_expunge("INBOX", 2);
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

#[tokio::test]
async fn under_qresync_changes_elsewhere_reach_the_store() {
    let h = synced_on(offering(|_| {})).await;
    changes_elsewhere_reach_the_store(h).await;
}

#[tokio::test]
async fn with_condstore_alone_changes_elsewhere_reach_the_store() {
    let h = synced_on(offering(|c| c.qresync = false)).await;
    changes_elsewhere_reach_the_store(h).await;
}

#[tokio::test]
async fn with_neither_changes_elsewhere_reach_the_store() {
    let h = synced_on(
        offering(|c| {
            c.qresync = false;
            c.condstore = false;
        }),
    )
    .await;
    changes_elsewhere_reach_the_store(h).await;
}

#[tokio::test]
async fn with_neither_a_quiet_look_announces_nothing() {
    let h = synced_on(
        offering(|c| {
            c.qresync = false;
            c.condstore = false;
        }),
    )
    .await;
    h.sync.incremental().await.unwrap();
    h.drain();

    h.sync.incremental().await.unwrap();

    assert!(
        !h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::ThreadsChanged { .. })),
        "a look that finds the flags as they were redraws no list"
    );
}

/// A QRESYNC SELECT that answers past its byte budget drops the
/// connection; the look falls back to a plain SELECT, once, and still
/// finds what changed, through CONDSTORE. Two accounts run the same
/// single-message arrival, one of them with the failure planted, so the
/// only difference between their SELECT counts is the one extra SELECT
/// the fallback costs.
#[tokio::test]
async fn a_qresync_select_past_its_budget_falls_back_to_a_plain_look() {
    let ordinary = synced_on(offering(|_| {})).await;
    ordinary
        .imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));
    let before = ordinary.imap.calls_to("select");
    ordinary.sync.incremental().await.unwrap();
    let ordinary_selects = ordinary.imap.calls_to("select") - before;

    let retried = synced_on(offering(|_| {})).await;
    retried
        .imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));
    retried
        .imap
        .fail_on("select", ImapError::Protocol("too many changes".into()));
    let before = retried.imap.calls_to("select");
    retried.sync.incremental().await.unwrap();
    let retried_selects = retried.imap.calls_to("select") - before;

    assert_eq!(retried.ids().await, ["INBOX/1001/1", "INBOX/1001/2"]);
    assert_eq!(
        retried_selects,
        ordinary_selects + 1,
        "the failed QRESYNC SELECT costs exactly one extra plain SELECT"
    );
}
