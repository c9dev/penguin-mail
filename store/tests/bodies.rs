mod common;

use common::{db, meta, store};
use mailrs_domain::{Attachment, MessageBody};
use mailrs_store::{bodies, messages};

fn body(text: &str) -> MessageBody {
    MessageBody {
        html: Some(format!("<p>{text}</p>")),
        text: Some(text.into()),
        attachments: vec![Attachment {
            part_id: "2".into(),
            filename: "a.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 10,
            attachment_id: Some("att".into()),
            content_id: None,
        }],
        list_unsubscribe: Some("<https://news.example/u>".into()),
        one_click_unsubscribe: true,
        calendar: Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into()),
    }
}

#[test]
fn bodies_round_trip_with_attachments() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX"])]);
    bodies::put_body(&conn, id, "a", &body("hi"), 1).unwrap();
    assert_eq!(
        bodies::get_body(&conn, id, "a", 2).unwrap(),
        Some(body("hi"))
    );
    assert_eq!(bodies::get_body(&conn, id, "missing", 2).unwrap(), None);
}

#[test]
fn eviction_keeps_the_most_recently_read_bodies() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "a", "t1", 100, &[]),
            meta(id, "b", "t2", 100, &[]),
            meta(id, "c", "t3", 100, &[]),
        ],
    );
    // Each body is 15 bytes: "<p>xxxx</p>" plus "xxxx".
    bodies::put_body(&conn, id, "a", &body("aaaa"), 1).unwrap();
    bodies::put_body(&conn, id, "b", &body("bbbb"), 2).unwrap();
    bodies::put_body(&conn, id, "c", &body("cccc"), 3).unwrap();
    bodies::get_body(&conn, id, "a", 4).unwrap();
    assert_eq!(bodies::evict_bodies(&conn, 30).unwrap(), 1);
    assert!(bodies::get_body(&conn, id, "b", 5).unwrap().is_none());
    assert!(bodies::get_body(&conn, id, "a", 5).unwrap().is_some());
    assert!(bodies::get_body(&conn, id, "c", 5).unwrap().is_some());
}

#[test]
fn deleting_a_message_deletes_its_body() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX"])]);
    bodies::put_body(&conn, id, "a", &body("hi"), 1).unwrap();
    messages::delete_message(&conn, id, "a").unwrap();
    assert!(bodies::get_body(&conn, id, "a", 2).unwrap().is_none());
    let attachments: i64 = conn
        .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(attachments, 0);
}

#[test]
fn putting_a_body_again_replaces_it() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX"])]);
    bodies::put_body(&conn, id, "a", &body("one"), 1).unwrap();
    let plain = MessageBody {
        html: None,
        text: Some("two".into()),
        ..Default::default()
    };
    bodies::put_body(&conn, id, "a", &plain, 2).unwrap();
    assert_eq!(bodies::get_body(&conn, id, "a", 3).unwrap(), Some(plain));
}

#[test]
fn a_cache_under_the_cap_keeps_every_body() {
    let (conn, id) = db();
    store(
        &conn,
        &[meta(id, "a", "t1", 100, &[]), meta(id, "b", "t2", 100, &[])],
    );
    bodies::put_body(&conn, id, "a", &body("aaaa"), 1).unwrap();
    bodies::put_body(&conn, id, "b", &body("bbbb"), 2).unwrap();
    assert_eq!(bodies::evict_bodies(&conn, 30).unwrap(), 0);
    assert_eq!(bodies::evict_bodies(&conn, 29).unwrap(), 1);
    assert!(bodies::peek_body(&conn, id, "a").unwrap().is_none());
    let attachments: i64 = conn
        .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(attachments, 1, "the evicted body took its attachments");
}

#[test]
fn recorded_reads_only_move_access_times_forward() {
    let (conn, id) = db();
    store(
        &conn,
        &[meta(id, "a", "t1", 100, &[]), meta(id, "b", "t2", 100, &[])],
    );
    bodies::put_body(&conn, id, "a", &body("aaaa"), 1).unwrap();
    bodies::put_body(&conn, id, "b", &body("bbbb"), 2).unwrap();
    bodies::touch_bodies(
        &conn,
        id,
        &[("a".into(), 9), ("b".into(), 1), ("gone".into(), 9)],
    )
    .unwrap();
    // "a" was read last, so the older "b" goes first.
    assert_eq!(bodies::evict_bodies(&conn, 15).unwrap(), 1);
    assert!(bodies::peek_body(&conn, id, "a").unwrap().is_some());
    assert!(bodies::peek_body(&conn, id, "b").unwrap().is_none());
}
