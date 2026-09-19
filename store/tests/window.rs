mod common;

use common::{db, meta, store};
use mailrs_store::{messages, threads, window};

#[test]
fn pruning_drops_old_threads_outside_the_inbox() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "old", "told", 100, &[]),
            meta(id, "oldinbox", "toldinbox", 100, &["INBOX"]),
            meta(id, "new", "tnew", 5_000, &[]),
        ],
    );
    assert_eq!(window::prune_window(&conn, id, 1_000).unwrap(), ["told"]);
    assert!(threads::get_thread(&conn, id, "told").unwrap().is_none());
    assert!(messages::thread_messages(&conn, id, "told").unwrap().is_empty());
    assert!(threads::get_thread(&conn, id, "toldinbox").unwrap().is_some());
    assert!(threads::get_thread(&conn, id, "tnew").unwrap().is_some());
}

#[test]
fn a_thread_with_a_recent_message_survives_pruning() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &[]), meta(id, "b", "t1", 5_000, &[])]);
    assert!(window::prune_window(&conn, id, 1_000).unwrap().is_empty());
    assert_eq!(threads::get_thread(&conn, id, "t1").unwrap().unwrap().message_count, 2);
}

#[test]
fn sweeping_removes_messages_from_older_generations() {
    let (conn, id) = db();
    messages::upsert_message(&conn, &meta(id, "stale", "t1", 100, &["INBOX"]), 1).unwrap();
    messages::upsert_message(&conn, &meta(id, "fresh", "t1", 200, &["INBOX"]), 2).unwrap();
    messages::upsert_message(&conn, &meta(id, "gone", "t2", 300, &["INBOX"]), 1).unwrap();
    for thread in ["t1", "t2"] {
        messages::refresh_thread(&conn, id, thread).unwrap();
    }
    assert_eq!(window::sweep_stale(&conn, id, 2).unwrap(), ["t1", "t2"]);
    assert_eq!(threads::get_thread(&conn, id, "t1").unwrap().unwrap().message_count, 1);
    assert!(threads::get_thread(&conn, id, "t2").unwrap().is_none());
}
