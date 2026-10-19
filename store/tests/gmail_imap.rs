mod common;

use common::{db, meta, store};
use mailrs_domain::{MailboxKind, RemoteMailbox};
use mailrs_store::{drafts, mailboxes};

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
