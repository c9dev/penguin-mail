use std::sync::Arc;
use std::time::Duration;

use async_channel::Receiver;
use mailrs_dav::fake::FakeDav;
use mailrs_domain::{AccountState, ChangeEvent, MessageBody};
use mailrs_gmail::GmailError;
use mailrs_store::{Db, accounts};

use super::calendar_account::{PERSONAL, calendar_only};
use crate::MailBackend;
use crate::calendar_copy::CalendarCopy;
use crate::fake::{FakeGmail, fill_store, meta};
use crate::{
    AccountServices, AccountSync, Accounts, BackendError, CalendarService, EngineConfig,
    SyncEngine, now_millis,
};

struct Setup {
    engine: Arc<SyncEngine>,
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
        restart_after: Duration::from_millis(20),
        ..EngineConfig::default()
    };
    let (engine, events) = SyncEngine::new(db.clone(), config);
    Setup {
        engine: Arc::new(engine),
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
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
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
        AccountServices::google(Arc::clone(&s.fake)),
        s.db.clone(),
        sender,
    );
    fill_store(&earlier).await.unwrap();
    // Gmail archives it, and the store never hears.
    s.fake
        .with(|f| crate::fake::edit_labels(f.messages.get_mut("stale").unwrap(), Vec::clear));
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(
        &s.events,
        |e| matches!(e, ChangeEvent::ThreadsChanged { thread_ids, .. } if thread_ids == &["ts"]),
    )
    .await;
    let labels =
        s.db.read(|c| super::labels_of(c, 1, "stale"))
            .await
            .unwrap();
    assert!(labels.is_empty());
}

#[tokio::test]
async fn a_rejected_refresh_token_stops_the_account() {
    let s = setup().await;
    s.fake.fail_next(GmailError::NeedsReauth);
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
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
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Offline)).await;
    wait_for(&s.events, reached(AccountState::Ok)).await;
}

#[tokio::test]
async fn stopping_an_account_ends_its_loop() {
    let s = setup().await;
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Ok)).await;
    s.engine.stop_account(1);
    assert!(!s.engine.is_running(1));
    assert!(s.engine.account(1).is_err());
}

/// The bug this pins: a panic ended the account's task, nothing watched
/// it, and the account stopped syncing with the sidebar still saying all
/// was well.
#[tokio::test]
async fn a_loop_that_crashes_starts_again_after_a_pause() {
    let s = setup().await;
    s.fake.panic_next(1);
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Ok)).await;
    assert!(s.engine.is_running(1));
}

#[tokio::test]
async fn a_loop_that_crashes_twice_stops_and_says_so() {
    let s = setup().await;
    s.fake.panic_next(2);
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Stopped)).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.engine.is_running(1) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the account loop kept running");
    let all = s.db.read(accounts::list_accounts).await.unwrap();
    assert_eq!(all[0].state, AccountState::Stopped);
}

/// The bug this pins: with the network gone, each loop kept asking Gmail,
/// failed, and backed off, over and over, until it came back.
#[tokio::test]
async fn a_loop_waits_for_the_network_without_asking_gmail() {
    let s = setup().await;
    s.engine.set_network(false);
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Offline)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.fake.with(|f| f.usage.calls), 0);
    s.engine.set_network(true);
    wait_for(&s.events, reached(AccountState::Ok)).await;
}

#[tokio::test]
async fn losing_the_network_marks_a_waiting_account_offline_at_once() {
    let s = setup().await;
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Ok)).await;
    s.engine.set_network(false);
    wait_for(&s.events, reached(AccountState::Offline)).await;
}

/// The network monitor can be wrong, so Check for Mail still asks Gmail
/// while it says the network is gone.
#[tokio::test]
async fn a_check_asks_gmail_while_the_network_seems_gone() {
    let s = setup().await;
    s.engine.set_network(false);
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Offline)).await;
    s.engine.poke(1);
    wait_for(&s.events, reached(AccountState::Ok)).await;
}

const NOW: i64 = 1_790_000_000_000;

/// A calendar account with lunch on its server, kept by the engine, and
/// the copy that reads it.
async fn calendar_account() -> (Setup, Arc<FakeDav>, CalendarCopy<SyncEngine>) {
    let s = setup().await;
    let (dav, services) = calendar_only();
    dav.put_resource(
        &format!("{PERSONAL}lunch.ics"),
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:lunch\r\n\
         DTSTART:20261006T120000Z\r\nDTEND:20261006T130000Z\r\nSUMMARY:Lunch\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    s.engine.start_account(1, services);
    // Finish the startup announcement before a read changes the state,
    // so it cannot arrive among the notifications a test is checking.
    wait_for(&s.events, reached(AccountState::Bootstrapping)).await;
    let copy = CalendarCopy::new(Arc::clone(&s.engine), s.db.clone());
    (s, dav, copy)
}

async fn stored_state(s: &Setup) -> AccountState {
    s.db.read(|c| accounts::account(c, 1))
        .await
        .unwrap()
        .expect("the account")
        .state
}

#[tokio::test]
async fn an_account_with_no_mail_is_kept_and_the_copy_reads_its_calendar() {
    let (s, _, copy) = calendar_account().await;
    assert!(s.engine.is_running(1));
    let kept = Accounts::services(&*s.engine, 1).expect("the account's services");
    assert!(!kept.serves_mail() && kept.calendar.is_some());
    assert_eq!(copy.refresh_due(&[1], NOW, true).await.unwrap().events, 1);
    let calendars =
        s.db.read(|c| mailrs_store::calendar::calendars(c, 1))
            .await
            .unwrap();
    assert_eq!(
        calendars
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["Personal"]
    );
    // No mail loop ran to say so: the read that went through did.
    wait_for(&s.events, reached(AccountState::Ok)).await;
    s.engine.stop_account(1);
    assert!(!s.engine.is_running(1));
}

/// The window reads what a started account offers when it hears of it.
/// An account with no mail runs no loop to say how it is doing, and one
/// that worked last time has no new state for the copy to report, so it
/// says once, as it starts, the state it has.
#[tokio::test]
async fn a_calendar_account_says_its_state_once_it_starts() {
    let s = setup().await;
    s.db.write(|c| accounts::set_state(c, 1, AccountState::Ok))
        .await
        .unwrap();
    s.engine.start_account(1, calendar_only().1);
    wait_for(&s.events, reached(AccountState::Ok)).await;
}

#[tokio::test]
async fn a_calendar_account_whose_login_is_refused_waits_for_the_person_to_sign_in_again() {
    let (s, dav, copy) = calendar_account().await;
    dav.refuse_login(true);
    copy.refresh_due(&[1], NOW, true).await.unwrap();
    // Nothing else would notice the refused password.
    wait_for(&s.events, reached(AccountState::NeedsReauth)).await;

    // The server takes the password again, but the account that was
    // refused does not go on asking with the one it holds.
    dav.refuse_login(false);
    let later = NOW + 3_600_000;
    assert_eq!(copy.refresh_due(&[1], later, true).await.unwrap().events, 0);

    // Signing in again starts the account on new services, and it reads.
    s.engine
        .start_account(1, AccountServices::calendar_only(Arc::clone(&dav)));
    assert_eq!(copy.refresh_due(&[1], later, true).await.unwrap().events, 1);
    wait_for(&s.events, reached(AccountState::Ok)).await;
}

#[tokio::test]
async fn a_calendar_account_still_refused_when_it_starts_is_not_said_to_work() {
    let (s, dav, copy) = calendar_account().await;
    s.db.write(|c| accounts::set_state(c, 1, AccountState::NeedsReauth))
        .await
        .unwrap();
    dav.refuse_login(true);
    copy.refresh_due(&[1], NOW, true).await.unwrap();
    assert_eq!(stored_state(&s).await, AccountState::NeedsReauth);
    let said: Vec<ChangeEvent> = std::iter::from_fn(|| s.events.try_recv().ok()).collect();
    assert!(
        !said.iter().any(reached(AccountState::Ok)),
        "nothing spoke to the server and heard yes: {said:?}"
    );
}

#[tokio::test]
async fn a_refusal_met_outside_the_read_is_reported_once() {
    let (s, dav, copy) = calendar_account().await;
    assert_eq!(copy.refresh_due(&[1], NOW, true).await.unwrap().events, 1);
    wait_for(&s.events, reached(AccountState::Ok)).await;

    // The password stops working, and what meets the refusal first is
    // not the periodic read but a call made for the person.
    dav.refuse_login(true);
    let calendar = Accounts::services(&*s.engine, 1)
        .and_then(|services| services.calendar)
        .expect("the calendar");
    assert!(matches!(
        calendar.calendars().await,
        Err(BackendError::NeedsReauth)
    ));

    // The next tick comes before another read is due. It still says so.
    copy.refresh_due(&[1], NOW + 1_000, true).await.unwrap();
    wait_for(&s.events, reached(AccountState::NeedsReauth)).await;
    assert_eq!(stored_state(&s).await, AccountState::NeedsReauth);

    // And says it once, not at every tick after.
    copy.refresh_due(&[1], NOW + 2_000, true).await.unwrap();
    copy.refresh_due(&[1], NOW + 3_000, true).await.unwrap();
    let extra = s.events.try_recv();
    assert!(extra.is_err(), "the state was reported again: {extra:?}");
}

#[tokio::test]
async fn gmail_looks_less_often_while_only_the_tray_runs() {
    let s = setup().await;
    let services = AccountServices::google(Arc::clone(&s.fake));
    assert_eq!(
        services.mail.poll_interval(),
        None,
        "the engine's own pace while the window is open"
    );
    services.mail.set_window_open(false);
    assert_eq!(
        services.mail.poll_interval(),
        Some(Duration::from_secs(5 * 60))
    );
    services.mail.set_window_open(true);
    assert_eq!(services.mail.poll_interval(), None);
}

#[tokio::test]
async fn opening_the_window_looks_for_mail_at_once() {
    let s = setup().await;
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::Ok)).await;
    s.engine.set_window_open(false);
    s.fake
        .deliver(meta("n", "tn", now_millis(), &["INBOX", "UNREAD"]));
    // The tray's next look is minutes away; showing the window cannot wait for it.
    s.engine.set_window_open(true);
    wait_for(&s.events, |e| matches!(e, ChangeEvent::NewMail { .. })).await;
}

#[tokio::test]
async fn an_own_account_whose_token_is_refused_needs_reauth() {
    let s = setup().await;
    s.fake.fail_next(GmailError::NeedsReauth);
    s.engine
        .start_account(1, AccountServices::google(Arc::clone(&s.fake)));
    wait_for(&s.events, reached(AccountState::NeedsReauth)).await;
    assert_eq!(stored_state(&s).await, AccountState::NeedsReauth);
}

#[tokio::test]
async fn the_engine_prefetches_the_configured_number_of_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let fake = Arc::new(FakeGmail::new());
    fake.seed(meta("prefetched", "t1", now_millis(), &["INBOX"]));
    fake.with(|s| {
        s.bodies.insert(
            "prefetched".into(),
            MessageBody {
                text: Some("ready offline".into()),
                ..Default::default()
            },
        );
    });
    let config = EngineConfig {
        body_prefetch_count: 1,
        backfill_pause: Duration::from_millis(1),
        ..EngineConfig::default()
    };
    let (engine, _events) = SyncEngine::new(db.clone(), config);
    engine.start_account(account, AccountServices::google(Arc::clone(&fake)));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(body) = db
                .read(move |c| mailrs_store::bodies::peek_body(c, account, "prefetched"))
                .await
                .unwrap()
            {
                assert_eq!(body.text.as_deref(), Some("ready offline"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the engine did not cache the configured Inbox text");
    engine.shutdown();
}
