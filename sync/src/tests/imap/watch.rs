use std::sync::Arc;
use std::time::Duration;

use async_channel::Receiver;
use mailrs_domain::{AccountState, ChangeEvent};
use mailrs_imap::ImapError;
use mailrs_store::{Db, accounts};

use super::{days_ago, message, offering};
use crate::fake::{FakeImap, FakeSmtp};
use crate::services::ImapApi;
use crate::tests::{fake_settings, imap_harness, imap_harness_on};
use crate::{AccountServices, AccountSync, EngineConfig, MailBackend, SyncEngine};

async fn wait_for(events: &Receiver<ChangeEvent>, wanted: impl Fn(&ChangeEvent) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("the engine closed its event channel");
            if wanted(&event) {
                return;
            }
        }
    })
    .await
    .expect("timed out waiting for an event");
}

/// Waits until the fake has taken `count` IDLE calls, so a change made
/// after this reaches a waiting IDLE rather than one not yet started.
async fn idling(imap: &FakeImap, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while imap.calls_to("idle") < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("timed out waiting for IDLE");
}

#[tokio::test]
async fn the_watch_ends_when_the_server_reports_a_change() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let mail = h.sync.services().mail.clone();
    let watching = tokio::spawn(async move { mail.watch().await });
    idling(&h.imap, 1).await;

    h.imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));

    assert!(
        tokio::time::timeout(Duration::from_secs(5), watching)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn without_idle_the_watch_never_ends_and_the_inbox_is_polled_each_minute() {
    let h = imap_harness_on(offering(|c| c.idle = false), fake_settings()).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    h.imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));

    let woke =
        tokio::time::timeout(Duration::from_millis(50), h.sync.services().mail.watch()).await;

    assert!(woke.is_err());
    assert_eq!(
        h.sync.services().mail.poll_interval(),
        Some(Duration::from_secs(60))
    );
}

#[tokio::test]
async fn with_idle_the_feed_is_read_at_the_slow_pace_and_slower_in_the_tray() {
    let h = imap_harness().await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    let mail = &h.sync.services().mail;

    assert_eq!(mail.poll_interval(), Some(Duration::from_secs(5 * 60)));
    mail.set_window_open(false);
    assert_eq!(mail.poll_interval(), Some(Duration::from_secs(15 * 60)));
}

#[tokio::test]
async fn mailboxes_other_than_the_inbox_wait_for_the_slow_poll() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account_id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let imap = Arc::new(FakeImap::new());
    let services = AccountServices::fake_imap(Arc::clone(&imap), Arc::new(FakeSmtp::default()));
    let sync = AccountSync::new(
        account_id,
        services,
        db.clone(),
        async_channel::unbounded().0,
    );
    sync.bootstrap().await.unwrap();
    while sync.backfill_step().await.unwrap() {}
    sync.incremental().await.unwrap();
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(0));
    imap.deliver_flagged(
        "Sent",
        &message("s", "Re: Moss", ""),
        &["\\Seen"],
        days_ago(0),
    );

    sync.incremental().await.unwrap();

    let stored: Vec<String> = db
        .read(move |c| {
            let mut stmt =
                c.prepare("SELECT id FROM messages WHERE account_id = ?1 ORDER BY id")?;
            let ids = stmt
                .query_map([account_id], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(ids)
        })
        .await
        .unwrap();
    assert_eq!(stored, ["INBOX/1001/1"], "Sent waits for the slow poll");
}

#[tokio::test]
async fn a_mailbox_a_person_opens_is_listed_and_kept_in_step() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Work", None);
    h.imap
        .deliver_flagged("Work", &message("w1", "Plans", ""), &[], days_ago(2));
    h.bootstrap().await;
    assert!(
        h.ids().await.is_empty(),
        "a mailbox nobody opened is not synced"
    );

    h.sync.follow_mailbox("Work").await.unwrap();
    assert_eq!(h.ids().await, ["Work/1007/1"]);

    h.imap
        .deliver_flagged("Work", &message("w2", "More plans", ""), &[], days_ago(0));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ["Work/1007/1", "Work/1007/2"]);
}

#[tokio::test]
async fn a_followed_mailbox_deleted_elsewhere_is_let_go() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Work", None);
    h.bootstrap().await;
    h.sync.follow_mailbox("Work").await.unwrap();
    h.sync.incremental().await.unwrap();
    h.imap.delete("Work").await.unwrap();

    h.sync.incremental().await.unwrap();
    assert!(
        !h.is_followed("Work"),
        "a mailbox deleted elsewhere leaves the followed set"
    );
    h.sync.incremental().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_watch_that_keeps_failing_waits_longer_each_time() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let mail = h.sync.services().mail.clone();

    h.imap.fail_on("idle", ImapError::Network("dropped".into()));
    let first = tokio::time::Instant::now();
    mail.watch().await;
    let first_wait = first.elapsed();

    h.imap.fail_on("idle", ImapError::Network("dropped".into()));
    let second = tokio::time::Instant::now();
    mail.watch().await;
    let second_wait = second.elapsed();

    assert!(
        second_wait > first_wait,
        "a second failure in a row should wait longer than the first: {first_wait:?} then {second_wait:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_that_floods_every_idle_is_watched_less_and_less_often() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let mail = h.sync.services().mail.clone();
    let mut waits = Vec::new();
    for _ in 0..3 {
        h.imap.overflow_next_idle();
        let start = tokio::time::Instant::now();
        mail.watch().await;
        waits.push(start.elapsed());
    }

    assert!(
        waits[0] < Duration::from_secs(1),
        "the first drop wakes the engine at once: {waits:?}"
    );
    assert!(waits[1] >= Duration::from_secs(60), "{waits:?}");
    assert!(waits[2] > waits[1], "{waits:?}");
}

#[tokio::test]
async fn new_mail_arrives_by_idle_without_waiting_for_a_poll() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    db.write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let config = EngineConfig {
        poll_interval: Duration::from_secs(60 * 60),
        ..EngineConfig::default()
    };
    let (engine, events) = SyncEngine::new(db.clone(), config);
    let imap = Arc::new(FakeImap::new());
    engine.start_account(
        1,
        AccountServices::fake_imap(Arc::clone(&imap), Arc::new(FakeSmtp::default())),
    );
    wait_for(&events, |e| {
        matches!(
            e,
            ChangeEvent::AccountStateChanged {
                state: AccountState::Ok,
                ..
            }
        )
    })
    .await;

    // The loop waits in IDLE once the window has loaded; mail arriving
    // before that would be read by the first poll, an hour away here.
    idling(&imap, 1).await;
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(0));

    wait_for(&events, |e| matches!(e, ChangeEvent::NewMail { .. })).await;
}
