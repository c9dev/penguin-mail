mod common;

use std::collections::HashSet;

use common::{LabelChange, db, labels_of, meta, store};
use mailrs_domain::{Applied, LabelKind, MailSet, MailboxKind, Membership, RemoteMailbox, Role};
use mailrs_store::messages::Change;
use mailrs_store::threads::ThreadFilter;
use mailrs_store::{labels, mailboxes, messages, threads};

fn listed(id: &str, name: &str, kind: MailboxKind, color: Option<&str>) -> RemoteMailbox {
    RemoteMailbox {
        id: id.into(),
        name: name.into(),
        kind,
        role: mailrs_gmail::labels::role_of(id),
        color: color.map(str::to_string),
        hidden: false,
    }
}

#[test]
fn labels_are_replaced_wholesale_and_listed_system_first() {
    let (conn, id) = db();
    mailboxes::replace_listed(
        &conn,
        id,
        &[
            listed("Label_2", "Zeta", MailboxKind::Label, None),
            listed("INBOX", "INBOX", MailboxKind::System, None),
            listed("Label_1", "Alpha", MailboxKind::Label, None),
        ],
    )
    .unwrap();
    let names: Vec<String> = labels::list_labels(&conn, id)
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["INBOX", "Alpha", "Zeta"]);
    let inbox = listed("INBOX", "INBOX", MailboxKind::System, None);
    mailboxes::replace_listed(&conn, id, &[inbox]).unwrap();
    assert_eq!(labels::list_labels(&conn, id).unwrap().len(), 1);
}

#[test]
fn a_mailbox_that_holds_only_mailboxes_lists_as_a_group() {
    let (conn, id) = db();
    mailboxes::replace_listed(
        &conn,
        id,
        &[
            listed("Work", "Work", MailboxKind::Group, None),
            listed("Work/Clients", "Work/Clients", MailboxKind::Folder, None),
        ],
    )
    .unwrap();
    let kinds: Vec<(String, LabelKind)> = labels::list_labels(&conn, id)
        .unwrap()
        .into_iter()
        .map(|l| (l.name, l.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("Work".to_string(), LabelKind::Group),
            ("Work/Clients".to_string(), LabelKind::User),
        ]
    );
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
    let touched = messages::apply(
        &conn,
        id,
        &[
            Change::label("a", "UNREAD", false),
            Change::label("a", "STARRED", true),
        ],
    )
    .unwrap();
    assert_eq!(touched.threads.into_iter().collect::<Vec<_>>(), ["t1"]);
    assert_eq!(
        labels_of(&conn, id, "a"),
        ["INBOX", "STARRED"]
    );
    let unknown = messages::apply(&conn, id, &[Change::label("zzz", "INBOX", true)]).unwrap();
    assert!(unknown.threads.is_empty());
    assert!(
        !threads::get_thread(&conn, id, "t1")
            .unwrap()
            .unwrap()
            .unread
    );
}

#[test]
fn an_upsert_replaces_the_whole_label_set() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX", "UNREAD"])]);
    store(&conn, &[meta(id, "a", "t1", 100, &["SENT"])]);
    assert_eq!(labels_of(&conn, id, "a"), ["SENT"]);
}

#[test]
fn deleting_the_last_message_removes_the_thread() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "a", "t1", 100, &["INBOX"])]);
    let delete = [Change::Delete {
        message_id: "a".into(),
    }];
    let touched = messages::apply(&conn, id, &delete).unwrap();
    assert_eq!(touched.threads.into_iter().collect::<Vec<_>>(), ["t1"]);
    assert!(
        messages::apply(&conn, id, &delete)
            .unwrap()
            .threads
            .is_empty()
    );
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
    let delete = Change::DeleteThread {
        thread_id: "t1".into(),
    };
    messages::apply(&conn, id, &[delete]).unwrap();
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

#[test]
fn existing_ids_answers_for_more_ids_than_one_statement_takes() {
    let (conn, id) = db();
    let other = mailrs_store::accounts::insert_account(&conn, "other@example.com", 0).unwrap();
    store(
        &conn,
        &[
            meta(id, "m0", "t1", 100, &["INBOX"]),
            meta(id, "m999", "t2", 200, &["INBOX"]),
            meta(id, "m1499", "t3", 300, &["INBOX"]),
            meta(other, "m5", "t4", 400, &["INBOX"]),
        ],
    );
    let asked: Vec<String> = (0..1500).map(|n| format!("m{n}")).collect();
    let found = messages::existing_ids(&conn, id, &asked).unwrap();
    assert_eq!(
        found,
        HashSet::from(["m0".to_string(), "m999".to_string(), "m1499".to_string()])
    );
    assert!(messages::existing_ids(&conn, id, &[]).unwrap().is_empty());
}

#[test]
fn each_message_of_a_thread_carries_its_own_labels() {
    let (conn, id) = db();
    let other = mailrs_store::accounts::insert_account(&conn, "other@example.com", 0).unwrap();
    store(
        &conn,
        &[
            meta(id, "a", "t1", 100, &["UNREAD", "INBOX"]),
            meta(id, "b", "t1", 200, &["SENT"]),
            meta(id, "c", "t1", 300, &[]),
            meta(other, "a", "t1", 100, &["TRASH"]),
        ],
    );
    let labels = |metas: Vec<mailrs_domain::MessageMeta>| -> Vec<(String, Vec<String>)> {
        metas
            .into_iter()
            .map(|m| (m.id.clone(), mailrs_gmail::labels::label_ids(&m)))
            .collect()
    };
    let expected = vec![
        (
            "a".to_string(),
            vec!["INBOX".to_string(), "UNREAD".to_string()],
        ),
        ("b".to_string(), vec!["SENT".to_string()]),
        ("c".to_string(), vec![]),
    ];
    assert_eq!(
        labels(messages::thread_messages(&conn, id, "t1").unwrap()),
        expected
    );
    let ids: Vec<String> = ["c", "a", "b", "z"].iter().map(|s| s.to_string()).collect();
    assert_eq!(labels(messages::by_ids(&conn, id, &ids).unwrap()), expected);
}

/// One call writes a batch of changes, refreshes every thread they
/// touched, and says what each membership change did to each message,
/// leaving out what a message already had.
#[test]
fn a_change_set_refreshes_what_it_touched_and_reports_what_it_did() {
    let (conn, id) = db();
    let touched = messages::apply(
        &conn,
        id,
        &[
            Change::Upsert {
                meta: Box::new(meta(id, "a", "t1", 100, &["INBOX", "UNREAD"])),
                generation: 1,
            },
            Change::Upsert {
                meta: Box::new(meta(id, "b", "t2", 200, &["INBOX"])),
                generation: 1,
            },
        ],
    )
    .unwrap();
    assert_eq!(
        touched.threads.into_iter().collect::<Vec<_>>(),
        ["t1", "t2"]
    );
    assert!(
        touched.applied.is_empty(),
        "an upsert is not a membership change"
    );

    let touched = messages::apply(
        &conn,
        id,
        &[
            Change::label("a", "UNREAD", false),
            Change::label("a", "STARRED", true),
            Change::label("b", "INBOX", true),
            Change::label("zzz", "INBOX", true),
        ],
    )
    .unwrap();
    assert_eq!(
        touched.threads.into_iter().collect::<Vec<_>>(),
        ["t1", "t2"]
    );
    assert_eq!(
        touched.applied,
        [Applied {
            thread_id: "t1".into(),
            message_id: "a".into(),
            gained: vec![
                Membership::Keyword("$seen".into()),
                Membership::Keyword("$flagged".into()),
            ],
            lost: vec![],
        }],
        "b was in the inbox already, and zzz is not stored"
    );
    assert_eq!(
        labels_of(&conn, id, "a"),
        ["INBOX", "STARRED"]
    );
    let t1 = threads::get_thread(&conn, id, "t1").unwrap().unwrap();
    assert!(!t1.unread && t1.starred);

    messages::apply(
        &conn,
        id,
        &[
            Change::Delete {
                message_id: "a".into(),
            },
            Change::MarkWhole {
                thread_id: "t2".into(),
            },
        ],
    )
    .unwrap();
    assert!(threads::get_thread(&conn, id, "t1").unwrap().is_none());
    assert!(messages::is_whole(&conn, id, "t2").unwrap());
}

/// A mailbox met on a message before any listing named it has no role
/// yet; the listing gives it one, and the message then lists in it.
#[test]
fn a_mailbox_met_before_the_listing_takes_its_role_from_the_listing() {
    let (conn, account) = common::bare_db();
    let mut m = meta(account, "a", "t1", 100, &[]);
    m.held.mailboxes = vec!["INBOX".into()];
    store(&conn, &[m]);
    let inbox = ThreadFilter::account(account, MailSet::Role(Role::Inbox));
    assert_eq!(threads::count_threads(&conn, &inbox).unwrap(), 0);
    mailboxes::upsert(
        &conn,
        account,
        &RemoteMailbox {
            id: "INBOX".into(),
            name: "INBOX".into(),
            kind: MailboxKind::System,
            role: Some(Role::Inbox),
            color: None,
            hidden: false,
        },
    )
    .unwrap();
    assert_eq!(threads::count_threads(&conn, &inbox).unwrap(), 1);
}

#[test]
fn a_stored_message_reads_back_with_its_roles() {
    let (conn, account) = db();
    store(&conn, &[meta(account, "a", "t1", 100, &["INBOX", "STARRED", "Label_1"])]);
    let read = messages::by_ids(&conn, account, &["a".into()]).unwrap();
    assert_eq!(read[0].roles, [Role::Inbox]);
    assert!(read[0].is_flagged());
    assert!(read[0].in_mailbox("Label_1"));
}
