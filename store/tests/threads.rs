mod common;

use common::{meta, mixed_mail, store};
use mailrs_domain::{AccountId, ThreadSummary};
use mailrs_store::messages::Change;
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{accounts, messages, open_in_memory};
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

const OTHERS: [&str; 4] = [
    "CATEGORY_SOCIAL",
    "CATEGORY_UPDATES",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_FORUMS",
];

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
fn a_unified_filter_narrows_to_one_account() {
    let (conn, a, _) = two_accounts();
    let narrowed = ThreadFilter::unified("INBOX").in_account(a);
    assert_eq!(narrowed, ThreadFilter::account(a, "INBOX"));
    assert_eq!(
        ids(threads::list_threads(&conn, &narrowed, 0, 10).unwrap()),
        ["ta2", "ta1"]
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
fn paging_past_the_last_row_walks_the_list_without_gaps() {
    let (conn, _, _) = two_accounts();
    let inbox = ThreadFilter::unified("INBOX");
    let mut seen = Vec::new();
    let mut last = None;
    for _ in 0..3 {
        let page = threads::list_threads_after(&conn, &inbox, last.as_ref(), 1).unwrap();
        last = page.last().cloned();
        seen.extend(ids(page));
    }
    assert_eq!(seen, ["tb1", "ta2", "ta1"]);
    assert!(
        threads::list_threads_after(&conn, &inbox, last.as_ref(), 1)
            .unwrap()
            .is_empty()
    );
    let mut messages = Vec::new();
    let mut last = None;
    loop {
        let page = threads::list_messages_after(&conn, &inbox, last.as_ref(), 2).unwrap();
        last = page.last().cloned();
        let done = page.len() < 2;
        messages.extend(page.into_iter().map(|row| row.message_id.unwrap()));
        if done {
            break;
        }
    }
    let by_offset: Vec<String> = threads::list_messages(&conn, &inbox, 0, 100)
        .unwrap()
        .into_iter()
        .map(|row| row.message_id.unwrap())
        .collect();
    assert_eq!(messages, by_offset);
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

#[test]
fn a_label_view_of_one_account_leaves_the_others_out() {
    let (conn, a, b) = mixed_mail();
    let ids = |filter: &ThreadFilter| -> Vec<String> {
        threads::list_threads(&conn, filter, 0, 10)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect()
    };
    // tb3 is in Spam and ta5 is in the Trash, so a label view drops them.
    assert_eq!(ids(&ThreadFilter::account(b, "INBOX")), ["tb2", "tb1"]);
    assert_eq!(
        ids(&ThreadFilter::unified("INBOX")),
        ["tb2", "tb1", "ta3", "ta2", "ta1"]
    );
    // Any mail leaves out Trash and Spam, in both account and unified views.
    assert_eq!(
        ids(&ThreadFilter::unified("")),
        ["tb2", "tb1", "ta4", "ta3", "ta2", "ta1"]
    );
    assert_eq!(
        ids(&ThreadFilter::account(a, "")),
        ["ta4", "ta3", "ta2", "ta1"]
    );
    // A category narrows a label view without losing the account.
    let social = ThreadFilter::account(b, "INBOX").with_labels(&["CATEGORY_FORUMS"], &[]);
    assert_eq!(ids(&social), ["tb1"]);
    let primary = ThreadFilter::unified("INBOX").with_labels(&[], &OTHERS);
    assert_eq!(ids(&primary), ["tb2", "ta1"]);
}

#[test]
fn trashing_sent_mail_takes_it_out_of_the_sent_list_and_its_count() {
    let (conn, a) = common::db();
    store(
        &conn,
        &[
            meta(a, "s1", "ts1", 100, &["SENT"]),
            meta(a, "s2", "ts2", 200, &["SENT"]),
        ],
    );
    let sent = ThreadFilter::account(a, "SENT");
    assert_eq!(
        ids(threads::list_threads(&conn, &sent, 0, 10).unwrap()),
        ["ts2", "ts1"]
    );
    assert_eq!(threads::count_threads(&conn, &sent).unwrap(), 2);

    // The Delete key moves the newer one to the Trash, as Gmail does.
    messages::apply(&conn, a, &[Change::label("s2", "TRASH", true)]).unwrap();

    assert_eq!(
        ids(threads::list_threads(&conn, &sent, 0, 10).unwrap()),
        ["ts1"]
    );
    assert_eq!(threads::count_threads(&conn, &sent).unwrap(), 1);
    assert_eq!(
        threads::label_counts(&conn)
            .unwrap()
            .account(a, "SENT")
            .threads,
        1,
        "the sidebar count agrees with the list"
    );
    // The Trash list is a Gmail search, but the label itself still holds it.
    let trash = ThreadFilter::account(a, "TRASH");
    assert_eq!(
        ids(threads::list_threads(&conn, &trash, 0, 10).unwrap()),
        ["ts2"]
    );
    assert_eq!(
        threads::label_counts(&conn)
            .unwrap()
            .account(a, "TRASH")
            .threads,
        1
    );
}

#[test]
fn spam_leaves_a_label_list_and_a_flagged_list() {
    let (conn, a) = common::db();
    store(
        &conn,
        &[
            meta(a, "j1", "tj1", 100, &["Label_1", "STARRED", "UNREAD"]),
            meta(
                a,
                "j2",
                "tj2",
                200,
                &["Label_1", "STARRED", "UNREAD", "SPAM"],
            ),
        ],
    );
    for label in ["Label_1", "STARRED"] {
        let filter = ThreadFilter::account(a, label);
        assert_eq!(
            ids(threads::list_threads(&conn, &filter, 0, 10).unwrap()),
            ["tj1"],
            "{label}"
        );
        assert_eq!(
            threads::unread_threads(&conn, &filter).unwrap(),
            1,
            "{label}"
        );
        assert_eq!(
            threads::label_counts(&conn).unwrap().account(a, label),
            threads::Count {
                threads: 1,
                unread: 1
            },
            "{label}"
        );
    }
    let spam = ThreadFilter::account(a, "SPAM");
    assert_eq!(
        ids(threads::list_threads(&conn, &spam, 0, 10).unwrap()),
        ["tj2"]
    );
}

/// Gmail keeps a conversation in the inbox while one of its messages
/// outside the Trash is there, so deleting the start of a thread does not
/// hide the reply that arrived after it.
#[test]
fn a_reply_in_the_inbox_shows_its_thread_when_earlier_messages_are_in_the_trash() {
    let (conn, a) = common::db();
    store(
        &conn,
        &[
            meta(a, "r1", "tr", 100, &["TRASH"]),
            meta(a, "r2", "tr", 200, &["TRASH"]),
            meta(a, "r3", "tr", 300, &["INBOX", "UNREAD"]),
            meta(a, "g1", "tg", 150, &["INBOX", "TRASH"]),
        ],
    );
    let inbox = ThreadFilter::account(a, "INBOX");
    assert_eq!(
        ids(threads::list_threads(&conn, &inbox, 0, 10).unwrap()),
        ["tr"],
        "a thread whose only inbox message is trashed stays out"
    );
    assert_eq!(threads::count_threads(&conn, &inbox).unwrap(), 1);
    assert_eq!(
        threads::label_counts(&conn)
            .unwrap()
            .account(a, "INBOX")
            .threads,
        1,
        "the sidebar count agrees with the list"
    );
    let any = ThreadFilter::account(a, "");
    assert_eq!(
        ids(threads::list_threads(&conn, &any, 0, 10).unwrap()),
        ["tr"],
        "any mail shows a thread with one message outside the Trash"
    );
}

#[test]
fn a_muted_thread_says_so_in_its_row() {
    let (conn, a) = common::db();
    store(
        &conn,
        &[
            meta(a, "m1", "t1", 100, &["MUTE"]),
            meta(a, "m2", "t2", 200, &["INBOX"]),
        ],
    );
    let muted =
        |rows: Vec<ThreadSummary>| -> Vec<bool> { rows.into_iter().map(|t| t.muted).collect() };
    let all = ThreadFilter::unified("");
    assert_eq!(
        muted(threads::list_threads(&conn, &all, 0, 10).unwrap()),
        [false, true]
    );
    assert_eq!(
        muted(threads::list_messages(&conn, &all, 0, 10).unwrap()),
        [false, true]
    );
    assert_eq!(
        ids(threads::list_threads(&conn, &ThreadFilter::unified("MUTE"), 0, 10).unwrap()),
        ["t1"]
    );
}
