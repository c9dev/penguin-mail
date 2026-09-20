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
