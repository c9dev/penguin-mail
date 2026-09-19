#![allow(dead_code)]

use mailrs_domain::{AccountId, Address, MessageMeta};
use mailrs_store::{accounts, messages};
use rusqlite::Connection;

pub fn db() -> (Connection, AccountId) {
    let conn = mailrs_store::open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    (conn, id)
}

pub fn meta(
    account_id: AccountId,
    id: &str,
    thread: &str,
    date: i64,
    labels: &[&str],
) -> MessageMeta {
    MessageMeta {
        account_id,
        id: id.into(),
        thread_id: thread.into(),
        rfc822_msgid: Some(format!("<{id}@example.com>")),
        from: Some(Address {
            name: Some(format!("Sender {id}")),
            email: format!("{id}@example.com"),
        }),
        to: vec![Address {
            name: None,
            email: "me@example.com".into(),
        }],
        cc: vec![],
        subject: format!("Subject {id}"),
        date,
        snippet: format!("snippet {id}"),
        size: 100,
        has_attachments: false,
        label_ids: labels.iter().map(|l| l.to_string()).collect(),
    }
}

/// Upserts at generation 1 and refreshes the touched threads.
pub fn store(conn: &Connection, metas: &[MessageMeta]) {
    for m in metas {
        messages::upsert_message(conn, m, 1).unwrap();
    }
    for m in metas {
        messages::refresh_thread(conn, m.account_id, &m.thread_id).unwrap();
    }
}

/// Two accounts with inbox mail in several categories, a sent thread, one
/// thread in Trash, and two starred threads, one of them in Spam.
pub fn mixed_mail() -> (Connection, AccountId, AccountId) {
    let conn = mailrs_store::open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
    store(
        &conn,
        &[
            meta(a, "a1", "ta1", 100, &["INBOX", "CATEGORY_PERSONAL"]),
            meta(
                a,
                "a2",
                "ta2",
                200,
                &["INBOX", "UNREAD", "CATEGORY_UPDATES"],
            ),
            meta(a, "a3", "ta3", 300, &["INBOX", "UNREAD", "CATEGORY_SOCIAL"]),
            meta(a, "a4", "ta4", 400, &["SENT"]),
            meta(a, "a5", "ta5", 500, &["INBOX", "UNREAD", "TRASH"]),
            meta(b, "b1", "tb1", 600, &["INBOX", "UNREAD", "CATEGORY_FORUMS"]),
            meta(b, "b2", "tb2", 700, &["INBOX", "STARRED"]),
            meta(b, "b3", "tb3", 800, &["INBOX", "STARRED", "SPAM"]),
        ],
    );
    (conn, a, b)
}
