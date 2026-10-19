use mailrs_domain::{ChangeEvent, MailSet, Role};
use mailrs_gmail::GmailError;

use std::sync::Arc;

use super::{harness, harness_with};
use crate::fake::{FakeGmail, meta};
use crate::now_millis;
use crate::{AccountServices, MailCapabilities};

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
    assert_eq!(
        h.threads(MailSet::Role(Role::Inbox)).await,
        ["tnew", "told"]
    );
    assert_eq!(h.history_id().await, Some(101));
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
    assert_eq!(h.threads(MailSet::Role(Role::Sent)).await, ["ts"]);
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
    assert!(h.labels_of("a").await.is_empty());
    assert!(!h.thread("ta").await.unwrap().unread);
    assert!(h.thread("tb").await.is_none());
}

#[tokio::test]
async fn old_mail_moved_into_the_inbox_is_fetched() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake
        .seed(meta("old", "told", now_millis() - 90 * DAY, &[]));
    h.fake.remote_relabel("old", &["INBOX"], &[]);
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["told"]);
    assert!(
        !h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::NewMail { .. })),
        "a feed that does not restate announces no old mail"
    );
}

/// Gmail's services, claiming a change feed that restates each changed
/// message whole, as Graph's delta queries do.
fn restating(fake: Arc<FakeGmail>) -> AccountServices {
    let caps = MailCapabilities {
        restates: true,
        ..AccountServices::google(Arc::clone(&fake)).capabilities()
    };
    AccountServices::fake_with_capabilities(fake, caps)
}

#[tokio::test]
async fn a_restating_feed_stores_mail_it_names_first() {
    let h = harness_with(restating).await;
    h.bootstrap_all().await;
    h.fake
        .seed(meta("old", "told", now_millis() - 90 * DAY, &[]));
    h.fake.remote_relabel("old", &["Label_1"], &[]);
    h.sync.incremental().await.unwrap();
    assert_eq!(h.labels_of("old").await, ["Label_1"]);
}

#[tokio::test]
async fn a_restating_feed_announces_new_inbox_mail() {
    let h = harness_with(restating).await;
    h.bootstrap_all().await;
    h.fake.seed(meta("new", "tnew", now_millis(), &["UNREAD"]));
    h.fake.remote_relabel("new", &["INBOX"], &[]);
    h.sync.incremental().await.unwrap();
    assert!(h.drain().contains(&ChangeEvent::NewMail {
        account_id: 1,
        message_ids: vec!["new".into()]
    }));
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
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await.len(), 5);
    assert_eq!(h.history_id().await, Some(105));
}

/// History comes two changes a page here, so what happens to one message
/// spreads over three pages. A message starred and archived on the first
/// page and put back on the second ends in the inbox, still starred. One
/// delivered on the second and deleted on the third is neither stored nor
/// announced.
#[tokio::test]
async fn history_pages_apply_in_the_order_gmail_sent_them() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "ta", now - DAY, &["INBOX"]));
    h.bootstrap_all().await;
    h.drain();
    h.fake.remote_relabel("a", &["STARRED"], &[]);
    h.fake.remote_relabel("a", &[], &["INBOX"]);
    h.fake.remote_relabel("a", &["INBOX"], &[]);
    h.fake
        .deliver(meta("brief", "tbrief", now, &["INBOX", "UNREAD"]));
    h.fake.remote_delete("brief");
    h.fake.reset_usage();

    h.sync.incremental().await.unwrap();

    assert_eq!(h.fake.usage().calls_to("users.history.list"), 3);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["ta"]);
    assert_eq!(h.labels_of("a").await, ["INBOX", "STARRED"]);
    assert!(h.thread("tbrief").await.is_none());
    assert!(
        !h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::NewMail { .. }))
    );
    assert_eq!(h.history_id().await, Some(105));
}

/// Gmail answers the first history page and garbles the second. Nothing
/// from the first page is stored and the cursor stays put, so the next
/// check replays both pages.
#[tokio::test]
async fn a_garbled_later_history_page_applies_nothing() {
    let h = harness().await;
    h.bootstrap_all().await;
    let now = now_millis();
    for i in 0..3 {
        h.fake.deliver(meta(
            &format!("m{i}"),
            &format!("t{i}"),
            now - i,
            &["INBOX"],
        ));
    }
    h.fake.fail_call(
        "users.history.list",
        1,
        GmailError::Decode("expected value at line 1 column 1".into()),
    );

    assert!(h.sync.incremental().await.is_err());
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
    assert_eq!(h.history_id().await, Some(100));

    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await.len(), 3);
    assert_eq!(h.history_id().await, Some(103));
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
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["tkeep"]);
    let cursor = h.cursor().await;
    assert_eq!(h.history_id().await, Some(101));
    assert!(cursor.backfill_done);
}

#[tokio::test]
async fn incremental_without_a_cursor_bootstraps() {
    let h = harness().await;
    h.fake.seed(meta("a", "ta", now_millis(), &["INBOX"]));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["ta"]);
}

#[tokio::test]
async fn a_gmail_failure_leaves_the_cursor_alone() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.deliver(meta("a", "ta", now_millis(), &["INBOX"]));
    h.fake.fail_next(GmailError::Network("down".into()));
    assert!(h.sync.incremental().await.is_err());
    assert_eq!(h.history_id().await, Some(100));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["ta"]);
}

/// A message whose stored copy missed an archive: Gmail took it out of the
/// inbox, but no history after the cursor mentions it, as when a write of
/// older labels landed after the replay that carried the change.
async fn archived_behind_the_cursor() -> super::Harness {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("kept", "tkept", now, &["INBOX"]));
    h.fake
        .seed(meta("stale", "tstale", now - 1000, &["INBOX", "IMPORTANT"]));
    h.bootstrap_all().await;
    h.fake.with(|s| {
        let stale = s.messages.get_mut("stale").expect("seeded");
        crate::fake::edit_labels(stale, |ids| ids.retain(|l| l != "INBOX"));
    });
    h.sync.incremental().await.unwrap();
    h
}

#[tokio::test]
async fn history_alone_leaves_a_missed_archive_in_the_inbox() {
    let h = archived_behind_the_cursor().await;
    assert_eq!(
        h.threads(MailSet::Role(Role::Inbox)).await,
        ["tkept", "tstale"]
    );
}

#[tokio::test]
async fn reconciling_the_inbox_takes_out_mail_gmail_archived() {
    let h = archived_behind_the_cursor().await;
    h.drain();
    h.sync.reconcile_inbox().await.unwrap();
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["tkept"]);
    assert_eq!(h.labels_of("stale").await, ["IMPORTANT"]);
    assert!(h.drain().contains(&ChangeEvent::ThreadsChanged {
        account_id: 1,
        thread_ids: vec!["tstale".into()]
    }));
}

#[tokio::test]
async fn reconciling_the_inbox_brings_back_mail_it_missed() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("kept", "tkept", now, &["INBOX"]));
    h.fake
        .seed(meta("filed", "tfiled", now - 1000, &["Label_1"]));
    h.bootstrap_all().await;
    h.fake.with(|s| {
        let filed = s.messages.get_mut("filed").expect("seeded");
        crate::fake::edit_labels(filed, |ids| ids.push("INBOX".into()));
    });
    h.sync.reconcile_inbox().await.unwrap();
    assert_eq!(
        h.threads(MailSet::Role(Role::Inbox)).await,
        ["tkept", "tfiled"]
    );
}

#[tokio::test]
async fn reconciling_leaves_mail_that_just_arrived_to_history() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake
        .deliver(meta("new", "tnew", now_millis(), &["INBOX", "UNREAD"]));
    h.sync.reconcile_inbox().await.unwrap();
    assert!(h.thread("tnew").await.is_none());
    h.sync.incremental().await.unwrap();
    assert!(h.drain().contains(&ChangeEvent::NewMail {
        account_id: 1,
        message_ids: vec!["new".into()]
    }));
}

#[tokio::test]
async fn reconciling_the_inbox_drops_mail_gmail_no_longer_has() {
    let h = harness().await;
    h.fake.seed(meta("gone", "tgone", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.remote_delete_silently("gone");
    h.sync.reconcile_inbox().await.unwrap();
    assert!(h.thread("tgone").await.is_none());
}

#[tokio::test]
async fn a_matching_inbox_is_left_alone() {
    let h = harness().await;
    h.fake.seed(meta("a", "ta", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.reset_usage();
    h.sync.reconcile_inbox().await.unwrap();
    assert_eq!(h.fake.usage().calls_to("users.messages.get"), 0);
    assert!(h.drain().is_empty());
}

#[tokio::test]
async fn reconciling_waits_for_the_window_to_finish_loading() {
    let h = harness().await;
    let now = now_millis();
    for i in 0..3 {
        h.fake.seed(meta(
            &format!("m{i}"),
            &format!("t{i}"),
            now - i,
            &["INBOX"],
        ));
    }
    h.sync.bootstrap().await.unwrap();
    assert!(!h.cursor().await.backfill_done);
    h.fake.reset_usage();
    h.sync.reconcile_inbox().await.unwrap();
    assert_eq!(h.fake.usage().calls, 0);
}

/// Gmail's services claiming an IMAP server's place names beside Gmail's
/// own labels, as Gmail over IMAP does: messages keep their hex ids and
/// a ref follows each between mailboxes. `slot` receives the Google
/// service, so a test can add changes to its feed.
fn locating(
    fake: Arc<FakeGmail>,
    slot: Arc<std::sync::Mutex<Option<crate::services::Google<crate::AnyGmail>>>>,
) -> AccountServices {
    let caps = MailCapabilities {
        renames: true,
        ..AccountServices::google(Arc::clone(&fake)).capabilities()
    };
    let services = AccountServices::fake_with_capabilities(fake, caps);
    if let crate::AnyMail::Google(google) = &services.mail {
        *slot.lock().unwrap() = Some(google.clone());
    }
    services
}

#[tokio::test]
async fn a_located_message_survives_its_old_mailboxs_vanished() {
    use mailrs_domain::Location;
    use mailrs_imap::UidSet;
    use mailrs_store::remote_refs;

    let slot = Arc::default();
    let h = harness_with(|fake| locating(fake, Arc::clone(&slot))).await;
    h.fake
        .seed(meta("18c2f0a1b2c3d4e5", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let place = |mailbox: &str, uid| Location {
        mailbox: mailbox.into(),
        uidvalidity: 1,
        uid,
    };
    let (account_id, there) = (h.account_id, place("A", 5));
    h.db.write(move |c| remote_refs::locate(c, account_id, "18c2f0a1b2c3d4e5", &there))
        .await
        .unwrap();

    slot.lock().unwrap().as_ref().unwrap().report(vec![
        crate::RemoteChange::Located {
            id: "18c2f0a1b2c3d4e5".into(),
            at: place("B", 9),
        },
        crate::RemoteChange::Vanished {
            mailbox: "A".into(),
            uidvalidity: 1,
            uids: UidSet::from_uids([5]),
        },
    ]);
    h.sync.incremental().await.unwrap();

    assert_eq!(h.all_threads().await, ["t1"]);
    let moved = h
        .db
        .read(move |c| remote_refs::remotes_of(c, account_id, &["18c2f0a1b2c3d4e5".to_string()]))
        .await
        .unwrap();
    assert_eq!(moved["18c2f0a1b2c3d4e5"], "B/1/9");
}
