use mailrs_domain::{AccountState, GmailSignIn, MailRoute, Provider, SignInClient};
use mailrs_store::accounts::{self, SyncCursor};
use mailrs_store::{open_connection, open_in_memory, schema_version};

#[test]
fn migrations_run_once_and_record_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_connection(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 66);
    drop(conn);
    let conn = open_connection(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 66);
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
        accounts::gmail_sign_in(&conn, id).unwrap(),
        GmailSignIn::BuiltInApi
    );
}

#[test]
fn every_gmail_sign_in_round_trips_through_the_store() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    for sign_in in [
        GmailSignIn::OwnProject,
        GmailSignIn::AppPassword {
            google: SignInClient::Own,
        },
        GmailSignIn::AppPassword {
            google: SignInClient::BuiltIn,
        },
        GmailSignIn::OnlineAccounts {
            goa_id: "account_1_0".into(),
        },
        GmailSignIn::BuiltInApi,
    ] {
        accounts::set_gmail_sign_in(&conn, id, &sign_in).unwrap();
        assert_eq!(accounts::gmail_sign_in(&conn, id).unwrap(), sign_in);
    }
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
fn a_bad_stored_route_reads_as_corrupt() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    conn.execute("UPDATE accounts SET mail_route = 'pigeon'", [])
        .unwrap();
    assert!(matches!(
        accounts::gmail_sign_in(&conn, id),
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
    assert_eq!(all[&gmail], GmailSignIn::BuiltInApi);
}

#[test]
fn a_sign_in_this_build_cannot_read_leaves_the_others_listed() {
    let conn = open_in_memory().unwrap();
    let good = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    let odd = accounts::insert_account(&conn, "lee@gmail.com", 1).unwrap();
    conn.execute(
        "UPDATE accounts SET oauth_client = 'from-a-newer-build' WHERE id = ?1",
        [odd],
    )
    .unwrap();
    let all = accounts::google_sign_ins(&conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[&good], GmailSignIn::BuiltInApi);
}

#[test]
fn a_new_online_accounts_id_lands_only_on_the_link_it_replaces() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    let linked = GmailSignIn::OnlineAccounts {
        goa_id: "account_1".into(),
    };
    accounts::set_gmail_sign_in(&conn, id, &linked).unwrap();
    accounts::relink_goa(&conn, id, "account_1", "account_2").unwrap();
    assert_eq!(
        accounts::gmail_sign_in(&conn, id).unwrap(),
        GmailSignIn::OnlineAccounts {
            goa_id: "account_2".into()
        }
    );
    // An account that left Online Accounts in between keeps its sign-in.
    accounts::set_gmail_sign_in(&conn, id, &GmailSignIn::OwnProject).unwrap();
    accounts::relink_goa(&conn, id, "account_2", "account_3").unwrap();
    assert_eq!(
        accounts::gmail_sign_in(&conn, id).unwrap(),
        GmailSignIn::OwnProject
    );
}

#[test]
fn recording_a_sign_in_keeps_the_sync_cursor() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@gmail.com", 1).unwrap();
    accounts::set_sync_state(&conn, id, "12345").unwrap();
    accounts::set_gmail_sign_in(&conn, id, &GmailSignIn::OwnProject).unwrap();
    assert_eq!(
        accounts::sync_cursor(&conn, id).unwrap().state.as_deref(),
        Some("12345")
    );
    assert_eq!(
        accounts::gmail_sign_in(&conn, id).unwrap(),
        GmailSignIn::OwnProject
    );
}

#[test]
fn leaving_online_accounts_forgets_its_id() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@example.com", 1).unwrap();
    let goa = GmailSignIn::OnlineAccounts {
        goa_id: "account_1_0".into(),
    };
    accounts::set_gmail_sign_in(&conn, id, &goa).unwrap();
    accounts::set_gmail_sign_in(&conn, id, &GmailSignIn::BuiltInApi).unwrap();
    let kept: Option<String> = conn
        .query_row("SELECT goa_id FROM accounts WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kept, None);
}

#[test]
fn an_online_accounts_row_reads_its_mail_over_imap_whatever_its_route_column_says() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "dana@example.com", 1).unwrap();
    conn.execute(
        "UPDATE accounts SET oauth_client = 'goa', goa_id = 'account_1_0', mail_route = 'gmail_api'",
        [],
    )
    .unwrap();
    let sign_in = accounts::gmail_sign_in(&conn, id).unwrap();
    assert_eq!(sign_in.route(), MailRoute::Imap);
}
