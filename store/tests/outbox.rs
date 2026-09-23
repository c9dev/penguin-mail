mod common;

use common::db;
use mailrs_store::outbox::{self, Queued};
use mailrs_store::{accounts, open_connection};

fn scheduled(account_id: i64, draft: &str, at: i64) -> Queued {
    Queued {
        account_id,
        draft_id: Some(draft.into()),
        message_id: Some(format!("{draft}-m")),
        thread_id: Some(format!("{draft}-t")),
        subject: "Hello".into(),
        recipients: "Ann".into(),
        send_at: at,
        ..Queued::default()
    }
}

fn held(account_id: i64, subject: &str, at: i64) -> Queued {
    Queued {
        account_id,
        subject: subject.into(),
        recipients: "Ann".into(),
        send_at: at,
        raw: Some(b"From: me\r\n\r\nhello".to_vec()),
        composer: r#"{"subject":"Hello"}"#.into(),
        attempts: 1,
        problem: Some("No network".into()),
        ..Queued::default()
    }
}

#[test]
fn scheduled_sends_come_due_in_order_and_can_move() {
    let (conn, id) = db();
    outbox::put(&conn, &scheduled(id, "b", 200)).unwrap();
    outbox::put(&conn, &scheduled(id, "a", 100)).unwrap();
    let due: Vec<String> = outbox::due(&conn, 150)
        .unwrap()
        .into_iter()
        .filter_map(|m| m.draft_id)
        .collect();
    assert_eq!(due, ["a"]);

    outbox::put(&conn, &scheduled(id, "a", 300)).unwrap();
    outbox::set_message(&conn, id, "a", "a-m2", "a-t").unwrap();
    let a = outbox::find_draft(&conn, id, "a").unwrap().unwrap();
    assert_eq!((a.send_at, a.message_id.as_deref()), (300, Some("a-m2")));
    assert!(outbox::by_message(&conn, id, "a-m2").unwrap().is_some());

    outbox::remove_draft(&conn, id, "a").unwrap();
    assert_eq!(outbox::list(&conn).unwrap().len(), 1);
}

#[test]
fn a_message_with_no_gmail_draft_gets_a_row_of_its_own_each_time() {
    let (conn, id) = db();
    let first = outbox::put(&conn, &held(id, "One", 100)).unwrap();
    let second = outbox::put(&conn, &held(id, "Two", 200)).unwrap();
    assert_ne!(first, second);
    assert_eq!(outbox::list(&conn).unwrap().len(), 2);
    assert_eq!(
        outbox::find(&conn, first).unwrap().unwrap().subject,
        "One",
        "each row is found by the id it was given"
    );
    outbox::remove(&conn, first).unwrap();
    assert!(outbox::find(&conn, first).unwrap().is_none());
}

#[test]
fn a_recorded_problem_moves_a_message_from_send_later_to_the_outbox() {
    let (conn, id) = db();
    let waiting = outbox::put(&conn, &scheduled(id, "a", 100)).unwrap();
    assert_eq!(outbox::scheduled(&conn).unwrap().len(), 1);
    assert!(outbox::stuck(&conn).unwrap().is_empty());

    outbox::failed(&conn, waiting, "Gmail is busy", 500).unwrap();
    assert!(outbox::scheduled(&conn).unwrap().is_empty());
    let stuck = outbox::stuck(&conn).unwrap();
    assert_eq!(stuck.len(), 1);
    assert_eq!(stuck[0].problem.as_deref(), Some("Gmail is busy"));
    assert_eq!((stuck[0].attempts, stuck[0].send_at), (1, 500));
}

#[test]
fn the_network_coming_back_brings_stuck_messages_forward_and_leaves_the_rest() {
    let (conn, id) = db();
    let stuck = outbox::put(&conn, &held(id, "Stuck", 9_000)).unwrap();
    outbox::put(&conn, &scheduled(id, "later", 9_000)).unwrap();
    outbox::try_now(&conn, 1_000).unwrap();
    assert_eq!(outbox::find(&conn, stuck).unwrap().unwrap().send_at, 1_000);
    assert_eq!(
        outbox::find_draft(&conn, id, "later")
            .unwrap()
            .unwrap()
            .send_at,
        9_000,
        "a message waiting for its hour keeps it"
    );
}

/// The store as it stood before the outbox: enough of it for the
/// migration to have something to carry over.
///
/// It is written out by hand rather than migrated, so it holds only the
/// tables the migrations after it touch. A later migration that alters
/// another table has to be given that table here too, or this test fails
/// on a database that never existed.
fn version_fourteen(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE accounts (
            id              INTEGER PRIMARY KEY,
            email           TEXT NOT NULL UNIQUE,
            state           TEXT NOT NULL DEFAULT 'bootstrapping',
            history_id      INTEGER,
            backfill_cursor TEXT,
            backfill_done   INTEGER NOT NULL DEFAULT 0,
            sync_gen        INTEGER NOT NULL DEFAULT 1,
            added_at        INTEGER NOT NULL
        );
        CREATE TABLE scheduled (
            account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            draft_id   TEXT NOT NULL,
            message_id TEXT NOT NULL,
            thread_id  TEXT NOT NULL,
            subject    TEXT NOT NULL,
            recipients TEXT NOT NULL,
            send_at    INTEGER NOT NULL,
            PRIMARY KEY (account_id, draft_id)
        );
        CREATE TABLE messages (
            account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            id              TEXT NOT NULL,
            thread_id       TEXT NOT NULL,
            rfc822_msgid    TEXT,
            from_name       TEXT,
            from_addr       TEXT,
            to_addrs        TEXT NOT NULL,
            cc_addrs        TEXT NOT NULL,
            subject         TEXT NOT NULL,
            date            INTEGER NOT NULL,
            snippet         TEXT NOT NULL,
            size            INTEGER NOT NULL,
            has_attachments INTEGER NOT NULL,
            sync_gen        INTEGER NOT NULL,
            PRIMARY KEY (account_id, id)
        );
        CREATE TABLE bodies (
            account_id           INTEGER NOT NULL,
            message_id           TEXT NOT NULL,
            html                 TEXT,
            text                 TEXT,
            size                 INTEGER NOT NULL,
            fetched_at           INTEGER NOT NULL,
            accessed_at          INTEGER NOT NULL,
            list_unsubscribe     TEXT,
            one_click_unsubscribe INTEGER NOT NULL DEFAULT 0,
            calendar             TEXT,
            protection           TEXT,
            PRIMARY KEY (account_id, message_id)
        );
        CREATE TABLE attachments (
            account_id    INTEGER NOT NULL,
            message_id    TEXT NOT NULL,
            part_id       TEXT NOT NULL,
            filename      TEXT NOT NULL,
            mime_type     TEXT NOT NULL,
            size          INTEGER NOT NULL,
            attachment_id TEXT,
            content_id    TEXT,
            PRIMARY KEY (account_id, message_id, part_id)
        );
        CREATE TABLE threads (
            account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            id              TEXT NOT NULL,
            last_message_at INTEGER NOT NULL,
            subject         TEXT NOT NULL,
            snippet         TEXT NOT NULL,
            from_display    TEXT NOT NULL,
            message_count   INTEGER NOT NULL,
            unread          INTEGER NOT NULL,
            starred         INTEGER NOT NULL,
            has_attachments INTEGER NOT NULL,
            PRIMARY KEY (account_id, id)
        );
        INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@example.com', 0);
        INSERT INTO scheduled VALUES (1, 'r1', 'm1', 't1', 'Monday', 'Ann', 5000);
        PRAGMA user_version = 14;",
    )
    .unwrap();
}

#[test]
fn a_scheduled_send_from_the_old_store_carries_over_to_the_outbox() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    version_fourteen(&path);

    let conn = open_connection(&path).unwrap();
    let waiting = outbox::scheduled(&conn).unwrap();
    assert_eq!(waiting.len(), 1, "the message survived the migration");
    assert_eq!(waiting[0].draft_id.as_deref(), Some("r1"));
    assert_eq!(waiting[0].message_id.as_deref(), Some("m1"));
    assert_eq!(waiting[0].thread_id.as_deref(), Some("t1"));
    assert_eq!(waiting[0].subject, "Monday");
    assert_eq!(waiting[0].send_at, 5000);
    assert!(waiting[0].id > 0, "and it was given a row id of its own");
    assert!(waiting[0].raw.is_none(), "its bytes are still Gmail's");
}

#[test]
fn a_queued_message_is_still_there_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_connection(&path).unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    let row = outbox::put(&conn, &held(id, "Report", 100)).unwrap();
    drop(conn);

    let conn = open_connection(&path).unwrap();
    let found = outbox::find(&conn, row).unwrap().unwrap();
    assert_eq!(found.subject, "Report");
    assert_eq!(found.raw.as_deref(), Some(&b"From: me\r\n\r\nhello"[..]));
    assert_eq!(found.composer, r#"{"subject":"Hello"}"#);
    assert_eq!(found.problem.as_deref(), Some("No network"));
}

#[test]
fn a_message_can_be_claimed_once_until_the_claim_lapses_or_goes() {
    let (conn, id) = db();
    let row = outbox::put(&conn, &held(id, "One", 100)).unwrap();

    assert!(outbox::claim(&conn, row, 1_000, 0).unwrap().is_some());
    assert!(
        outbox::claim(&conn, row, 1_001, 0).unwrap().is_none(),
        "a second caller finds it taken"
    );
    assert!(
        outbox::claim(&conn, row, 5_000, 2_000).unwrap().is_some(),
        "a claim older than the cutoff has lapsed"
    );

    outbox::release(&conn, row).unwrap();
    let mut back = outbox::claim(&conn, row, 6_000, 0).unwrap().unwrap();
    back.attempts += 1;
    outbox::put(&conn, &back).unwrap();
    assert!(
        outbox::claim(&conn, row, 6_001, 0).unwrap().is_some(),
        "writing the row back lets go of it"
    );
    assert!(outbox::claim(&conn, 999, 7_000, 0).unwrap().is_none());
}

#[test]
fn the_sidebar_counts_waiting_and_stuck_messages_without_reading_them() {
    let (conn, id) = db();
    outbox::put(&conn, &scheduled(id, "a", 100)).unwrap();
    outbox::put(&conn, &scheduled(id, "b", 200)).unwrap();
    outbox::put(&conn, &held(id, "Stuck", 300)).unwrap();
    assert_eq!(outbox::counts(&conn).unwrap(), (2, 1));
}
