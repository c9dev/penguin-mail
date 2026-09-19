mod common;

use common::db;
use mailrs_store::reminders::{self, Reminder};

#[test]
fn reminders_come_due_and_can_move_or_go() {
    let (conn, id) = db();
    let at = |thread: &str, when: i64| Reminder {
        account_id: id,
        thread_id: thread.into(),
        subject: "Invoice".into(),
        remind_at: when,
    };
    reminders::set(&conn, &at("t2", 200)).unwrap();
    reminders::set(&conn, &at("t1", 100)).unwrap();
    let due: Vec<String> = reminders::due(&conn, 150)
        .unwrap()
        .into_iter()
        .map(|r| r.thread_id)
        .collect();
    assert_eq!(due, ["t1"]);
    reminders::set(&conn, &at("t1", 300)).unwrap();
    assert!(
        reminders::due(&conn, 250)
            .unwrap()
            .iter()
            .all(|r| r.thread_id == "t2")
    );
    reminders::remove(&conn, id, "t2").unwrap();
    assert_eq!(reminders::list(&conn).unwrap().len(), 1);
}
