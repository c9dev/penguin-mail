use std::sync::Arc;
use std::time::Duration;

use async_channel::Receiver;
use mailrs_domain::{AccountState, ChangeEvent};
use mailrs_gmail::GmailError;
use mailrs_store::{Db, accounts, messages};

use crate::fake::{FakeGmail, fill_store, meta};
use crate::{AccountServices, AccountSync, EngineConfig, SyncEngine, now_millis};

struct Setup {
    engine: SyncEngine,
    events: Receiver<ChangeEvent>,
    fake: Arc<FakeGmail>,
    db: Db,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    db.write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let config = EngineConfig {
        poll_interval: Duration::from_secs(60),
        max_backoff: Duration::from_millis(20),
        ..EngineConfig::default()
    };
    let (engine, events) = SyncEngine::new(db.clone(), config);
    Setup {
        engine,
        events,
        fake: Arc::new(FakeGmail::new()),
        db,
        _dir: dir,
    }
}

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

fn reached(wanted: AccountState) -> impl Fn(&ChangeEvent) -> bool {
    move |event| matches!(event, ChangeEvent::AccountStateChanged { state, .. } if *state == wanted)
}

#[tokio::test]
async fn the_engine_bootstraps_and_polls_when_poked() {
    let s = setup().await;
    s.engine.start_account(1, AccountServices::fake(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Ok)).await;
    s.fake
        .deliver(meta("n", "tn", now_millis(), &["INBOX", "UNREAD"]));
    s.engine.poke(1);
    wait_for(&s.events, |e| matches!(e, ChangeEvent::NewMail { .. })).await;
    assert!(s.engine.is_running(1));
    assert!(s.engine.account(1).is_ok());
    assert!(s.engine.account(2).is_err());
}

#[tokio::test]
async fn the_engine_corrects_a_stale_inbox_when_it_starts() {
    let s = setup().await;
    s.fake.seed(meta("stale", "ts", now_millis(), &["INBOX"]));
    let (sender, _receiver) = async_channel::unbounded();
    let earlier = AccountSync::new(
        1,
        AccountServices::fake(Arc::clone(&s.fake)),
        s.db.clone(),
        sender,
    );
    fill_store(&earlier).await.unwrap();
    // Gmail archives it, and the store never hears.
    s.fake
        .with(|f| f.messages.get_mut("stale").unwrap().label_ids.clear());
    s.engine.start_account(1, AccountServices::fake(Arc::clone(&s.fake)));
    wait_for(
        &s.events,
        |e| matches!(e, ChangeEvent::ThreadsChanged { thread_ids, .. } if thread_ids == &["ts"]),
    )
    .await;
    let labels =
        s.db.read(|c| messages::labels_of(c, 1, "stale"))
            .await
            .unwrap();
    assert!(labels.is_empty());
}

#[tokio::test]
async fn a_rejected_refresh_token_stops_the_account() {
    let s = setup().await;
    s.fake.fail_next(GmailError::NeedsReauth);
    s.engine.start_account(1, AccountServices::fake(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::NeedsReauth)).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.engine.is_running(1) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the account loop kept running");
}

#[tokio::test]
async fn a_network_failure_goes_offline_then_recovers() {
    let s = setup().await;
    s.fake.fail_next(GmailError::Network("down".into()));
    s.engine.start_account(1, AccountServices::fake(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Offline)).await;
    wait_for(&s.events, reached(AccountState::Ok)).await;
}

#[tokio::test]
async fn stopping_an_account_ends_its_loop() {
    let s = setup().await;
    s.engine.start_account(1, AccountServices::fake(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Ok)).await;
    s.engine.stop_account(1);
    assert!(!s.engine.is_running(1));
    assert!(s.engine.account(1).is_err());
}
