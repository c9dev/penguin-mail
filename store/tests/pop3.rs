mod common;

use std::collections::HashSet;

use common::{meta, store};
use mailrs_domain::RemoveSetting;
use mailrs_store::messages::{self, Change};
use mailrs_store::pop3::{FailReason, Failing};
use mailrs_store::{accounts, local_messages, open_in_memory, pop3};
use rusqlite::Connection;

fn pop3_account(remove: RemoveSetting) -> (Connection, i64) {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_pop3_account(&conn, "dana@example.org", "example.org", remove, 0)
        .unwrap()
        .expect("a new account");
    (conn, id)
}

fn uidls(list: &[&str]) -> Vec<String> {
    list.iter().map(|u| u.to_string()).collect()
}

#[test]
fn a_raw_copy_goes_with_its_message() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    store(&conn, &[meta(id, "pop3/u1", "t1", 1, &[])]);
    local_messages::put(&conn, id, "pop3/u1", b"Subject: hi\r\n\r\nbody").unwrap();
    assert_eq!(
        local_messages::get(&conn, id, "pop3/u1")
            .unwrap()
            .as_deref(),
        Some(&b"Subject: hi\r\n\r\nbody"[..])
    );
    messages::apply(
        &conn,
        id,
        &[Change::Delete {
            message_id: "pop3/u1".into(),
        }],
    )
    .unwrap();
    assert_eq!(
        local_messages::get(&conn, id, "pop3/u1").unwrap(),
        None,
        "Delete Forever takes the raw copy"
    );
}

#[test]
fn only_uidls_never_downloaded_count_as_new() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    pop3::mark_downloaded(&conn, id, "u1", "pop3/u1", 10).unwrap();
    assert_eq!(
        pop3::unseen(&conn, id, &uidls(&["u1", "u2", "u3"])).unwrap(),
        uidls(&["u2", "u3"])
    );
}

#[test]
fn leave_on_server_never_marks_a_downloaded_message_for_removal() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    pop3::mark_downloaded(&conn, id, "u1", "pop3/u1", 10).unwrap();
    pop3::mark_downloaded(&conn, id, "u2", "pop3/u2", 10).unwrap();
    assert!(
        pop3::pending_removal(&conn, id, None, pop3::PAGE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_wanted_removal_waits_until_a_clean_quit_confirms_it() {
    let (conn, id) = pop3_account(RemoveSetting::Downloaded);
    for uidl in ["u1", "u2", "u3"] {
        pop3::mark_downloaded(&conn, id, uidl, &format!("pop3/{uidl}"), 10).unwrap();
    }
    pop3::want_removed(&conn, id, &uidls(&["u1", "u3"])).unwrap();
    assert_eq!(
        pop3::pending_removal(&conn, id, None, pop3::PAGE).unwrap(),
        uidls(&["u1", "u3"])
    );
    assert_eq!(
        pop3::pending_removal(&conn, id, Some("u1"), pop3::PAGE).unwrap(),
        uidls(&["u3"]),
        "pages go on after the cursor"
    );
    pop3::mark_removed(&conn, id, &uidls(&["u1"])).unwrap();
    assert_eq!(
        pop3::pending_removal(&conn, id, None, pop3::PAGE).unwrap(),
        uidls(&["u3"])
    );
}

#[test]
fn removal_after_days_wants_only_the_old_ones() {
    let (conn, id) = pop3_account(RemoveSetting::Days(30));
    pop3::mark_downloaded(&conn, id, "old", "pop3/old", 1_000).unwrap();
    pop3::mark_downloaded(&conn, id, "new", "pop3/new", 9_000).unwrap();
    assert_eq!(pop3::want_removed_before(&conn, id, 5_000).unwrap(), 1);
    assert_eq!(
        pop3::pending_removal(&conn, id, None, pop3::PAGE).unwrap(),
        uidls(&["old"])
    );
}

#[test]
fn a_refused_retr_is_counted_apart_from_what_was_downloaded() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    assert_eq!(
        pop3::record_failure(&conn, id, "u9", FailReason::Refused, "-ERR no such message").unwrap(),
        1
    );
    assert_eq!(
        pop3::record_failure(&conn, id, "u9", FailReason::Refused, "-ERR still gone").unwrap(),
        2
    );
    assert!(
        pop3::failing(&conn, id).unwrap().is_empty(),
        "two failures stay off the menu"
    );
    assert_eq!(
        pop3::unseen(&conn, id, &uidls(&["u9"])).unwrap(),
        uidls(&["u9"]),
        "a failure is tried again"
    );
    assert_eq!(
        pop3::record_failure(&conn, id, "u9", FailReason::Refused, "-ERR still gone").unwrap(),
        3
    );
    assert_eq!(
        pop3::failing(&conn, id).unwrap(),
        [Failing {
            uidl: "u9".into(),
            reason: FailReason::Refused,
            words: "-ERR still gone".into(),
            sender: None,
            subject: None,
        }]
    );
    assert_eq!(pop3::accounts_failing(&conn).unwrap(), [id]);
    pop3::mark_downloaded(&conn, id, "u9", "pop3/u9", 10).unwrap();
    assert!(
        pop3::failing(&conn, id).unwrap().is_empty(),
        "a download clears the count"
    );
}

#[test]
fn a_failure_keeps_its_latest_reason_and_the_message_named_once_known() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    pop3::record_failure(&conn, id, "u9", FailReason::Refused, "-ERR busy").unwrap();
    pop3::record_failure(&conn, id, "u9", FailReason::TooLarge, "").unwrap();
    pop3::record_failure(&conn, id, "u9", FailReason::TooLarge, "").unwrap();
    pop3::name_failure(&conn, id, "u9", Some("Ana Lima"), Some("Photos")).unwrap();
    assert_eq!(
        pop3::failing(&conn, id).unwrap(),
        [Failing {
            uidl: "u9".into(),
            reason: FailReason::TooLarge,
            words: String::new(),
            sender: Some("Ana Lima".into()),
            subject: Some("Photos".into()),
        }]
    );
    assert_eq!(
        pop3::failure_reasons(&conn, id, &uidls(&["u1", "u9"])).unwrap(),
        [("u9".to_string(), FailReason::TooLarge)]
    );
}

#[test]
fn a_uidl_the_server_no_longer_lists_is_forgotten() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    for n in 0..(pop3::PAGE + 3) {
        pop3::mark_downloaded(&conn, id, &format!("u{n:04}"), &format!("pop3/u{n:04}"), 10).unwrap();
    }
    let listed: HashSet<String> = (0..pop3::PAGE).map(|n| format!("u{n:04}")).collect();
    assert_eq!(pop3::forget_gone(&conn, id, &listed).unwrap(), 3);
    assert_eq!(
        pop3::unseen(&conn, id, &uidls(&["u0000", "u0501"])).unwrap(),
        uidls(&["u0501"])
    );
}

#[test]
fn a_sent_copy_is_found_by_its_message_id() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    mailrs_store::mailboxes::upsert(
        &conn,
        id,
        &mailrs_domain::RemoteMailbox {
            id: "sent".into(),
            name: "Sent".into(),
            kind: mailrs_domain::MailboxKind::System,
            role: Some(mailrs_domain::Role::Sent),
            color: None,
            hidden: false,
        },
    )
    .unwrap();
    let mut sent = meta(id, "local/1", "t1", 1, &[]);
    sent.held.mailboxes = vec!["sent".into()];
    store(&conn, &[sent]);
    assert_eq!(
        pop3::sent_with(&conn, id, "<local/1@example.com>")
            .unwrap()
            .as_deref(),
        Some("local/1")
    );
    assert_eq!(
        pop3::sent_with(&conn, id, "<other@example.com>").unwrap(),
        None
    );
}

#[test]
fn a_pop3_account_keeps_its_removal_setting_and_its_address() {
    let (conn, id) = pop3_account(RemoveSetting::Days(14));
    assert_eq!(
        accounts::pop3_remove(&conn, id).unwrap(),
        RemoveSetting::Days(14)
    );
    accounts::set_pop3_remove(&conn, id, RemoveSetting::Never).unwrap();
    assert_eq!(
        accounts::pop3_remove(&conn, id).unwrap(),
        RemoveSetting::Never
    );
    let again = accounts::insert_pop3_account(
        &conn,
        "dana@example.org",
        "Example",
        RemoveSetting::Downloaded,
        5,
    )
    .unwrap();
    assert_eq!(again, Some(id), "signing in again finds the same account");
    assert!(
        accounts::insert_imap_account(&conn, "dana@example.org", "Example", 5)
            .unwrap()
            .is_none()
    );
    let account = accounts::account(&conn, id).unwrap().unwrap();
    assert_eq!(account.provider, mailrs_domain::Provider::Pop3);
    assert_eq!(account.provider_name(), "Example");
}

#[test]
fn a_first_check_is_unfinished_until_it_is_marked() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    assert!(!pop3::first_check_finished(&conn, id).unwrap());
    pop3::mark_downloaded(&conn, id, "u1", "pop3/u1", 10).unwrap();
    assert!(!pop3::first_check_finished(&conn, id).unwrap(), "a download alone does not finish it");
    pop3::finish_first_check(&conn, id).unwrap();
    assert!(pop3::first_check_finished(&conn, id).unwrap());
}

#[test]
fn a_uidl_whose_store_id_is_taken_gets_a_fresh_one() {
    let (conn, id) = pop3_account(RemoveSetting::Never);
    assert_eq!(pop3::download_id(&conn, id, "u1").unwrap(), "pop3/u1");
    store(&conn, &[meta(id, "pop3/u1", "t1", 1, &[])]);
    assert_eq!(
        pop3::download_id(&conn, id, "u1").unwrap(),
        "pop3/u1/2",
        "the server gave u1 to a new message; the old one keeps its id"
    );
    store(&conn, &[meta(id, "pop3/u1/2", "t2", 2, &[])]);
    assert_eq!(pop3::download_id(&conn, id, "u1").unwrap(), "pop3/u1/3");
}

#[test]
fn a_removal_asked_for_a_message_reaches_its_own_download_only() {
    let (conn, id) = pop3_account(RemoveSetting::Downloaded);
    pop3::mark_downloaded(&conn, id, "u1", "pop3/u1/2", 10).unwrap();
    pop3::want_removed_of(&conn, id, &uidls(&["pop3/u1"])).unwrap();
    assert!(
        pop3::pending_removal(&conn, id, None, pop3::PAGE)
            .unwrap()
            .is_empty(),
        "pop3/u1 is an older message the server once called u1"
    );
    pop3::want_removed_of(&conn, id, &uidls(&["pop3/u1/2"])).unwrap();
    assert_eq!(
        pop3::pending_removal(&conn, id, None, pop3::PAGE).unwrap(),
        uidls(&["u1"])
    );
}
