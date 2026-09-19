mod common;

use std::collections::HashSet;

use common::{db, meta, store};
use mailrs_domain::{Label, LabelKind};
use mailrs_store::{labels, messages, threads};

#[test]
fn labels_are_replaced_wholesale_and_listed_system_first() {
    let (conn, id) = db();
    let label = |lid: &str, name: &str, kind: LabelKind| Label {
        account_id: id,
        id: lid.into(),
        name: name.into(),
        kind,
        color: None,
    };
    labels::replace_labels(
        &conn,
        id,
        &[
            label("Label_2", "Zeta", LabelKind::User),
            label("INBOX", "INBOX", LabelKind::System),
            label("Label_1", "Alpha", LabelKind::User),
        ],
    )
    .unwrap();
    let names: Vec<String> = labels::list_labels(&conn, id)
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["INBOX", "Alpha", "Zeta"]);
    labels::replace_labels(&conn, id, &[label("INBOX", "INBOX", LabelKind::System)]).unwrap();
    assert_eq!(labels::list_labels(&conn, id).unwrap().len(), 1);
}

#[test]
fn messages_round_trip_in_date_order() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "b", "t1", 200, &["INBOX"]),
            meta(id, "a", "t1", 100, &["INBOX", "UNREAD"]),
        ],
    );
    assert_eq!(
        messages::thread_messages(&conn, id, "t1").unwrap(),
        vec![
            meta(id, "a", "t1", 100, &["INBOX", "UNREAD"]),
            meta(id, "b", "t1", 200, &["INBOX"])
        ]
    );
}

#[test]
fn upsert_replaces_fields_and_labels() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX", "UNREAD"])]);
    let mut changed = meta(id, "a", "t1", 100, &["INBOX"]);
    changed.subject = "Edited".into();
    store(&conn, &[changed.clone()]);
    assert_eq!(
        messages::thread_messages(&conn, id, "t1").unwrap(),
        vec![changed]
    );
}

#[test]
fn thread_summary_aggregates_its_messages() {
    let (conn, id) = db();
    let mut first = meta(id, "a", "t1", 100, &["INBOX", "STARRED"]);
    first.subject = "Hello".into();
    let mut second = meta(id, "b", "t1", 200, &["INBOX", "UNREAD"]);
    second.subject = "Re: Hello".into();
    second.has_attachments = true;
    store(&conn, &[first, second]);
    let t = threads::get_thread(&conn, id, "t1").unwrap().unwrap();
    assert_eq!(t.subject, "Hello");
    assert_eq!(t.snippet, "snippet b");
    assert_eq!(t.from, "Sender b");
    assert_eq!(t.message_count, 2);
    assert_eq!(t.last_message_at, 200);
    assert!(t.unread);
    assert!(t.starred);
    assert!(t.has_attachments);
}

#[test]
fn label_changes_report_the_thread_and_skip_unknown_messages() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX", "UNREAD"])]);
    assert_eq!(
        messages::remove_labels(&conn, id, "a", &["UNREAD".into()])
            .unwrap()
            .as_deref(),
        Some("t1")
    );
    assert_eq!(
        messages::add_labels(&conn, id, "a", &["STARRED".into()])
            .unwrap()
            .as_deref(),
        Some("t1")
    );
    assert_eq!(
        messages::labels_of(&conn, id, "a").unwrap(),
        ["INBOX", "STARRED"]
    );
    assert_eq!(
        messages::add_labels(&conn, id, "zzz", &["INBOX".into()]).unwrap(),
        None
    );
    messages::refresh_thread(&conn, id, "t1").unwrap();
    assert!(
        !threads::get_thread(&conn, id, "t1")
            .unwrap()
            .unwrap()
            .unread
    );
}

#[test]
fn set_labels_replaces_the_whole_set() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX", "UNREAD"])]);
    messages::set_labels(&conn, id, "a", &["SENT".into()]).unwrap();
    assert_eq!(messages::labels_of(&conn, id, "a").unwrap(), ["SENT"]);
}

#[test]
fn deleting_the_last_message_removes_the_thread() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX"])]);
    assert_eq!(
        messages::delete_message(&conn, id, "a").unwrap().as_deref(),
        Some("t1")
    );
    assert_eq!(messages::delete_message(&conn, id, "a").unwrap(), None);
    messages::refresh_thread(&conn, id, "t1").unwrap();
    assert!(threads::get_thread(&conn, id, "t1").unwrap().is_none());
}

#[test]
fn delete_thread_removes_messages_and_summary() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "a", "t1", 100, &["INBOX"]),
            meta(id, "b", "t1", 200, &["INBOX"]),
        ],
    );
    messages::delete_thread(&conn, id, "t1").unwrap();
    assert!(
        messages::thread_messages(&conn, id, "t1")
            .unwrap()
            .is_empty()
    );
    assert!(threads::get_thread(&conn, id, "t1").unwrap().is_none());
}

#[test]
fn existing_ids_reports_only_stored_messages() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX"])]);
    let found = messages::existing_ids(&conn, id, &["a".into(), "b".into()]).unwrap();
    assert_eq!(found, HashSet::from(["a".to_string()]));
}
