use mailrs_domain::MessageBody;
use mailrs_store::messages;

use super::harness;
use crate::fake::meta;
use crate::now_millis;

const DAY: i64 = 24 * 60 * 60 * 1000;

#[tokio::test]
async fn opening_a_thread_pulls_messages_outside_the_window() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("recent", "t1", now, &["INBOX"]));
    h.fake
        .seed_outside_window(meta("older", "t1", now - 60 * DAY, &[]));
    h.bootstrap_all().await;
    assert_eq!(h.thread("t1").await.unwrap().message_count, 1);
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(h.thread("t1").await.unwrap().message_count, 2);
    let ids: Vec<String> =
        h.db.read(|c| messages::thread_messages(c, 1, "t1"))
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
    assert_eq!(ids, ["older", "recent"]);
}

#[tokio::test]
async fn opening_a_thread_gmail_no_longer_has_removes_it() {
    let h = harness().await;
    h.fake.seed(meta("recent", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.remote_delete_silently("recent");
    h.sync.ensure_thread("t1").await.unwrap();
    assert!(h.thread("t1").await.is_none());
}

#[tokio::test]
async fn bodies_are_fetched_once_then_served_from_the_cache() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let body = MessageBody {
        html: None,
        text: Some("hello".into()),
        ..Default::default()
    };
    h.fake.with(|s| {
        s.bodies.insert("a".into(), body.clone());
    });
    assert_eq!(h.sync.body("a").await.unwrap(), body);
    assert_eq!(h.sync.body("a").await.unwrap(), body);
    assert_eq!(h.fake.with(|s| s.body_fetches), 1);
}

#[tokio::test]
async fn bodies_of_unstored_messages_are_not_cached() {
    let h = harness().await;
    h.fake.with(|s| {
        s.bodies.insert("x".into(), MessageBody::default());
    });
    h.sync.body("x").await.unwrap();
    h.sync.body("x").await.unwrap();
    assert_eq!(h.fake.with(|s| s.body_fetches), 2);
}

#[tokio::test]
async fn opening_an_unchanged_thread_announces_nothing() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.sync.ensure_thread("t1").await.unwrap();
    h.drain();
    h.sync.ensure_thread("t1").await.unwrap();
    assert!(h.drain().is_empty());

    h.fake.seed(meta("b", "t1", now_millis(), &["INBOX"]));
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(h.drain().len(), 1);
    h.sync.ensure_thread("t1").await.unwrap();
    assert!(h.drain().is_empty());

    h.fake.remote_delete_silently("a");
    h.fake.remote_delete_silently("b");
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(h.drain().len(), 1);
    h.sync.ensure_thread("t1").await.unwrap();
    assert!(h.drain().is_empty());
}

#[tokio::test]
async fn reading_a_cached_body_records_the_read() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.with(|s| {
        s.bodies.insert("a".into(), MessageBody::default());
    });
    h.sync.body("a").await.unwrap();
    let stored = h.accessed_at("a").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    h.sync.body("a").await.unwrap();
    let mut read_again = h.accessed_at("a").await;
    for _ in 0..100 {
        if read_again > stored {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        read_again = h.accessed_at("a").await;
    }
    assert!(read_again > stored, "{read_again} is not after {stored}");
}

#[tokio::test]
async fn opening_a_thread_right_after_history_asks_gmail_nothing() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("recent", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    // A history replay with nothing to report speaks for the whole mailbox.
    h.sync.incremental().await.unwrap();
    // Gmail gains a message that history has not announced yet.
    h.fake
        .seed_outside_window(meta("older", "t1", now - 60 * DAY, &[]));
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(
        h.thread("t1").await.unwrap().message_count,
        1,
        "the open should trust the store and fetch nothing"
    );
}
