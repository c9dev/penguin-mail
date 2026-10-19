use mailrs_domain::{AccountState, MailRoute, Provider, SignInClient};
use mailrs_store::accounts::{self, GoogleSignIn, RouteCommit, SyncCursor};
use mailrs_store::{open_connection, open_in_memory, schema_version};

#[test]
fn migrations_run_once_and_record_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_connection(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 67);
    drop(conn);
    let conn = open_connection(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 67);
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
            backfill_extends: false,
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
            backfill_extends: false,
            sync_gen: 2
        }
    );
}

#[test]
fn an_account_records_the_window_its_mail_was_loaded_for() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    assert_eq!(accounts::window_days(&conn, id).unwrap(), None);
    accounts::set_window_days(&conn, id, 365).unwrap();
    assert_eq!(accounts::window_days(&conn, id).unwrap(), Some(365));
}

#[test]
fn extending_the_window_starts_the_backfill_again_in_the_same_generation() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    accounts::set_backfill(&conn, id, None, true).unwrap();
    accounts::extend_backfill(&conn, id).unwrap();
    let cursor = accounts::sync_cursor(&conn, id).unwrap();
    assert!(cursor.backfill_extends && !cursor.backfill_done);
    assert_eq!(cursor.sync_gen, 1);
    // A page token on the way keeps the extension.
    accounts::set_backfill(&conn, id, Some("p2"), false).unwrap();
    assert!(accounts::sync_cursor(&conn, id).unwrap().backfill_extends);
    accounts::set_backfill(&conn, id, None, true).unwrap();
    assert!(
        !accounts::sync_cursor(&conn, id).unwrap().backfill_extends,
        "the last page ends it"
    );
    accounts::extend_backfill(&conn, id).unwrap();
    accounts::start_generation(&conn, id, "s2").unwrap();
    assert!(
        !accounts::sync_cursor(&conn, id).unwrap().backfill_extends,
        "a new generation fetches everything"
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
fn a_calendar_account_is_added_and_refused_a_taken_name() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_caldav_account(&conn, "robin@cloud.example.org", 5)
        .unwrap()
        .expect("nobody else holds the name");
    let listed = accounts::list_accounts(&conn).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider, Provider::CalDav);
    assert_eq!(listed[0].provider_name(), "CalDAV");
    assert_eq!(
        accounts::insert_caldav_account(&conn, "robin@cloud.example.org", 6).unwrap(),
        None,
        "adding it again adds nothing, and the sign-in decides what it finds"
    );
    assert_eq!(accounts::list_accounts(&conn).unwrap()[0].id, id);
    accounts::insert_imap_account(&conn, "me@example.org", "example.org", 1)
        .unwrap()
        .expect("a mail account");
    assert_eq!(
        accounts::insert_caldav_account(&conn, "me@example.org", 7).unwrap(),
        None,
        "a mail account holds the name"
    );
    assert_eq!(
        accounts::insert_imap_account(&conn, "robin@cloud.example.org", "example.org", 8).unwrap(),
        None,
        "and a calendar account's name is not a mail account's to take"
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
    let first = accounts::insert_imap_account(&conn, "me@example.org", "example.org", 1).unwrap();
    let again = accounts::insert_imap_account(&conn, "me@example.org", "Fastmail", 2).unwrap();
    assert_eq!(first, again);
    let all = accounts::list_accounts(&conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].provider_name(), "Fastmail");
}

#[test]
fn a_microsoft_account_keeps_its_provider_and_name() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_microsoft_account(&conn, "dana@outlook.com", "Outlook", 0)
        .unwrap()
        .unwrap();
    let account = accounts::account(&conn, id).unwrap().unwrap();
    assert_eq!(account.provider, Provider::Microsoft);
    assert_eq!(account.provider_name(), "Outlook");
    // Signing in again finds the same row and takes the new name.
    assert_eq!(
        accounts::insert_microsoft_account(&conn, "dana@outlook.com", "Microsoft 365", 5).unwrap(),
        Some(id)
    );
    assert_eq!(
        accounts::account(&conn, id)
            .unwrap()
            .unwrap()
            .provider_name(),
        "Microsoft 365"
    );
    // Another provider's address is left alone.
    accounts::insert_account(&conn, "dana@gmail.com", 0).unwrap();
    assert_eq!(
        accounts::insert_microsoft_account(&conn, "dana@gmail.com", "Outlook", 0).unwrap(),
        None
    );
}

#[test]
fn a_mail_route_round_trips() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    assert_eq!(
        accounts::mail_route(&conn, id).unwrap(),
        MailRoute::GmailApi
    );
    accounts::set_mail_route(&conn, id, MailRoute::Imap).unwrap();
    assert_eq!(accounts::mail_route(&conn, id).unwrap(), MailRoute::Imap);
}

#[test]
fn a_bad_stored_route_reads_as_corrupt() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    conn.execute("UPDATE accounts SET mail_route = 'pigeon'", [])
        .unwrap();
    assert!(matches!(
        accounts::mail_route(&conn, id),
        Err(mailrs_store::StoreError::Corrupt {
            column: "accounts.mail_route",
            ..
        })
    ));
}

#[test]
fn google_sign_ins_list_only_gmail_rows() {
    let conn = open_in_memory().unwrap();
    let gmail = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    accounts::insert_imap_account(&conn, "lee@fastmail.com", "Fastmail", 1).unwrap();
    let all = accounts::google_sign_ins(&conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[&gmail],
        GoogleSignIn {
            client: SignInClient::BuiltIn,
            route: MailRoute::GmailApi
        }
    );
}

#[test]
fn only_a_built_in_gmail_account_on_the_api_needs_a_move() {
    let sign_in = |client, route| GoogleSignIn { client, route };
    let api = sign_in(SignInClient::BuiltIn, MailRoute::GmailApi);
    assert!(api.needs_move(Provider::Gmail));
    assert!(!api.needs_move(Provider::Imap));
    assert!(!sign_in(SignInClient::Own, MailRoute::GmailApi).needs_move(Provider::Gmail));
    assert!(!sign_in(SignInClient::BuiltIn, MailRoute::Imap).needs_move(Provider::Gmail));
}

#[test]
fn committing_a_route_restarts_the_sync_and_keeps_the_generation() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    accounts::set_sync_state(&conn, id, "12345").unwrap();
    let before = accounts::sync_cursor(&conn, id).unwrap().sync_gen;
    accounts::commit_route(
        &conn,
        id,
        &RouteCommit {
            route: MailRoute::Imap,
            client: SignInClient::BuiltIn,
            restart_sync: true,
        },
    )
    .unwrap();
    let cursor = accounts::sync_cursor(&conn, id).unwrap();
    assert_eq!(cursor.state, None);
    assert_eq!(cursor.sync_gen, before);
    assert_eq!(accounts::mail_route(&conn, id).unwrap(), MailRoute::Imap);
}

#[test]
fn committing_a_route_without_a_restart_keeps_the_cursor() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    accounts::set_sync_state(&conn, id, "12345").unwrap();
    accounts::commit_route(
        &conn,
        id,
        &RouteCommit {
            route: MailRoute::GmailApi,
            client: SignInClient::Own,
            restart_sync: false,
        },
    )
    .unwrap();
    assert_eq!(
        accounts::sync_cursor(&conn, id).unwrap().state.as_deref(),
        Some("12345")
    );
    assert_eq!(
        accounts::sign_in_client(&conn, id).unwrap(),
        SignInClient::Own
    );
}

#[test]
fn a_goa_account_keeps_its_online_accounts_id() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@example.com", 1).unwrap();
    assert_eq!(accounts::goa_id(&conn, id).unwrap(), None);
    accounts::set_sign_in_client(&conn, id, SignInClient::Goa).unwrap();
    accounts::set_goa_id(&conn, id, Some("account_1_0")).unwrap();
    assert_eq!(
        accounts::goa_id(&conn, id).unwrap().as_deref(),
        Some("account_1_0")
    );
    assert_eq!(
        accounts::sign_in_client(&conn, id).unwrap(),
        SignInClient::Goa
    );
    accounts::set_goa_id(&conn, id, None).unwrap();
    assert_eq!(accounts::goa_id(&conn, id).unwrap(), None);
}

#[test]
fn a_goa_account_never_needs_a_move() {
    for route in [MailRoute::GmailApi, MailRoute::Imap] {
        let sign_in = GoogleSignIn {
            client: SignInClient::Goa,
            route,
        };
        assert!(!sign_in.needs_move(Provider::Gmail), "{route:?}");
    }
}
