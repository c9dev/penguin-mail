use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{Account, AccountState, Folder, MessageBody};
use mailrs_store::messages;

use super::harness;
use crate::fake::meta;
use crate::mailbox::{Mailbox, Mailboxes, Scope, View};
use crate::now_millis;

const DAY: i64 = 24 * 60 * 60 * 1000;

#[tokio::test]
async fn opening_a_thread_pulls_messages_outside_the_window() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("recent", "t1", now, &["INBOX"]));
    h.fake.seed(meta("older", "t1", now - 60 * DAY, &[]));
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
async fn opening_a_thread_announces_a_label_change_but_not_a_new_label_order() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["UNREAD", "INBOX"]));
    h.bootstrap_all().await;
    h.sync.ensure_thread("t1").await.unwrap();
    h.drain();

    // Gmail lists the same labels in another order: nothing changed.
    let relabel = |labels: &[&str]| {
        h.fake.with(|s| {
            s.messages.get_mut("a").unwrap().label_ids =
                labels.iter().map(ToString::to_string).collect();
        });
    };
    relabel(&["INBOX", "UNREAD", "INBOX"]);
    h.sync.ensure_thread("t1").await.unwrap();
    assert!(h.drain().is_empty());

    relabel(&["INBOX"]);
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(h.drain().len(), 1);
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
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
async fn opening_a_thread_fetched_whole_right_after_history_asks_gmail_nothing() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("recent", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    h.sync.ensure_thread("t1").await.unwrap();
    // A history replay with nothing to report speaks for the whole mailbox.
    h.sync.incremental().await.unwrap();
    h.fake.reset_usage();
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(
        h.fake.usage().by_method.get("users.threads.get"),
        None,
        "the open should trust the store and fetch nothing"
    );
}

#[tokio::test]
async fn a_thread_the_window_holds_in_part_is_fetched_whole_on_opening() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("recent", "t1", now, &["INBOX"]));
    h.fake.seed(meta("older", "t1", now - 60 * DAY, &[]));
    h.bootstrap_all().await;
    h.sync.incremental().await.unwrap();
    // The window stored only the recent message. A fresh replay says the
    // stored messages are current, not that the thread is all there.
    h.sync.ensure_thread("t1").await.unwrap();
    assert_eq!(h.thread("t1").await.unwrap().message_count, 2);
}

#[tokio::test]
async fn opening_a_thread_keeps_a_change_history_brought_while_gmail_answered() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let mut held = h.fake.hold("users.threads.get");
    let opening = h.sync.ensure_thread("t1");
    // Gmail read the thread with the message still in the inbox, then the
    // message was archived and a replay stored that before the answer came.
    let meanwhile = async {
        held.entered().await;
        h.fake.remote_relabel("a", &[], &["INBOX"]);
        h.sync.incremental().await.unwrap();
        held.release();
    };
    let (opened, ()) = tokio::join!(opening, meanwhile);
    opened.unwrap();
    assert_eq!(h.labels_of("a").await, Vec::<String>::new());
}

/// Lists the Trash through `Mailboxes`, as the window does.
async fn list_trash(h: &super::Harness) -> Vec<String> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let lists = Mailboxes::new(Arc::new(super::Connected(connected)), h.db.clone());
    let scope = Scope::over([Account {
        id: h.account_id,
        email: "me@example.com".into(),
        state: AccountState::Ok,
    }]);
    let trash = Mailbox::Folder {
        account_id: None,
        folder: Folder::Trash,
    };
    lists
        .list(&trash, &scope, &View::default(), 0)
        .await
        .expect("the Trash lists")
        .rows
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// An old conversation outside the window: two trashed messages and a
/// third that the Trash does not list.
async fn trashed_thread() -> super::Harness {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("recent", "t1", now, &["INBOX"]));
    h.fake.seed(meta("first", "t9", now - 90 * DAY, &[]));
    h.fake
        .seed(meta("second", "t9", now - 89 * DAY, &["TRASH"]));
    h.fake.seed(meta("third", "t9", now - 88 * DAY, &["TRASH"]));
    h.bootstrap_all().await;
    h.fake.with(|s| s.page_size = 1000);
    h
}

#[tokio::test]
async fn opening_a_thread_a_search_fetched_whole_asks_gmail_nothing() {
    let h = trashed_thread().await;
    h.sync.incremental().await.unwrap();
    h.fake.reset_usage();

    assert_eq!(list_trash(&h).await, ["t9"]);
    let listed = h.fake.usage();
    // Two hits in one thread cost one threads.get, no more than their two
    // messages.get calls would.
    assert_eq!(listed.calls_to("users.threads.get"), 1);
    assert_eq!(listed.calls_to("users.messages.get"), 0);
    assert!(h.thread("t9").await.is_none(), "listing stores nothing");
    h.fake.reset_usage();

    h.sync.open_thread("t9").await.unwrap();

    assert_eq!(h.fake.usage().calls_to("users.threads.get"), 0);
    assert_eq!(h.thread("t9").await.unwrap().message_count, 3);
}

#[tokio::test]
async fn a_search_copy_that_history_moved_past_is_fetched_again() {
    let h = trashed_thread().await;
    h.sync.incremental().await.unwrap();
    assert_eq!(list_trash(&h).await, ["t9"]);
    // The first message is starred in the browser. The store lacks t9, so
    // the replay moves the cursor past the change without storing it.
    h.fake.remote_relabel("first", &["STARRED"], &[]);
    h.sync.incremental().await.unwrap();
    h.fake.reset_usage();

    h.sync.open_thread("t9").await.unwrap();

    assert_eq!(h.fake.usage().calls_to("users.threads.get"), 1);
    assert_eq!(h.labels_of("first").await, ["STARRED"]);
}

/// The body cache keeps to its cap, though it no longer sums every stored
/// body each time one arrives.
#[tokio::test]
async fn the_body_cache_keeps_to_its_cap() {
    let h = harness().await;
    let now = now_millis();
    for id in ["a", "b", "c"] {
        h.fake.seed(meta(id, &format!("t{id}"), now, &["INBOX"]));
        h.fake.with(|s| {
            s.bodies.insert(
                id.into(),
                mailrs_domain::MessageBody {
                    text: Some("x".repeat(600)),
                    ..Default::default()
                },
            )
        });
    }
    h.bootstrap_all().await;
    let sync = h
        .sync_with(std::time::Duration::from_secs(1))
        .with_limits(30, 1000);

    for id in ["a", "b", "c"] {
        sync.body(id).await.unwrap();
    }

    let kept: i64 =
        h.db.read(|c| {
            Ok(c.query_row("SELECT TOTAL(size) FROM bodies", [], |r| r.get::<_, f64>(0))? as i64)
        })
        .await
        .unwrap();
    assert!(kept <= 1000, "{kept} bytes kept");
}
