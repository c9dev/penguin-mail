//! The POP3 check against `FakePop3`: nothing downloads twice, removal
//! follows the account's setting and waits for a clean QUIT, one refused
//! message holds up nothing, and two checks never share the server.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{ChangeEvent, EpochMillis, MailSet, MessageMeta, RemoveSetting, Role, Target};
use mailrs_pop3::MOST_MESSAGE_BYTES;
use mailrs_store::pop3::{FailReason, Failing};
use mailrs_store::{local_messages, messages, pop3};
use tokio::time::Instant;

use super::pop3::{Pop3Harness, pop3_harness};
use crate::fake::{FakePop3, pop3_mail};
use crate::{BackendError, EngineConfig, SyncError, now_millis};

const DAY: EpochMillis = 24 * 60 * 60 * 1000;

fn drain(h: &Pop3Harness) -> Vec<ChangeEvent> {
    std::iter::from_fn(|| h.events.try_recv().ok()).collect()
}

async fn meta(h: &Pop3Harness, id: &str) -> MessageMeta {
    let (account_id, ids) = (h.account_id, vec![id.to_string()]);
    h.db.read(move |c| messages::by_ids(c, account_id, &ids)).await.unwrap().pop().expect("stored")
}

async fn pending(h: &Pop3Harness) -> Vec<String> {
    let account_id = h.account_id;
    h.db.read(move |c| pop3::pending_removal(c, account_id, None, pop3::PAGE)).await.unwrap()
}

async fn failing(h: &Pop3Harness) -> Vec<Failing> {
    let account_id = h.account_id;
    h.db.read(move |c| pop3::failing(c, account_id)).await.unwrap()
}

#[tokio::test]
async fn a_first_check_keeps_every_message_with_its_bytes_and_announces_none() {
    let fake = FakePop3::default().with_message("u1", &pop3_mail(1)).with_message("u2", &pop3_mail(2));
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/u1", "pop3/u2"]);
    assert_eq!(h.ids_in(MailSet::Unseen).await.len(), 2);
    let account_id = h.account_id;
    let raw = h.db.read(move |c| local_messages::get(c, account_id, "pop3/u1")).await.unwrap();
    assert_eq!(raw, Some(pop3_mail(1)), "the raw copy is kept with the row");
    assert_eq!(meta(&h, "pop3/u1").await.date, 1_609_750_800_000, "a first download keeps each message's own date");
    assert_eq!(h.fake.held(), ["u1", "u2"], "Leave on Server");
    assert!(h.fake.deleted().is_empty());
    let heard = drain(&h);
    assert!(heard.iter().any(|e| matches!(e, ChangeEvent::ThreadsChanged { .. })));
    assert!(!heard.iter().any(|e| matches!(e, ChangeEvent::NewMail { .. })), "a first download raises no notifications");
}

#[tokio::test]
async fn nothing_downloads_twice_across_a_restart_or_after_delete_forever() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    h.again().pop3_check().await.unwrap();
    assert_eq!(h.fake.retr_calls(), [1]);
    let thread = h.thread_of("pop3/u1").await;
    h.sync.erase_all(&[Target::thread(h.account_id, thread)]).await.unwrap();
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.fake.retr_calls(), [1], "a message deleted here stays gone while the server keeps it");
    assert!(h.ids_in(MailSet::Role(Role::Inbox)).await.is_empty());
    assert_eq!(h.fake.held(), ["u1"], "Leave on Server keeps the server's copy");
}

#[tokio::test]
async fn leave_on_server_never_sends_dele() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Never).await;
    for _ in 0..3 {
        h.sync.pop3_check().await.unwrap();
    }
    assert!(h.fake.deleted().is_empty());
    assert!(pending(&h).await.is_empty());
}

#[tokio::test]
async fn leave_on_server_sends_no_dele_for_a_row_an_earlier_setting_marked() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Never).await;
    h.keep("u1", &pop3_mail(1), "inbox", now_millis()).await;
    let account_id = h.account_id;
    h.db.write(move |c| pop3::want_removed(c, account_id, &["u1".to_string()])).await.unwrap();
    h.sync.pop3_check().await.unwrap();
    assert!(h.fake.deleted().is_empty(), "the person switched to Leave on Server since");
    assert_eq!(pending(&h).await, ["u1"]);
}

#[tokio::test]
async fn remove_after_downloading_deletes_on_the_server_at_a_clean_quit() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Downloaded).await;
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.fake.deleted(), [1]);
    assert!(h.fake.held().is_empty());
    assert!(pending(&h).await.is_empty(), "QUIT confirmed it");
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/u1"], "the copy here stays");
}

#[tokio::test]
async fn a_session_dropped_before_quit_retries_its_removal_next_check() {
    let dropping = FakePop3::default().with_message("u1", &pop3_mail(1)).dropping_before_quit();
    let h = pop3_harness(dropping, RemoveSetting::Downloaded).await;
    assert!(h.sync.pop3_check().await.is_err(), "QUIT failed");
    assert_eq!(h.fake.held(), ["u1"], "the dropped session deleted nothing");
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/u1"], "the message is kept all the same");
    let heard = drain(&h);
    assert!(heard.iter().any(|e| matches!(e, ChangeEvent::ThreadsChanged { .. })), "the window hears of it all the same");
    assert_eq!(pending(&h).await, ["u1"]);

    let server = Arc::new(FakePop3::default().with_message("u1", &pop3_mail(1)));
    h.with_server(Arc::clone(&server)).pop3_check().await.unwrap();
    assert!(server.retr_calls().is_empty(), "not downloaded again");
    assert_eq!(server.deleted(), [1], "the DELE went again");
    assert!(server.held().is_empty());
    assert!(pending(&h).await.is_empty());
}

#[tokio::test]
async fn removal_after_days_deletes_only_what_has_been_here_that_long() {
    let fake = FakePop3::default().with_message("old", &pop3_mail(1)).with_message("new", &pop3_mail(2));
    let h = pop3_harness(fake, RemoveSetting::Days(30)).await;
    h.keep("old", &pop3_mail(1), "inbox", now_millis() - 40 * DAY).await;
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.fake.held(), ["new"]);
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/new", "pop3/old"], "both stay here");
}

#[tokio::test]
async fn delete_forever_on_a_removing_account_reaches_the_server_at_the_next_check() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Days(30)).await;
    h.sync.pop3_check().await.unwrap();
    assert!(h.fake.deleted().is_empty(), "not thirty days yet");
    let thread = h.thread_of("pop3/u1").await;
    h.sync.erase_all(&[Target::thread(h.account_id, thread)]).await.unwrap();
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.fake.deleted(), [1]);
    assert!(h.fake.held().is_empty());
}

#[tokio::test]
async fn two_checks_for_one_account_never_hold_two_sessions() {
    let (fake, release) = FakePop3::default().with_message("u1", &pop3_mail(1)).holding_retr();
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    let (a, b) = (Arc::clone(&h.sync), Arc::clone(&h.sync));
    let first = tokio::spawn(async move { a.pop3_check().await });
    let second = tokio::spawn(async move { b.pop3_check().await });
    // Each check reads the store on a blocking thread before it reaches
    // the server, so yielding alone does not get one as far as RETR.
    let started = Instant::now();
    while h.fake.retr_calls().is_empty() {
        assert!(started.elapsed() < Duration::from_secs(5), "a check reached RETR");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!((h.fake.in_flight(), h.fake.connects()), (1, 1), "the first check holds RETR; the second waits its turn");
    release.release();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(h.fake.connects(), 2, "the second check ran after the first");
    assert_eq!(h.fake.most_in_flight(), 1);
    assert_eq!(h.fake.retr_calls(), [1], "and found the message downloaded");
}

#[tokio::test]
async fn one_refused_retr_leaves_the_rest_downloaded_and_counts_a_failure() {
    let fake = FakePop3::default()
        .with_message("bad", &pop3_mail(1))
        .with_message("u2", &pop3_mail(2))
        .failing_retr("bad");
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    for round in 1..=3 {
        drain(&h);
        h.sync.pop3_check().await.unwrap();
        let told = drain(&h).iter().any(|e| matches!(e, ChangeEvent::LabelsChanged { .. }));
        assert_eq!(told, round == 3, "the third failure tells the window to rebuild the menu");
        assert_eq!(failing(&h).await.is_empty(), round < 3, "check {round}");
    }
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/u2"]);
    assert_eq!(
        failing(&h).await,
        [Failing {
            uidl: "bad".into(),
            reason: FailReason::Refused,
            words: "message 1 cannot be read".into(),
            sender: Some("Ana".into()),
            subject: Some("Hello 1".into()),
        }],
        "the third failure reads the message's headers with TOP, so the menu can name it"
    );
    assert_eq!(h.fake.retr_calls(), [1, 2, 1, 1], "tried again at each check");
}

#[tokio::test]
async fn a_message_over_the_size_limit_is_counted_without_retr() {
    let fake = FakePop3::default().with_message("big", &pop3_mail(1)).claiming_size("big", MOST_MESSAGE_BYTES + 1);
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    for _ in 0..3 {
        h.sync.pop3_check().await.unwrap();
    }
    assert!(h.fake.retr_calls().is_empty());
    assert_eq!(failing(&h).await.len(), 1);
}

#[tokio::test]
async fn mail_after_the_first_check_is_announced_and_dated_when_it_arrived() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    drain(&h);
    h.fake.add("u2", &pop3_mail(2));
    let before = now_millis();
    h.sync.pop3_check().await.unwrap();
    let announced: Vec<Vec<String>> = drain(&h)
        .into_iter()
        .filter_map(|e| match e {
            ChangeEvent::NewMail { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .collect();
    assert_eq!(announced, [vec!["pop3/u2".to_string()]], "local rules and the notification hear of it");
    assert!(meta(&h, "pop3/u2").await.date >= before, "dated when it arrived, so the rules' watermark takes it");
}

#[tokio::test]
async fn a_uidl_the_server_dropped_is_forgotten_after_a_clean_quit() {
    let fake = FakePop3::default().with_message("u1", &pop3_mail(1)).with_message("u2", &pop3_mail(2));
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    h.fake.take("u2");
    h.sync.pop3_check().await.unwrap();
    let account_id = h.account_id;
    let unseen = h
        .db
        .read(move |c| pop3::unseen(c, account_id, &["u1".to_string(), "u2".to_string()]))
        .await
        .unwrap();
    assert_eq!(unseen, ["u2"], "u2's row went; u1's stays while the server lists it");
}

#[tokio::test]
async fn a_refused_password_needs_a_new_sign_in() {
    let h = pop3_harness(FakePop3::default().refusing_sign_in(), RemoveSetting::Never).await;
    let refused = h.sync.pop3_check().await;
    assert!(matches!(refused, Err(SyncError::Backend(BackendError::NeedsReauth))), "{refused:?}");
}

#[tokio::test]
async fn the_engine_checks_a_pop3_account_at_its_tick() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Never).await;
    let (mut next_poll, mut next_prune, mut stagger) = (Instant::now(), Instant::now(), Duration::ZERO);
    crate::engine::tick(&h.sync, &mut next_poll, &mut next_prune, &mut stagger, &EngineConfig::default()).await.unwrap();
    assert_eq!(h.fake.connects(), 1);
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/u1"]);
    assert!(next_poll > Instant::now() + Duration::from_secs(50), "the next check waits the adapter's minute");
}

async fn first_check_finished(h: &Pop3Harness) -> bool {
    let account_id = h.account_id;
    h.db.read(move |c| pop3::first_check_finished(c, account_id)).await.unwrap()
}

fn five() -> FakePop3 {
    (1..=5).fold(FakePop3::default(), |fake, n| fake.with_message(&format!("u{n}"), &pop3_mail(n)))
}

#[tokio::test]
async fn a_first_check_cut_short_carries_on_as_a_first_check() {
    let h = pop3_harness(five().dropping_after_retrs(2), RemoveSetting::Never).await;
    assert!(h.sync.pop3_check().await.is_err(), "the connection dropped");
    assert!(!first_check_finished(&h).await, "two of five is not the first check done");
    drain(&h);

    let rest = Arc::new(five());
    h.with_server(Arc::clone(&rest)).pop3_check().await.unwrap();
    assert_eq!(rest.retr_calls(), [3, 4, 5], "only what was left");
    let heard = drain(&h);
    assert!(!heard.iter().any(|e| matches!(e, ChangeEvent::NewMail { .. })), "the rest of the old mail raises no notifications");
    for id in ["pop3/u3", "pop3/u4", "pop3/u5"] {
        assert_eq!(meta(&h, id).await.date, 1_609_750_800_000, "{id} keeps its own date");
    }
    assert!(first_check_finished(&h).await);
}

#[tokio::test]
async fn a_finished_first_check_sets_the_marker_and_the_next_check_announces_new_mail() {
    let h = pop3_harness(five(), RemoveSetting::Never).await;
    assert!(!first_check_finished(&h).await);
    h.sync.pop3_check().await.unwrap();
    assert!(first_check_finished(&h).await);
    drain(&h);
    h.fake.add("u6", &pop3_mail(6));
    h.sync.pop3_check().await.unwrap();
    let announced: Vec<String> = drain(&h)
        .into_iter()
        .filter_map(|e| match e {
            ChangeEvent::NewMail { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(announced, ["pop3/u6"]);
}

#[tokio::test]
async fn a_first_check_with_a_refused_message_still_finishes() {
    let h = pop3_harness(five().failing_retr("u2"), RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    assert!(first_check_finished(&h).await, "a refused message is recorded as failed, so the check dealt with it");
}

async fn raw(h: &Pop3Harness, id: &str) -> Option<Vec<u8>> {
    let (account_id, id) = (h.account_id, id.to_string());
    h.db.read(move |c| local_messages::get(c, account_id, &id)).await.unwrap()
}

/// RFC 1939 lets a server give a UIDL to a new message once the old one
/// is gone. The old message may be the only copy left.
#[tokio::test]
async fn a_reused_uidl_keeps_the_old_message_and_stores_the_new_one_beside_it() {
    let fake = FakePop3::default().with_message("u1", &pop3_mail(1)).with_message("u2", &pop3_mail(2));
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    h.fake.take("u1");
    h.sync.pop3_check().await.unwrap();
    h.fake.add("u1", &pop3_mail(3));
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/u1", "pop3/u1/2", "pop3/u2"]);
    assert_eq!(raw(&h, "pop3/u1").await, Some(pop3_mail(1)), "the first message keeps its bytes");
    assert_eq!(raw(&h, "pop3/u1/2").await, Some(pop3_mail(3)));
    assert_eq!(meta(&h, "pop3/u1").await.subject, "Hello 1", "and its row");
}

#[tokio::test]
async fn a_uidl_listed_twice_in_one_session_downloads_once_and_removes_only_that_one() {
    let fake = FakePop3::default().with_message("d", &pop3_mail(1)).with_message("d", &pop3_mail(2));
    let h = pop3_harness(fake, RemoveSetting::Downloaded).await;
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.fake.retr_calls(), [1], "the second listing is skipped");
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, ["pop3/d"]);
    assert_eq!(raw(&h, "pop3/d").await, Some(pop3_mail(1)));
    assert_eq!(h.fake.deleted(), [1], "the DELE goes to the message that came down, not the skipped one");
}

/// The store commits without waiting for the disk (WAL, synchronous
/// NORMAL). Once a DELE goes out, the server may delete its copy at QUIT,
/// so by then the download must be in the database file itself.
#[tokio::test]
async fn a_download_is_in_the_database_file_before_its_dele_goes_out() {
    let store: Arc<std::sync::Mutex<Option<std::path::PathBuf>>> = Arc::default();
    let copies = tempfile::tempdir().unwrap();
    let copy = copies.path().join("at-dele.db");
    let fake = FakePop3::default().with_message("u1", &pop3_mail(1)).on_dele({
        let (store, copy) = (Arc::clone(&store), copy.clone());
        move || {
            if let Some(path) = store.lock().unwrap().as_ref() {
                std::fs::copy(path, &copy).unwrap();
            }
        }
    });
    let h = pop3_harness(fake, RemoveSetting::Downloaded).await;
    *store.lock().unwrap() = Some(h.db_path());
    h.sync.pop3_check().await.unwrap();
    assert_eq!(h.fake.deleted(), [1]);
    // The file without its write-ahead log, as a power cut can leave it.
    let conn = rusqlite::Connection::open(&copy).unwrap();
    let kept: i64 = conn
        .query_row("SELECT COUNT(*) FROM local_messages WHERE message_id = 'pop3/u1'", [], |row| row.get(0))
        .unwrap_or(0);
    assert_eq!(kept, 1, "the downloaded bytes were on disk when the DELE went out");
}

/// Run alone, so other tests add nothing to the process's peak:
/// `cargo test -p mailrs-sync --lib -- --ignored a_first_download_holds`
#[tokio::test]
#[ignore = "a measurement of the whole process, run alone"]
async fn a_first_download_holds_one_message_at_a_time() {
    use crate::tests::heap::ProcessMark;
    let body = "x".repeat(250 * 1024);
    let fake = FakePop3::default();
    for n in 0..200 {
        fake.add(&format!("u{n:03}"), format!("Subject: {n}\r\n\r\n{body}\r\n").as_bytes());
    }
    let h = pop3_harness(fake, RemoveSetting::Never).await;
    let mark = ProcessMark::start();
    h.sync.pop3_check().await.unwrap();
    let peak = mark.peak();
    eprintln!("200 messages of 250 KB held {peak} bytes at the peak");
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await.len(), 200);
    assert!(peak < 16 << 20, "the check held {peak} bytes; 200 messages come to 50 MB");
}
