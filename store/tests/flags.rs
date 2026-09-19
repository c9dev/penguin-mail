mod common;

use common::{db, meta, mixed_mail, store};
use mailrs_domain::FlagColor;
use mailrs_store::threads::{self, ThreadFilter, list_messages, list_threads};
use mailrs_store::{flags, messages};

fn ids(rows: Vec<mailrs_domain::ThreadSummary>) -> Vec<String> {
    rows.into_iter().map(|r| r.id).collect()
}

#[test]
fn flag_colours_filter_and_default_to_red() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "a", "t1", 100, &["INBOX", "STARRED"]),
            meta(id, "b", "t2", 200, &["INBOX", "STARRED"]),
            meta(id, "c", "t3", 300, &["INBOX"]),
        ],
    );
    flags::set_color(&conn, id, "t2", None, Some(FlagColor::Blue)).unwrap();
    let blue = ThreadFilter::unified("").with_flag(FlagColor::Blue);
    assert_eq!(ids(list_threads(&conn, &blue, 0, 10).unwrap()), ["t2"]);
    let red = ThreadFilter::unified("").with_flag(FlagColor::Red);
    assert_eq!(ids(list_threads(&conn, &red, 0, 10).unwrap()), ["t1"]);
    let all = list_threads(&conn, &ThreadFilter::unified("INBOX"), 0, 10).unwrap();
    assert_eq!(all[1].flag_color, Some(FlagColor::Blue));
    assert_eq!(all[0].flag_color, None);
    let counts = flags::counts(&conn).unwrap();
    assert_eq!(
        (counts.get(&FlagColor::Red), counts.get(&FlagColor::Blue)),
        (Some(&1), Some(&1))
    );
    let rows = list_messages(&conn, &blue, 0, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message_id.as_deref(), Some("b"));
    flags::set_color(&conn, id, "t2", None, None).unwrap();
    assert!(list_threads(&conn, &blue, 0, 10).unwrap().is_empty());
}

#[test]
fn senders_and_any_label_leave_out_trash() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "a", "t1", 100, &["INBOX"]),
            meta(id, "b", "t2", 200, &[]),
            meta(id, "c", "t3", 300, &["TRASH"]),
        ],
    );
    let any = ThreadFilter::unified("");
    assert_eq!(ids(list_threads(&conn, &any, 0, 10).unwrap()), ["t2", "t1"]);
    // `meta` sends message "a" from a@example.com.
    let vip = ThreadFilter::unified("").from_senders(vec!["A@Example.com".into()]);
    assert_eq!(ids(list_threads(&conn, &vip, 0, 10).unwrap()), ["t1"]);
    messages::add_labels(&conn, id, "a", &["TRASH".to_string()]).unwrap();
    messages::refresh_thread(&conn, id, "t1").unwrap();
    assert!(list_threads(&conn, &vip, 0, 10).unwrap().is_empty());
}

#[test]
fn a_thread_row_carries_the_flag_colour_and_the_newest_sender() {
    let (conn, a, b) = mixed_mail();
    flags::set_color(&conn, b, "tb2", None, Some(FlagColor::Green)).unwrap();
    let flagged = threads::get_thread(&conn, b, "tb2").unwrap().unwrap();
    assert_eq!(flagged.flag_color, Some(FlagColor::Green));
    // `meta` sends message "a2" from a2@example.com.
    let newest = threads::get_thread(&conn, a, "ta2").unwrap().unwrap();
    assert_eq!(newest.from_email, "a2@example.com");
    assert_eq!(newest.flag_color, None);
    flags::set_color(&conn, b, "tb2", None, None).unwrap();
    assert_eq!(
        threads::get_thread(&conn, b, "tb2")
            .unwrap()
            .unwrap()
            .flag_color,
        None
    );
    // A later message takes over both columns.
    store(&conn, &[meta(b, "b9", "tb2", 900, &["INBOX"])]);
    let row = threads::get_thread(&conn, b, "tb2").unwrap().unwrap();
    assert_eq!(row.from_email, "b9@example.com");
    assert_eq!(
        threads::list_threads(&conn, &ThreadFilter::account(b, "INBOX"), 0, 10)
            .unwrap()
            .first()
            .map(|t| t.from_email.clone()),
        Some("b9@example.com".to_string())
    );
}
