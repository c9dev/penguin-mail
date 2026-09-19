mod common;

use common::db;
use mailrs_store::scheduled::{self, Scheduled};

fn item(account_id: i64, draft: &str, at: i64) -> Scheduled {
    Scheduled {
        account_id,
        draft_id: draft.into(),
        message_id: format!("{draft}-m"),
        thread_id: format!("{draft}-t"),
        subject: "Hello".into(),
        recipients: "Ann".into(),
        send_at: at,
    }
}

#[test]
fn scheduled_sends_come_due_in_order_and_can_move() {
    let (conn, id) = db();
    scheduled::schedule(&conn, &item(id, "b", 200)).unwrap();
    scheduled::schedule(&conn, &item(id, "a", 100)).unwrap();
    let due: Vec<String> = scheduled::due(&conn, 150)
        .unwrap()
        .into_iter()
        .map(|s| s.draft_id)
        .collect();
    assert_eq!(due, ["a"]);

    scheduled::schedule(&conn, &item(id, "a", 300)).unwrap();
    scheduled::set_message(&conn, id, "a", "a-m2", "a-t").unwrap();
    let a = scheduled::find(&conn, id, "a").unwrap().unwrap();
    assert_eq!((a.send_at, a.message_id.as_str()), (300, "a-m2"));
    assert!(scheduled::by_message(&conn, id, "a-m2").unwrap().is_some());

    scheduled::remove(&conn, id, "a").unwrap();
    assert_eq!(scheduled::list(&conn).unwrap().len(), 1);
}
