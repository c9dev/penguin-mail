//! Query trees run over the store's copy of the mail: folders and smart
//! mailboxes list what Gmail's search would list for the same tree.

mod common;

use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};
use common::{db, meta, store};
use mailrs_domain::query::{MEGABYTE, Query, Term};
use mailrs_domain::smart::{Condition, Field};
use mailrs_domain::{
    Address, Folder, MailSet, MailboxKind, MessageBody, RemoteMailbox, Role, SmartMailbox,
};
use mailrs_store::query::matching;
use mailrs_store::{accounts, bodies, mailboxes};
use rusqlite::Connection;

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;
/// 1 September 2026, 00:00 UTC.
const NOW: i64 = 1_788_220_800_000;

fn now() -> DateTime<Utc> {
    Utc.timestamp_millis_opt(NOW).unwrap()
}

/// The ids `query` matches in account `account_id`, newest first.
fn found(conn: &Connection, account_id: i64, query: &Query) -> Vec<String> {
    matching(conn, account_id, query, &now(), 100)
        .unwrap()
        .into_iter()
        .map(|m| m.message_id)
        .collect()
}

fn smart(match_all: bool, conditions: &[(Field, &str)]) -> Query {
    SmartMailbox {
        id: "s1".into(),
        name: "Test".into(),
        account: None,
        match_all,
        conditions: conditions
            .iter()
            .map(|(field, value)| Condition {
                field: *field,
                value: value.to_string(),
            })
            .collect(),
    }
    .query()
    .expect("a usable condition")
}

#[test]
fn each_folder_lists_what_its_gmail_search_would() {
    let (conn, a) = db();
    store(
        &conn,
        &[
            meta(a, "inbox", "t1", NOW - HOUR, &["INBOX"]),
            meta(a, "archived", "t2", NOW - 2 * HOUR, &["Label_1"]),
            meta(a, "sent", "t3", NOW - 3 * HOUR, &["SENT"]),
            meta(a, "draft", "t4", NOW - 4 * HOUR, &["DRAFT"]),
            meta(a, "junk", "t5", NOW - 5 * HOUR, &["SPAM"]),
            meta(a, "binned", "t6", NOW - 6 * HOUR, &["TRASH"]),
        ],
    );
    assert_eq!(found(&conn, a, &Folder::Archive.query()), ["archived"]);
    assert_eq!(found(&conn, a, &Folder::Junk.query()), ["junk"]);
    assert_eq!(found(&conn, a, &Folder::Trash.query()), ["binned"]);
    assert_eq!(
        found(&conn, a, &Folder::AllMail.query()),
        ["inbox", "archived", "sent", "draft"]
    );
}

#[test]
fn a_search_leaves_out_junk_and_trash_unless_it_names_them() {
    let (conn, a) = db();
    store(
        &conn,
        &[
            meta(a, "kept", "t1", NOW - HOUR, &["INBOX", "UNREAD"]),
            meta(a, "junk", "t2", NOW - 2 * HOUR, &["SPAM", "UNREAD"]),
        ],
    );
    let unread = Query::term(Term::Unread);
    assert_eq!(found(&conn, a, &unread), ["kept"]);
    let unread_junk = Query::And(vec![unread, Query::is_in(MailSet::Role(Role::Junk))]);
    assert_eq!(found(&conn, a, &unread_junk), ["junk"]);
}

#[test]
fn every_smart_mailbox_condition_runs_over_the_store() {
    let (conn, a) = db();
    mailboxes::upsert(
        &conn,
        a,
        &RemoteMailbox {
            id: "Label_7".into(),
            name: "Work/Clients".into(),
            kind: MailboxKind::Label,
            role: None,
            color: None,
            hidden: false,
        },
    )
    .unwrap();
    let mut ann = meta(a, "ann", "t1", NOW - HOUR, &["INBOX", "UNREAD"]);
    ann.from = Some(Address {
        name: Some("Ann Smith".into()),
        email: "ann@example.com".into(),
    });
    ann.subject = "Quarterly report".into();
    let mut big = meta(a, "big", "t2", NOW - 2 * DAY, &["Label_7", "STARRED"]);
    big.size = 6 * MEGABYTE;
    big.has_attachments = true;
    big.to = vec![Address {
        name: None,
        email: "bo@example.org".into(),
    }];
    let old = meta(a, "old", "t3", NOW - 30 * DAY, &["INBOX"]);
    store(&conn, &[ann, big, old]);

    let one = |field: Field, value: &str| found(&conn, a, &smart(true, &[(field, value)]));
    assert_eq!(one(Field::From, "ann smith"), ["ann"]);
    assert_eq!(one(Field::To, "bo@example.org"), ["big"]);
    assert_eq!(one(Field::Subject, "QUARTERLY"), ["ann"]);
    assert_eq!(one(Field::Words, "report"), ["ann"]);
    assert_eq!(one(Field::Label, "work/clients"), ["big"]);
    assert_eq!(one(Field::NewerThanDays, "7"), ["ann", "big"]);
    assert_eq!(one(Field::LargerThanMb, "5"), ["big"]);
    assert_eq!(one(Field::HasAttachment, ""), ["big"]);
    assert_eq!(one(Field::Unread, ""), ["ann"]);
    assert_eq!(one(Field::Flagged, ""), ["big"]);
    let either = smart(false, &[(Field::Unread, ""), (Field::Flagged, "")]);
    assert_eq!(found(&conn, a, &either), ["ann", "big"]);
    let both = smart(true, &[(Field::Unread, ""), (Field::Flagged, "")]);
    assert!(found(&conn, a, &both).is_empty());
}

#[test]
fn text_matches_ignore_case_in_every_script() {
    let (conn, a) = db();
    let mut elia = meta(a, "elia", "t1", NOW - HOUR, &["INBOX"]);
    elia.from = Some(Address {
        name: Some("Élia Sousa".into()),
        email: "elia@example.pt".into(),
    });
    elia.subject = "RELATÓRIO DO MÊS".into();
    store(&conn, &[elia]);
    let from = Query::term(Term::From("élia".into()));
    assert_eq!(found(&conn, a, &from), ["elia"]);
    let subject = Query::term(Term::Subject("relatório".into()));
    assert_eq!(found(&conn, a, &subject), ["elia"]);
}

#[test]
fn not_from_keeps_mail_whose_sender_has_no_name() {
    let (conn, a) = db();
    let mut nameless = meta(a, "nameless", "t1", NOW - HOUR, &["INBOX"]);
    nameless.from = Some(Address {
        name: None,
        email: "bo@example.org".into(),
    });
    let mut none = meta(a, "no-sender", "t2", NOW - 2 * HOUR, &["INBOX"]);
    none.from = None;
    store(&conn, &[nameless, none]);
    let not_ann = Query::Not(Box::new(Query::term(Term::From("ann".into()))));
    assert_eq!(found(&conn, a, &not_ann), ["nameless", "no-sender"]);
}

#[test]
fn words_reach_the_snippet_and_a_stored_body() {
    let (conn, a) = db();
    store(
        &conn,
        &[
            meta(a, "m1", "t1", NOW - HOUR, &["INBOX"]),
            meta(a, "m2", "t2", NOW - 2 * HOUR, &["INBOX"]),
        ],
    );
    let body = MessageBody {
        text: Some("The boat leaves at nine.".into()),
        ..MessageBody::default()
    };
    bodies::put_body(&conn, a, "m2", &body, NOW).unwrap();
    assert_eq!(
        found(&conn, a, &Query::term(Term::Words("snippet m1".into()))),
        ["m1"]
    );
    assert_eq!(
        found(&conn, a, &Query::term(Term::Words("BOAT".into()))),
        ["m2"]
    );
}

#[test]
fn text_with_nothing_to_search_for_drops_out_as_the_printer_drops_it() {
    let (conn, a) = db();
    store(
        &conn,
        &[
            meta(a, "read", "t1", NOW - HOUR, &["INBOX"]),
            meta(a, "unread", "t2", NOW - 2 * HOUR, &["INBOX", "UNREAD"]),
        ],
    );
    let empty = || Query::term(Term::Words("\"()".into()));
    let and = Query::And(vec![empty(), Query::term(Term::Unread)]);
    assert_eq!(found(&conn, a, &and), ["unread"]);
    let not = Query::And(vec![
        Query::Not(Box::new(empty())),
        Query::term(Term::Unread),
    ]);
    assert_eq!(found(&conn, a, &not), ["unread"]);
    assert_eq!(found(&conn, a, &empty()), ["read", "unread"]);
}

#[test]
fn a_day_starts_at_midnight_in_the_zone_of_now() {
    let (conn, a) = db();
    // 00:30 on 2 September in Lisbon's summer time, 23:30 UTC on 1 September.
    let late = NOW + 23 * HOUR + 30 * 60 * 1000;
    store(&conn, &[meta(a, "late", "t1", late, &["INBOX"])]);
    let lisbon = FixedOffset::east_opt(3600).unwrap();
    let now = lisbon.timestamp_millis_opt(NOW + 2 * DAY).unwrap();
    let second = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let since = Query::term(Term::Since(second));
    let before = Query::term(Term::Before(second));
    let ids = |query: &Query| -> Vec<String> {
        matching(&conn, a, query, &now, 10)
            .unwrap()
            .into_iter()
            .map(|m| m.message_id)
            .collect()
    };
    assert_eq!(ids(&since), ["late"]);
    assert!(ids(&before).is_empty());
}

#[test]
fn another_accounts_mail_stays_out_and_the_limit_keeps_the_newest() {
    let (conn, a) = db();
    let b = accounts::insert_account(&conn, "other@example.com", 0).unwrap();
    common::list_gmail_roles(&conn, b);
    store(
        &conn,
        &[
            meta(a, "a1", "t1", NOW - 3 * HOUR, &["INBOX"]),
            meta(a, "a2", "t2", NOW - 2 * HOUR, &["INBOX"]),
            meta(a, "a3", "t3", NOW - HOUR, &["INBOX"]),
            meta(b, "b1", "u1", NOW, &["INBOX"]),
        ],
    );
    let inbox = Query::is_in(MailSet::Role(Role::Inbox));
    let two: Vec<String> = matching(&conn, a, &inbox, &now(), 2)
        .unwrap()
        .into_iter()
        .map(|m| m.message_id)
        .collect();
    assert_eq!(two, ["a3", "a2"]);
}
