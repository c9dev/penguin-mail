mod common;

use common::{db, meta, store};
use mailrs_domain::{AccountId, ThreadSummary};
use mailrs_store::threads::{self, ThreadFilter};

const OTHERS: [&str; 4] = [
    "CATEGORY_SOCIAL",
    "CATEGORY_UPDATES",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_FORUMS",
];

fn ids(rows: Vec<ThreadSummary>) -> Vec<String> {
    rows.into_iter().map(|t| t.id).collect()
}

fn inbox() -> (rusqlite::Connection, AccountId) {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "p1", "personal", 100, &["INBOX", "CATEGORY_PERSONAL"]),
            meta(
                id,
                "u1",
                "bank",
                200,
                &["INBOX", "UNREAD", "CATEGORY_UPDATES"],
            ),
            meta(id, "s1", "party", 300, &["INBOX", "CATEGORY_SOCIAL"]),
            meta(
                id,
                "f1",
                "forum",
                400,
                &["INBOX", "UNREAD", "CATEGORY_FORUMS"],
            ),
            meta(id, "d1", "deal", 500, &["INBOX", "CATEGORY_PROMOTIONS"]),
            meta(id, "o1", "old", 600, &["CATEGORY_UPDATES"]),
            meta(id, "n1", "plain", 700, &["INBOX", "UNREAD"]),
        ],
    );
    (conn, id)
}

#[test]
fn primary_leaves_out_every_other_category() {
    let (conn, _) = inbox();
    let primary = ThreadFilter::unified("INBOX").with_labels(&[], &OTHERS);
    assert_eq!(
        ids(threads::list_threads(&conn, &primary, 0, 10).unwrap()),
        ["plain", "personal"]
    );
    assert_eq!(threads::unread_threads(&conn, &primary).unwrap(), 1);
}

#[test]
fn social_takes_forums_too() {
    let (conn, _) = inbox();
    let social =
        ThreadFilter::unified("INBOX").with_labels(&["CATEGORY_SOCIAL", "CATEGORY_FORUMS"], &[]);
    assert_eq!(
        ids(threads::list_threads(&conn, &social, 0, 10).unwrap()),
        ["forum", "party"]
    );
    assert_eq!(threads::unread_threads(&conn, &social).unwrap(), 1);
}

#[test]
fn a_category_stays_inside_the_mailbox() {
    let (conn, id) = inbox();
    let updates = ThreadFilter::account(id, "INBOX").with_labels(&["CATEGORY_UPDATES"], &[]);
    assert_eq!(
        ids(threads::list_threads(&conn, &updates, 0, 10).unwrap()),
        ["bank"]
    );
    assert_eq!(threads::count_threads(&conn, &updates).unwrap(), 1);
}

#[test]
fn message_listings_filter_by_category_per_message() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "a1", "t", 100, &["INBOX", "CATEGORY_PROMOTIONS"]),
            meta(id, "a2", "t", 200, &["INBOX", "UNREAD"]),
        ],
    );
    let promotions = ThreadFilter::unified("INBOX").with_labels(&["CATEGORY_PROMOTIONS"], &[]);
    let rows = threads::list_messages(&conn, &promotions, 0, 10).unwrap();
    let messages: Vec<Option<String>> = rows.into_iter().map(|r| r.message_id).collect();
    assert_eq!(messages, [Some("a1".to_string())]);
    let primary = ThreadFilter::unified("INBOX").with_labels(&[], &OTHERS);
    assert_eq!(threads::unread_messages(&conn, &primary).unwrap(), 1);
}
