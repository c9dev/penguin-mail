mod common;

use common::{db, meta, store};
use mailrs_domain::{FlagColor, MailboxKind, RemoteMailbox};
use mailrs_store::{accounts, drafts, flags, mailboxes, switch};

fn label(id: &str, name: &str) -> RemoteMailbox {
    RemoteMailbox {
        id: id.into(),
        name: name.into(),
        kind: MailboxKind::Label,
        role: None,
        color: None,
        hidden: false,
    }
}

#[test]
fn user_labels_list_by_id_and_name_and_system_rows_stay_out() {
    let (conn, id) = db();
    mailboxes::upsert(&conn, id, &label("Label_3", "Work/Clients")).unwrap();
    mailboxes::upsert(&conn, id, &label("Label_9", "Travel")).unwrap();
    assert_eq!(
        mailboxes::user_ids_and_names(&conn, id).unwrap(),
        [
            ("Label_3".to_string(), "Work/Clients".to_string()),
            ("Label_9".to_string(), "Travel".to_string()),
        ]
    );
}

#[test]
fn forgetting_an_accounts_drafts_leaves_other_mail_alone() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "m1", "t1", 100, &["DRAFT"]),
            meta(id, "m2", "t2", 200, &["INBOX"]),
        ],
    );
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    drafts::forget_account(&conn, id).unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m1").unwrap(), None);
    drafts::remember(&conn, id, "d2", "m2").unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m2").unwrap(), None);
}

/// `meta` of the message `id` the new route lists again under `new_id`,
/// with its Message-ID header unchanged.
fn listed_again(account: i64, id: &str, new_id: &str) -> mailrs_domain::MessageMeta {
    mailrs_domain::MessageMeta {
        rfc822_msgid: Some(format!("<{id}@example.com>")),
        ..meta(account, new_id, "t-new", 100, &["INBOX"])
    }
}

#[test]
fn a_colour_flag_waits_through_the_emptied_mail_and_returns_by_message_id() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "m1", "t1", 100, &["INBOX"])]);
    flags::set_color(&conn, id, "t1", Some("m1"), Some(FlagColor::Red)).unwrap();

    switch::empty_mail(&conn, id).unwrap();
    store(&conn, &[listed_again(id, "m1", "INBOX/1/7")]);

    assert_eq!(
        flags::colors(&conn, id, "t-new", None).unwrap(),
        [("INBOX/1/7".to_string(), Some(FlagColor::Red))]
    );
}

#[test]
fn a_kept_colour_flag_outlasts_the_first_full_sync() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "m1", "t1", 100, &["INBOX"])]);
    flags::set_color(&conn, id, "t1", Some("m1"), Some(FlagColor::Red)).unwrap();

    switch::empty_mail(&conn, id).unwrap();
    accounts::set_backfill(&conn, id, None, true).unwrap();
    store(&conn, &[listed_again(id, "m1", "INBOX/1/7")]);

    assert_eq!(
        flags::colors(&conn, id, "t-new", None).unwrap(),
        [("INBOX/1/7".to_string(), Some(FlagColor::Red))]
    );
}

#[test]
fn a_kept_colour_flag_goes_once_it_is_too_old() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "m1", "t1", 100, &["INBOX"])]);
    flags::set_color(&conn, id, "t1", Some("m1"), Some(FlagColor::Red)).unwrap();

    switch::empty_mail(&conn, id).unwrap();
    conn.execute(
        "UPDATE kept_flags SET kept_at = kept_at - ?1 - 1000",
        [accounts::KEEP_FLAGS_FOR],
    )
    .unwrap();
    accounts::set_backfill(&conn, id, None, true).unwrap();
    store(&conn, &[listed_again(id, "m1", "INBOX/1/7")]);

    assert_eq!(
        flags::colors(&conn, id, "t-new", None).unwrap(),
        [("INBOX/1/7".to_string(), None)]
    );
}
