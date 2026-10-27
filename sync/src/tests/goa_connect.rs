//! A Gmail account signed in through the desktop's Online Accounts: how it
//! links, how it connects, and what it does when Online Accounts changes.

use std::sync::Arc;

use mailrs_domain::{Account, GmailSignIn};
use mailrs_imap::Login;
use mailrs_store::{Db, accounts};
use wiremock::MockServer;

use super::connector::{built_in_clients_answering, come_back_from_google, gmail_account, store};
use crate::config::Config;
use crate::connect::gmail_route::{GmailImapSignIn, goa_imap_sign_in};
use crate::goa::fake::FakeAccounts;
use crate::goa::{GoaAccount, GoaError};
use crate::passwords::Secrets;
use crate::services::AnyMail;
use crate::sign_in::calendar_consent;
use crate::sign_in::goa_link::{LinkError, link};
use crate::{Clients, Connected, Connector};

const NOW: i64 = 1_700_000_000_000;

fn dana() -> GoaAccount {
    GoaAccount {
        id: "account_1_0".into(),
        identity: "dana@example.com".into(),
        presentation: "dana@example.com".into(),
        mail_disabled: false,
        attention_needed: false,
    }
}

/// A build with the built-in Google client, which carries the calendar.
fn built_in() -> Clients {
    Clients {
        google: Some(mailrs_gmail::OAuthClient::new("cid", "secret")),
        ..Clients::default()
    }
}

fn reach(db: &Db, goa: &Arc<FakeAccounts>, clients: Clients) -> Connector {
    Connector {
        db: db.clone(),
        config: Config::default(),
        clients,
        secrets: Secrets::memory(),
        goa: goa.clone(),
    }
}

fn on_dana() -> GmailSignIn {
    GmailSignIn::OnlineAccounts {
        goa_id: "account_1_0".into(),
    }
}

async fn sign_in(db: &Db, id: i64) -> GmailSignIn {
    db.read(move |c| accounts::gmail_sign_in(c, id))
        .await
        .unwrap()
}

async fn linked_account(db: &Db) -> Account {
    let email = "dana@example.com";
    db.write(move |c| {
        let id = accounts::insert_account(c, email, 0)?;
        accounts::set_gmail_sign_in(c, id, &on_dana())
    })
    .await
    .unwrap();
    db.read(move |c| accounts::account_by_email(c, email))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn linking_keeps_a_new_address_on_goa_and_imap() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());

    let account = link(&reach(&db, &goa, Clients::default()), &dana(), NOW)
        .await
        .unwrap();

    assert_eq!(sign_in(&db, account.id).await, on_dana());
}

#[tokio::test]
async fn a_token_online_accounts_refuses_asks_for_a_sign_in_there() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    goa.refuse_next_token(GoaError::NotAuthorized);

    let refused = link(&reach(&db, &goa, Clients::default()), &dana(), NOW).await;

    assert!(
        matches!(refused, Err(LinkError::SignInAgain)),
        "{refused:?}"
    );
    assert!(
        db.read(accounts::list_accounts).await.unwrap().is_empty(),
        "no account row"
    );
}

#[tokio::test]
async fn linking_refuses_an_account_with_mail_turned_off() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    let off = GoaAccount {
        mail_disabled: true,
        ..dana()
    };
    goa.add(off.clone());

    let refused = link(&reach(&db, &goa, Clients::default()), &off, NOW).await;

    assert!(matches!(refused, Err(LinkError::MailOff)), "{refused:?}");
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
}

#[tokio::test]
async fn linking_an_address_the_store_holds_is_refused_and_changes_nothing() {
    let (db, _dir) = store().await;
    let held = gmail_account(&db).await;
    let goa = FakeAccounts::new();
    let same = GoaAccount {
        identity: "Me@Gmail.com".into(),
        ..dana()
    };
    goa.add(same.clone());

    let refused = link(&reach(&db, &goa, Clients::default()), &same, NOW).await;

    assert!(
        matches!(refused, Err(LinkError::Taken { .. })),
        "{refused:?}"
    );
    assert_eq!(
        sign_in(&db, held.id).await,
        GmailSignIn::BuiltInApi,
        "removing the account and adding it again changes the sign-in"
    );
}

#[tokio::test]
async fn a_goa_account_writes_no_mail_token_to_the_keyring() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let reach = reach(&db, &goa, Clients::default());

    let account = link(&reach, &dana(), NOW).await.unwrap();
    reach.connect(&account).await.ok();

    assert_eq!(reach.secrets.google.load("dana@example.com").unwrap(), None);
}

#[tokio::test]
async fn a_goa_account_left_on_the_api_still_reads_its_mail_over_imap() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db).await;

    let Connected::Ready(services) = reach(&db, &goa, Clients::default())
        .connect(&account)
        .await
        .unwrap()
    else {
        panic!("the account should be ready");
    };

    assert!(matches!(services.mail, AnyMail::GmailImap(_)));
}

#[tokio::test]
async fn a_goa_imap_account_hands_the_seam_a_token_login() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db).await;
    let connector = reach(&db, &goa, Clients::default());

    let sign_in = goa_imap_sign_in(&connector, &account, &dana());

    let GmailImapSignIn::Token(source) = sign_in else {
        panic!("a GOA account signs in with a token");
    };
    assert_eq!(
        source.login(false).await.unwrap(),
        Login::bearer("dana@example.com", "t1")
    );
}

#[tokio::test]
async fn a_goa_imap_account_connects_to_gmail_over_imap() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db).await;

    let Connected::Ready(services) = reach(&db, &goa, Clients::default())
        .connect(&account)
        .await
        .unwrap()
    else {
        panic!("the account should be ready");
    };

    assert!(matches!(services.mail, AnyMail::GmailImap(_)));
}

#[tokio::test]
async fn an_imap_login_asked_again_after_a_refusal_renews_the_token_once() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db).await;
    let connector = reach(&db, &goa, Clients::default());
    let GmailImapSignIn::Token(source) = goa_imap_sign_in(&connector, &account, &dana()) else {
        panic!("a GOA account signs in with a token");
    };

    source.login(false).await.unwrap();
    let renewed = source.login(true).await.unwrap();

    assert_eq!(renewed, Login::bearer("dana@example.com", "t2"));
    assert_eq!(goa.ensure_calls(), 1);
}

#[tokio::test]
async fn a_login_refused_by_online_accounts_is_an_auth_error() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db).await;
    let connector = reach(&db, &goa, Clients::default());
    let GmailImapSignIn::Token(source) = goa_imap_sign_in(&connector, &account, &dana()) else {
        panic!("a GOA account signs in with a token");
    };

    goa.refuse_next_token(GoaError::NotAuthorized);

    assert!(matches!(
        source.login(false).await,
        Err(mailrs_imap::ImapError::Auth { .. })
    ));
}

/// The built-in client's token, which carries the calendar.
fn keep_calendar_token(connector: &Connector) {
    connector
        .secrets
        .google
        .save("dana@example.com", "rt")
        .unwrap();
}

#[tokio::test]
async fn mail_turned_off_keeps_the_calendar_and_stops_the_mail() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db).await;
    let connector = reach(&db, &goa, built_in());
    keep_calendar_token(&connector);
    goa.set("account_1_0", |a| a.mail_disabled = true);

    let Connected::Ready(services) = connector.connect(&account).await.unwrap() else {
        panic!("the account should be ready");
    };

    assert!(matches!(services.mail, AnyMail::NoMail(_)));
    assert!(services.calendar.is_some());
}

#[tokio::test]
async fn a_removed_account_relinks_by_its_address_with_the_new_id() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(GoaAccount {
        id: "account_9_0".into(),
        ..dana()
    });
    let account = linked_account(&db).await;

    let connected = reach(&db, &goa, Clients::default())
        .connect(&account)
        .await
        .unwrap();

    assert!(matches!(connected, Connected::Ready(_)));
    let now = sign_in(&db, account.id).await;
    assert_eq!(now.goa_id(), Some("account_9_0"));
}

#[tokio::test]
async fn a_removed_account_with_no_match_keeps_its_mail_and_calendar() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    let account = linked_account(&db).await;
    let connector = reach(&db, &goa, built_in());
    keep_calendar_token(&connector);

    let Connected::Ready(services) = connector.connect(&account).await.unwrap() else {
        panic!("the account should be ready");
    };

    assert!(matches!(services.mail, AnyMail::NoMail(_)));
    assert!(services.calendar.is_some());
    let still = db.read(accounts::list_accounts).await.unwrap();
    assert_eq!(still.len(), 1, "nothing is deleted");
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_online_accounts_starts_the_account_without_mail() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    let account = linked_account(&db).await;
    goa.hang(true);
    let connector = reach(&db, &goa, built_in());
    keep_calendar_token(&connector);

    let Connected::Ready(services) = connector.connect(&account).await.unwrap() else {
        panic!("the account should be ready");
    };

    assert!(matches!(services.mail, AnyMail::NoMail(_)));
    assert!(services.calendar.is_some(), "the calendar keeps running");
}

#[tokio::test]
async fn the_calendar_consent_keeps_a_goa_account_on_goa() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| accounts::set_gmail_sign_in(c, id, &on_dana()))
        .await
        .unwrap();
    let server = MockServer::start().await;
    let goa = FakeAccounts::new();
    let connector = reach(
        &db,
        &goa,
        built_in_clients_answering(&server, "me@gmail.com").await,
    );
    let mut hinted = None;

    let signed = calendar_consent(&connector, &me, |url: &str| {
        hinted = Some(url.to_string());
        come_back_from_google(url)
    })
    .await
    .unwrap();

    assert_eq!(signed.id, me.id);
    assert_eq!(sign_in(&db, id).await, on_dana());
    assert_eq!(
        connector
            .secrets
            .google
            .load("me@gmail.com")
            .unwrap()
            .as_deref(),
        Some("rt-new")
    );
    assert!(
        hinted.unwrap().contains("login_hint=me%40gmail.com"),
        "the consent names the account"
    );
}
