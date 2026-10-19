//! A Gmail account signed in through the desktop's Online Accounts: how it
//! links, how it connects, and what it does when Online Accounts changes.

use std::sync::Arc;

use mailrs_domain::{Account, MailRoute, SignInClient};
use mailrs_imap::Login;
use mailrs_store::{Db, accounts};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::connector::{built_in_clients_answering, come_back_from_google, gmail_account, store};
use crate::config::Config;
use crate::connect::gmail_route::{GmailImapSignIn, goa_imap_sign_in};
use crate::goa::fake::FakeAccounts;
use crate::goa::{GoaAccount, GoaError};
use crate::passwords::Secrets;
use crate::services::AnyMail;
use crate::sign_in::calendar_consent;
use crate::sign_in::goa_link::{LinkError, Linked, link};
use crate::{BackendError, Clients, Connected, Connector, SyncError};

const NOW: i64 = 1_700_000_000_000;
const PROFILE: &str = "/gmail/v1/users/me/profile";

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

async fn linked_account(db: &Db, route: MailRoute) -> Account {
    let email = "dana@example.com";
    db.write(move |c| {
        let id = accounts::insert_account(c, email, 0)?;
        accounts::set_sign_in_client(c, id, SignInClient::Goa)?;
        accounts::set_mail_route(c, id, route)?;
        accounts::set_goa_id(c, id, Some("account_1_0"))
    })
    .await
    .unwrap();
    db.read(move |c| accounts::account_by_email(c, email))
        .await
        .unwrap()
        .unwrap()
}

/// GNOME's project has the Gmail API off: what a live account answered.
fn api_off() -> ResponseTemplate {
    ResponseTemplate::new(403).set_body_json(json!({"error": {
        "code": 403,
        "message": "Gmail API has not been used in project 44438659992 before or it is disabled.",
        "status": "PERMISSION_DENIED",
        "errors": [{"reason": "accessNotConfigured", "domain": "usageLimits"}],
        "details": [{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": "SERVICE_DISABLED",
            "domain": "googleapis.com",
            "metadata": {
                "service": "gmail.googleapis.com",
                "serviceTitle": "Gmail API",
                "activationUrl": "https://console.developers.google.com/apis/api/gmail.googleapis.com/overview?project=44438659992"
            }
        }]
    }}))
}

async fn profile_answers(server: &MockServer, answer: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(PROFILE))
        .respond_with(answer)
        .mount(server)
        .await;
}

fn base(server: &MockServer) -> String {
    format!("{}/gmail/v1/users/me", server.uri())
}

#[tokio::test]
async fn linking_lands_on_imap_when_gnomes_project_has_the_gmail_api_off() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let server = MockServer::start().await;
    profile_answers(&server, api_off()).await;

    let (account, linked) = link(
        &reach(&db, &goa, Clients::default()),
        &dana(),
        &base(&server),
        NOW,
    )
    .await
    .unwrap();

    assert_eq!(linked, Linked::Imap);
    let id = account.id;
    let (route, client, goa_id) = db
        .read(move |c| {
            Ok((
                accounts::mail_route(c, id)?,
                accounts::sign_in_client(c, id)?,
                accounts::goa_id(c, id)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(route, MailRoute::Imap);
    assert_eq!(client, SignInClient::Goa);
    assert_eq!(goa_id.as_deref(), Some("account_1_0"));
}

#[tokio::test]
async fn linking_lands_on_the_api_when_get_profile_answers() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let server = MockServer::start().await;
    profile_answers(
        &server,
        ResponseTemplate::new(200)
            .set_body_json(json!({"emailAddress": "dana@example.com", "historyId": "9"})),
    )
    .await;

    let (account, linked) = link(
        &reach(&db, &goa, Clients::default()),
        &dana(),
        &base(&server),
        NOW,
    )
    .await
    .unwrap();

    assert_eq!(linked, Linked::Api);
    let id = account.id;
    let route = db.read(move |c| accounts::mail_route(c, id)).await.unwrap();
    assert_eq!(route, MailRoute::GmailApi);
}

#[tokio::test]
async fn a_401_after_one_renewal_asks_for_a_sign_in_in_online_accounts() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let server = MockServer::start().await;
    profile_answers(&server, ResponseTemplate::new(401)).await;

    let refused = link(
        &reach(&db, &goa, Clients::default()),
        &dana(),
        &base(&server),
        NOW,
    )
    .await;

    assert!(
        matches!(refused, Err(LinkError::SignInAgain)),
        "{refused:?}"
    );
    assert_eq!(goa.ensure_calls(), 1);
    assert!(
        db.read(accounts::list_accounts)
            .await
            .unwrap()
            .is_empty(),
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

    let refused = link(
        &reach(&db, &goa, Clients::default()),
        &off,
        "http://unused",
        NOW,
    )
    .await;

    assert!(matches!(refused, Err(LinkError::MailOff)), "{refused:?}");
    assert!(
        db.read(accounts::list_accounts)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn linking_an_address_the_store_holds_changes_nothing_and_answers_it() {
    let (db, _dir) = store().await;
    let held = gmail_account(&db).await;
    let goa = FakeAccounts::new();
    let same = GoaAccount {
        identity: "Me@Gmail.com".into(),
        ..dana()
    };
    goa.add(same.clone());
    let server = MockServer::start().await;
    profile_answers(&server, api_off()).await;

    let (account, linked) = link(
        &reach(&db, &goa, Clients::default()),
        &same,
        &base(&server),
        NOW,
    )
    .await
    .unwrap();

    assert_eq!(account.id, held.id);
    assert_eq!(linked, Linked::Imap);
    let id = held.id;
    let (client, goa_id) = db
        .read(move |c| Ok((accounts::sign_in_client(c, id)?, accounts::goa_id(c, id)?)))
        .await
        .unwrap();
    assert_eq!(
        client,
        SignInClient::BuiltIn,
        "the move changes it, not the link"
    );
    assert_eq!(goa_id, None);
}

#[tokio::test]
async fn a_goa_account_writes_no_mail_token_to_the_keyring() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let server = MockServer::start().await;
    profile_answers(&server, api_off()).await;
    let reach = reach(&db, &goa, Clients::default());

    let (account, _) = link(&reach, &dana(), &base(&server), NOW).await.unwrap();
    reach.connect(&account).await.ok();

    assert_eq!(reach.secrets.google.load("dana@example.com").unwrap(), None);
}

#[tokio::test]
async fn a_goa_api_account_connects_on_gnomes_token() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db, MailRoute::GmailApi).await;

    let connected = reach(&db, &goa, Clients::default())
        .connect(&account)
        .await
        .unwrap();

    let Connected::Ready(services) = connected else {
        panic!("the account should be ready");
    };
    assert!(matches!(services.mail, AnyMail::Google(_)));
}

#[tokio::test]
async fn a_goa_imap_account_hands_the_seam_a_token_login() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db, MailRoute::Imap).await;
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
    let account = linked_account(&db, MailRoute::Imap).await;

    let Connected::Ready(services) = reach(&db, &goa, Clients::default())
        .connect(&account)
        .await
        .unwrap()
    else {
        panic!("the account should be ready");
    };

    assert!(matches!(services.mail, AnyMail::Imap(_)));
}

#[tokio::test]
async fn an_imap_login_asked_again_after_a_refusal_renews_the_token_once() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    goa.add(dana());
    let account = linked_account(&db, MailRoute::Imap).await;
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
    let account = linked_account(&db, MailRoute::Imap).await;
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
    let account = linked_account(&db, MailRoute::Imap).await;
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
    let account = linked_account(&db, MailRoute::GmailApi).await;

    let connected = reach(&db, &goa, Clients::default())
        .connect(&account)
        .await
        .unwrap();

    assert!(matches!(connected, Connected::Ready(_)));
    let id = account.id;
    let goa_id = db.read(move |c| accounts::goa_id(c, id)).await.unwrap();
    assert_eq!(goa_id.as_deref(), Some("account_9_0"));
}

#[tokio::test]
async fn a_removed_account_with_no_match_keeps_its_mail_and_calendar() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    let account = linked_account(&db, MailRoute::GmailApi).await;
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
async fn an_unreachable_online_accounts_backs_off() {
    let (db, _dir) = store().await;
    let goa = FakeAccounts::new();
    let account = linked_account(&db, MailRoute::GmailApi).await;
    goa.hang(true);

    let failed = reach(&db, &goa, Clients::default()).connect(&account).await;

    assert!(
        matches!(failed, Err(SyncError::Backend(BackendError::Offline(_)))),
        "{:?}",
        failed.map(|_| ())
    );
}

#[tokio::test]
async fn the_calendar_consent_keeps_a_goa_account_on_goa() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| {
        accounts::set_sign_in_client(c, id, SignInClient::Goa)?;
        accounts::set_mail_route(c, id, MailRoute::Imap)?;
        accounts::set_goa_id(c, id, Some("account_1_0"))
    })
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
    let (client, goa_id) = db
        .read(move |c| Ok((accounts::sign_in_client(c, id)?, accounts::goa_id(c, id)?)))
        .await
        .unwrap();
    assert_eq!(client, SignInClient::Goa);
    assert_eq!(goa_id.as_deref(), Some("account_1_0"));
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

#[tokio::test]
async fn the_calendar_consent_opens_for_a_goa_account_on_the_api() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| accounts::set_sign_in_client(c, id, SignInClient::Goa))
        .await
        .unwrap();
    let server = MockServer::start().await;
    let connector = reach(
        &db,
        &FakeAccounts::new(),
        built_in_clients_answering(&server, "me@gmail.com").await,
    );

    let signed = calendar_consent(&connector, &me, come_back_from_google).await;

    assert!(signed.is_ok(), "{:?}", signed.err());
}
