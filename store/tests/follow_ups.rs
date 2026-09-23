mod common;

use common::{db, meta, store};
use mailrs_store::follow_ups;

const DAY: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 100 * DAY;

fn ago(days: i64) -> i64 {
    NOW - days * DAY
}

fn threads(conn: &rusqlite::Connection, now: i64) -> Vec<String> {
    follow_ups::waiting(conn, now)
        .unwrap()
        .into_iter()
        .map(|f| f.thread_id)
        .collect()
}

#[test]
fn only_unanswered_mail_from_three_to_thirty_days_ago_waits() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "a1", "asked", ago(10), &["INBOX"]),
            meta(id, "a2", "asked", ago(5), &["SENT"]),
            meta(id, "b1", "answered", ago(6), &["SENT"]),
            meta(id, "b2", "answered", ago(4), &["INBOX"]),
            meta(id, "c1", "too-soon", ago(1), &["SENT"]),
            meta(id, "d1", "too-old", ago(40), &["SENT"]),
            meta(id, "e1", "binned", ago(5), &["SENT", "TRASH"]),
            meta(id, "f1", "drafting", ago(8), &["SENT"]),
            meta(id, "f2", "drafting", ago(1), &["DRAFT"]),
        ],
    );
    assert_eq!(threads(&conn, NOW), ["asked", "drafting"]);
    assert_eq!(follow_ups::waiting_count(&conn, NOW).unwrap(), 2);
    let first = &follow_ups::waiting(&conn, NOW).unwrap()[0];
    assert_eq!((first.message_id.as_str(), first.sent_at), ("a2", ago(5)));
    assert_eq!(first.to[0].email, "me@example.com");
}

#[test]
fn dismissing_hides_a_thread_until_the_user_writes_again() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a1", "asked", ago(5), &["SENT"])]);
    follow_ups::dismiss(&conn, id, "asked", NOW).unwrap();
    assert!(threads(&conn, NOW).is_empty());
    follow_ups::restore(&conn, id, "asked").unwrap();
    assert_eq!(threads(&conn, NOW), ["asked"]);

    follow_ups::dismiss(&conn, id, "asked", NOW).unwrap();
    store(&conn, &[meta(id, "a2", "asked", NOW + DAY, &["SENT"])]);
    assert_eq!(threads(&conn, NOW + 5 * DAY), ["asked"]);
}
