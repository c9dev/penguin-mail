use mailrs_domain::{AccountState, SignInClient};
use mailrs_store::accounts::{self, SyncCursor};
use mailrs_store::{open_connection, open_in_memory, schema_version};

#[test]
fn migrations_run_once_and_record_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_connection(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 32);
    drop(conn);
    let conn = open_connection(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 32);
}

#[test]
fn the_last_inbox_check_is_kept_per_account() {
    let conn = open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
    assert_eq!(accounts::checked_at(&conn, a).unwrap(), None);
    accounts::set_checked_at(&conn, a, 1_700_000_000_000).unwrap();
    assert_eq!(
        accounts::checked_at(&conn, a).unwrap(),
        Some(1_700_000_000_000)
    );
    assert_eq!(accounts::checked_at(&conn, b).unwrap(), None);
}

#[test]
fn inserting_the_same_email_twice_returns_one_account() {
    let conn = open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "me@example.com", 10).unwrap();
    let b = accounts::insert_account(&conn, "me@example.com", 20).unwrap();
    assert_eq!(a, b);
    assert_eq!(accounts::list_accounts(&conn).unwrap().len(), 1);
}

#[test]
fn accounts_list_in_insertion_order_and_start_bootstrapping() {
    let conn = open_in_memory().unwrap();
    accounts::insert_account(&conn, "b@example.com", 0).unwrap();
    accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    let all = accounts::list_accounts(&conn).unwrap();
    let emails: Vec<&str> = all.iter().map(|a| a.email.as_str()).collect();
    assert_eq!(emails, ["b@example.com", "a@example.com"]);
    assert!(all.iter().all(|a| a.state == AccountState::Bootstrapping));
}

#[test]
fn state_changes_persist() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    accounts::set_state(&conn, id, AccountState::NeedsReauth).unwrap();
    let account = accounts::account_by_email(&conn, "me@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(account.state, AccountState::NeedsReauth);
    assert!(
        accounts::account_by_email(&conn, "nobody@example.com")
            .unwrap()
            .is_none()
    );
}

#[test]
fn cursors_start_empty_and_track_progress() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    assert_eq!(
        accounts::sync_cursor(&conn, id).unwrap(),
        SyncCursor {
            state: None,
            backfill_cursor: None,
            backfill_done: false,
            sync_gen: 1
        }
    );
    accounts::set_sync_state(&conn, id, "{\"history_id\":55}").unwrap();
    accounts::set_backfill(&conn, id, Some("p2"), false).unwrap();
    let cursor = accounts::sync_cursor(&conn, id).unwrap();
    assert_eq!(cursor.state.as_deref(), Some("{\"history_id\":55}"));
    assert_eq!(cursor.backfill_cursor.as_deref(), Some("p2"));
}

#[test]
fn a_new_generation_resets_backfill() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    accounts::set_backfill(&conn, id, Some("p9"), true).unwrap();
    assert_eq!(accounts::start_generation(&conn, id, "s99").unwrap(), 2);
    assert_eq!(
        accounts::sync_cursor(&conn, id).unwrap(),
        SyncCursor {
            state: Some("s99".into()),
            backfill_cursor: None,
            backfill_done: false,
            sync_gen: 2
        }
    );
}

#[test]
fn deleting_an_account_removes_it() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    accounts::delete_account(&conn, id).unwrap();
    assert!(accounts::list_accounts(&conn).unwrap().is_empty());
}

#[test]
fn a_new_account_signs_in_with_the_built_in_client() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    assert_eq!(
        accounts::sign_in_client(&conn, id).unwrap(),
        SignInClient::BuiltIn
    );
}

#[test]
fn signing_in_again_moves_an_account_to_the_built_in_client() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    accounts::set_sign_in_client(&conn, id, SignInClient::Own).unwrap();
    assert_eq!(
        accounts::sign_in_client(&conn, id).unwrap(),
        SignInClient::Own
    );
    accounts::set_sign_in_client(&conn, id, SignInClient::BuiltIn).unwrap();
    assert_eq!(
        accounts::sign_in_client(&conn, id).unwrap(),
        SignInClient::BuiltIn
    );
}

#[test]
fn a_new_account_is_served_by_gmail() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    let listed = accounts::list_accounts(&conn).unwrap();
    assert_eq!(listed[0].provider, mailrs_domain::Provider::Gmail);
    let found = accounts::account_by_email(&conn, "me@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(
        (found.id, found.provider),
        (id, mailrs_domain::Provider::Gmail)
    );
}

#[test]
fn an_imap_account_keeps_its_provider_and_its_name() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_imap_account(&conn, "dana@fastmail.com", "Fastmail", 5)
        .unwrap()
        .expect("nobody else holds the address");
    let found = accounts::account_by_email(&conn, "dana@fastmail.com")
        .unwrap()
        .unwrap();
    assert_eq!(found.id, id);
    assert_eq!(found.provider, mailrs_domain::Provider::Imap);
    assert_eq!(found.provider_name(), "Fastmail");
    assert_eq!(found.state, AccountState::Bootstrapping);
}

#[test]
fn a_gmail_account_is_never_taken_over_by_an_imap_one() {
    let conn = open_in_memory().unwrap();
    accounts::insert_account(&conn, "me@gmail.com", 0).unwrap();
    assert_eq!(
        accounts::insert_imap_account(&conn, "me@gmail.com", "Gmail", 1).unwrap(),
        None
    );
    let found = accounts::account_by_email(&conn, "me@gmail.com")
        .unwrap()
        .unwrap();
    assert_eq!(found.provider, mailrs_domain::Provider::Gmail);
    assert_eq!(found.provider_name, None);
}

#[test]
fn adding_an_imap_address_again_keeps_one_account_under_the_new_name() {
    let conn = open_in_memory().unwrap();
    let first = accounts::insert_imap_account(&conn, "me@example.org", "example.org", 1)
        .unwrap();
    let again = accounts::insert_imap_account(&conn, "me@example.org", "Fastmail", 2)
        .unwrap();
    assert_eq!(first, again);
    let all = accounts::list_accounts(&conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].provider_name(), "Fastmail");
}
