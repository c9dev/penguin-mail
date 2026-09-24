//! Local threading: the thread a message from a server without threads
//! joins, found by Message-ID, then References and In-Reply-To, then the
//! same subject within 30 days.

mod common;

use common::{db, meta};
use mailrs_domain::{AccountId, MessageMeta};
use mailrs_store::messages::{self, Change};
use mailrs_store::threading::Links;
use rusqlite::Connection;

const DAY: i64 = 24 * 60 * 60 * 1000;

fn message(account: AccountId, id: &str, subject: &str, at: i64) -> MessageMeta {
    let mut m = meta(account, id, "unset", at, &["INBOX"]);
    m.subject = subject.into();
    m.rfc822_msgid = Some(format!("<{id}@example.com>"));
    m
}

fn links(in_reply_to: Option<&str>, references: &[&str]) -> Links {
    Links {
        in_reply_to: in_reply_to.map(|id| format!("<{id}@example.com>")),
        references: references.iter().map(|id| format!("<{id}@example.com>")).collect(),
    }
}

fn thread_local(conn: &Connection, account: AccountId, m: MessageMeta, links: Links) -> String {
    let id = m.id.clone();
    messages::apply(
        conn,
        account,
        &[Change::UpsertLocal {
            meta: Box::new(m),
            generation: 1,
            links,
        }],
    )
    .unwrap();
    messages::thread_id_of(conn, account, &id).unwrap().unwrap()
}

#[test]
fn a_reply_joins_its_thread_by_references() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    let reply = thread_local(&conn, a, message(a, "b", "Something else", DAY), links(None, &["x", "a"]));
    assert_eq!(first, "a", "a message that links nowhere starts a thread named after itself");
    assert_eq!(reply, first);
}

#[test]
fn a_reply_joins_by_in_reply_to_alone() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    let reply = thread_local(&conn, a, message(a, "b", "Other", DAY), links(Some("a"), &[]));
    assert_eq!(reply, first);
}

#[test]
fn a_reply_joins_by_subject_inside_thirty_days() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    let reply = thread_local(&conn, a, message(a, "b", "Re: Lunch", 29 * DAY), Links::default());
    assert_eq!(reply, first);
}

#[test]
fn two_unrelated_messages_with_one_subject_forty_days_apart_stay_apart() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    let later = thread_local(&conn, a, message(a, "b", "Re: Lunch", 40 * DAY), Links::default());
    assert_ne!(later, first);
    assert_eq!(later, "b");
}

#[test]
fn a_subject_without_a_reply_prefix_does_not_join() {
    let (conn, a) = db();
    thread_local(&conn, a, message(a, "a", "Invoice", 0), Links::default());
    let other = thread_local(&conn, a, message(a, "b", "Invoice", DAY), Links::default());
    assert_eq!(other, "b");
}

#[test]
fn a_parent_that_arrives_after_its_reply_joins_the_reply() {
    let (conn, a) = db();
    let reply = thread_local(&conn, a, message(a, "b", "Re: Lunch", DAY), links(Some("a"), &["a"]));
    let parent = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    assert_eq!(parent, reply);
}

#[test]
fn a_second_copy_of_a_message_joins_its_thread() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    let mut copy = message(a, "a-copy", "Lunch", 0);
    copy.rfc822_msgid = Some("<a@example.com>".into());
    assert_eq!(thread_local(&conn, a, copy, Links::default()), first);
}

#[test]
fn a_message_stored_again_keeps_its_thread() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    thread_local(&conn, a, message(a, "b", "Re: Lunch", DAY), Links::default());
    let again = thread_local(&conn, a, message(a, "b", "Re: Lunch", DAY), links(Some("zzz"), &[]));
    assert_eq!(again, first);
}

// Ruling 12 (progress.md): extended so the test fails without UpsertLocal's
// threading. As first written it exercised only Change::Upsert, which
// never touches local threading, so it passed before this task's code
// existed.
#[test]
fn a_message_with_a_server_thread_keeps_it() {
    let (conn, a) = db();
    let first = thread_local(&conn, a, message(a, "a", "Lunch", 0), Links::default());
    let mut gmail = message(a, "g", "Re: Lunch", DAY);
    gmail.thread_id = "gmail-thread".into();
    messages::apply(&conn, a, &[Change::Upsert { meta: Box::new(gmail), generation: 1 }]).unwrap();
    assert_eq!(
        messages::thread_id_of(&conn, a, "g").unwrap().as_deref(),
        Some("gmail-thread")
    );
    // A local message with the same subject joins the local thread, never
    // Gmail's: Gmail's own message carries no base subject, since nothing
    // threaded it here.
    let local = thread_local(&conn, a, message(a, "b", "Re: Lunch", 2 * DAY), Links::default());
    assert_eq!(local, first);
}
