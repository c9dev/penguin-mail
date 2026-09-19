mod common;

use common::{meta, store};
use mailrs_domain::{AccountId, ThreadSummary};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{accounts, open_in_memory};
use rusqlite::Connection;

fn two_accounts() -> (Connection, AccountId, AccountId) {
    let conn = open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
    store(
        &conn,
        &[
            meta(a, "a1", "ta1", 100, &["INBOX"]),
            meta(b, "b1", "tb1", 300, &["INBOX", "UNREAD"]),
            meta(a, "a2", "ta2", 200, &["INBOX", "UNREAD"]),
            meta(b, "b2", "tb2", 400, &["SENT"]),
        ],
    );
    (conn, a, b)
}

fn ids(rows: Vec<ThreadSummary>) -> Vec<String> {
    rows.into_iter().map(|t| t.id).collect()
}

#[test]
fn the_unified_inbox_merges_accounts_newest_first() {
    let (conn, _, _) = two_accounts();
    let inbox = ThreadFilter::unified("INBOX");
    assert_eq!(
        ids(threads::list_threads(&conn, &inbox, 0, 10).unwrap()),
        ["tb1", "ta2", "ta1"]
    );
    assert_eq!(threads::count_threads(&conn, &inbox).unwrap(), 3);
    assert_eq!(threads::unread_threads(&conn, &inbox).unwrap(), 2);
}

#[test]
fn account_views_only_show_their_account() {
    let (conn, a, b) = two_accounts();
    assert_eq!(
        ids(threads::list_threads(&conn, &ThreadFilter::account(a, "INBOX"), 0, 10).unwrap()),
        ["ta2", "ta1"]
    );
    assert_eq!(
        ids(threads::list_threads(&conn, &ThreadFilter::account(b, "SENT"), 0, 10).unwrap()),
        ["tb2"]
    );
    assert_eq!(
        threads::unread_threads(&conn, &ThreadFilter::account(a, "INBOX")).unwrap(),
        1
    );
}

#[test]
fn paging_walks_the_list_without_gaps() {
    let (conn, _, _) = two_accounts();
    let inbox = ThreadFilter::unified("INBOX");
    let mut seen = Vec::new();
    for offset in 0..3 {
        seen.extend(ids(threads::list_threads(&conn, &inbox, offset, 1).unwrap()));
    }
    assert_eq!(seen, ["tb1", "ta2", "ta1"]);
    assert!(
        threads::list_threads(&conn, &inbox, 3, 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_thread_carries_every_label_of_its_messages() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    store(
        &conn,
        &[
            meta(id, "a", "t1", 100, &["INBOX"]),
            meta(id, "b", "t1", 200, &["SENT"]),
        ],
    );
    assert_eq!(
        ids(threads::list_threads(&conn, &ThreadFilter::account(id, "INBOX"), 0, 10).unwrap()),
        ["t1"]
    );
    assert_eq!(
        ids(threads::list_threads(&conn, &ThreadFilter::account(id, "SENT"), 0, 10).unwrap()),
        ["t1"]
    );
}

#[test]
fn ungrouped_lists_show_each_message() {
    let (conn, a, _) = two_accounts();
    store(
        &conn,
        &[meta(a, "a3", "ta1", 250, &["INBOX", "UNREAD", "STARRED"])],
    );
    let inbox = ThreadFilter::account(a, "INBOX");
    let rows = threads::list_messages(&conn, &inbox, 0, 10).unwrap();
    let ids: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.id.as_str(), r.message_id.as_deref()))
        .collect();
    assert_eq!(
        ids,
        [
            ("ta1", Some("a3")),
            ("ta2", Some("a2")),
            ("ta1", Some("a1"))
        ]
    );
    assert!(rows[0].unread && rows[0].starred);
    assert_eq!(rows[0].message_count, 1);
    assert_eq!(threads::unread_messages(&conn, &inbox).unwrap(), 2);
    assert!(
        threads::list_threads(&conn, &inbox, 0, 10)
            .unwrap()
            .iter()
            .all(|t| t.message_id.is_none())
    );
}
