use mailrs_domain::{AccountState, ChangeEvent};
use mailrs_store::{accounts, labels};

use super::harness;
use crate::fake::meta;
use crate::now_millis;

const DAY: i64 = 24 * 60 * 60 * 1000;

#[tokio::test]
async fn bootstrap_records_the_cursor_labels_and_first_page() {
    let h = harness().await;
    let now = now_millis();
    for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
        h.fake.seed(meta(id, &format!("t{id}"), now - i as i64 * 1000, &["INBOX"]));
    }
    h.sync.bootstrap().await.unwrap();

    let cursor = h.cursor().await;
    assert_eq!(cursor.history_id, Some(100));
    assert_eq!(cursor.backfill_cursor.as_deref(), Some("2"));
    assert!(!cursor.backfill_done);
    assert_eq!(h.threads("INBOX").await, ["ta", "tb"]);
    assert_eq!(h.db.read(|c| labels::list_labels(c, 1)).await.unwrap().len(), 4);
    let events = h.drain();
    assert!(events.contains(&ChangeEvent::LabelsChanged { account_id: 1 }));
    assert_eq!(events.last(), Some(&ChangeEvent::AccountStateChanged { account_id: 1, state: AccountState::Ok }));
}

#[tokio::test]
async fn backfill_finishes_the_window_then_stops() {
    let h = harness().await;
    let now = now_millis();
    for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
        h.fake.seed(meta(id, &format!("t{id}"), now - i as i64 * 1000, &["INBOX"]));
    }
    h.sync.bootstrap().await.unwrap();
    assert!(!h.sync.backfill_step().await.unwrap());
    assert_eq!(h.threads("INBOX").await, ["ta", "tb", "tc"]);
    assert!(h.cursor().await.backfill_done);
    assert!(!h.sync.backfill_step().await.unwrap());
}

#[tokio::test]
async fn a_rejected_page_token_restarts_the_listing() {
    let h = harness().await;
    let now = now_millis();
    for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
        h.fake.seed(meta(id, &format!("t{id}"), now - i as i64 * 1000, &["INBOX"]));
    }
    h.sync.bootstrap().await.unwrap();
    h.db.write(|c| accounts::set_backfill(c, 1, Some("bad"), false)).await.unwrap();
    assert!(h.sync.backfill_step().await.unwrap());
    assert_eq!(h.cursor().await.backfill_cursor, None);
    while h.sync.backfill_step().await.unwrap() {}
    assert_eq!(h.threads("INBOX").await.len(), 3);
}

#[tokio::test]
async fn pruning_drops_old_threads_that_left_the_inbox() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("old", "told", now - 40 * DAY, &[]));
    h.fake.seed(meta("pinned", "tpinned", now - 40 * DAY, &["INBOX"]));
    h.fake.seed(meta("recent", "trecent", now - DAY, &[]));
    h.bootstrap_all().await;
    h.sync.prune(now).await.unwrap();
    assert!(h.thread("told").await.is_none());
    assert!(h.thread("tpinned").await.is_some());
    assert!(h.thread("trecent").await.is_some());
    assert!(h.drain().contains(&ChangeEvent::ThreadsChanged { account_id: 1, thread_ids: vec!["told".into()] }));
}
