//! Connecting, forgetting and signing in an account through the one
//! interface the app and the CLI share, for every provider.

use mailrs_domain::{Account, AccountState, Provider, RemoveSetting};
use mailrs_gmail::OAuthClient;
use mailrs_graph::MicrosoftClient;
use mailrs_store::servers::{self, Pop3Servers, Saved, Security, Servers};
use mailrs_store::{Db, accounts};

use crate::config::Config;
use crate::passwords::{PasswordStore, Secrets};
use crate::services::{AnyIdentities, AnyMail};
use crate::sign_in::{Browser, BrowserSignInError, Wanted, in_browser};
use crate::{Clients, Connected, Connector, Lacks};

async fn store() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    (db, dir)
}

async fn account(db: &Db, email: &'static str) -> Account {
    db.read(move |c| accounts::account_by_email(c, email))
        .await
        .unwrap()
        .expect("the account is stored")
}

fn both_clients() -> Clients {
    Clients {
        google: Some(OAuthClient::new("cid", "secret")),
        microsoft: Some(MicrosoftClient::new("00000000-0000-0000-0000-000000000000")),
    }
}

fn connector(db: &Db, clients: Clients) -> Connector {
    Connector {
        db: db.clone(),
        config: Config::default(),
        clients,
        secrets: Secrets::memory(),
    }
}

fn saved(host: &str, port: u16, user: &str) -> Saved {
    Saved {
        host: host.into(),
        port,
        security: Security::Tls,
        user_name: user.into(),
    }
}

async fn gmail_account(db: &Db) -> Account {
    db.write(|c| accounts::insert_account(c, "me@gmail.com", 0))
        .await
        .unwrap();
    account(db, "me@gmail.com").await
}

async fn microsoft_account(db: &Db) -> Account {
    db.write(|c| accounts::insert_microsoft_account(c, "dana@outlook.com", "Outlook", 0))
        .await
        .unwrap()
        .unwrap();
    account(db, "dana@outlook.com").await
}

async fn imap_account(db: &Db) -> Account {
    db.write(|c| {
        let id = accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0)?
            .expect("nobody holds the address");
        servers::save(
            c,
            id,
            &Servers {
                imap: saved("imap.fastmail.com", 993, "dana@fastmail.com"),
                smtp: saved("smtp.fastmail.com", 465, "dana@fastmail.com"),
            },
        )
    })
    .await
    .unwrap();
    account(db, "dana@fastmail.com").await
}

async fn pop3_account(db: &Db) -> Account {
    db.write(|c| {
        let id = accounts::insert_pop3_account(c, "dana@example.org", "example.org", RemoveSetting::Never, 0)?
            .expect("nobody holds the address");
        servers::save_pop3(
            c,
            id,
            &Pop3Servers {
                pop3: saved("pop.example.org", 995, "dana@example.org"),
                smtp: saved("smtp.example.org", 465, "dana@example.org"),
            },
        )
    })
    .await
    .unwrap();
    account(db, "dana@example.org").await
}

async fn state(db: &Db, account: &Account) -> AccountState {
    let id = account.id;
    db.read(move |c| accounts::account(c, id)).await.unwrap().unwrap().state
}

#[tokio::test]
async fn a_google_account_with_its_token_connects_to_google() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, both_clients());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    let connected = reach.connect(&me).await.unwrap();
    assert!(matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Google(_))));
}

#[tokio::test]
async fn a_google_account_without_a_token_needs_to_sign_in() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let connected = connector(&db, both_clients()).connect(&me).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
}

#[tokio::test]
async fn a_build_without_a_google_client_asks_its_google_accounts_to_sign_in() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, Clients::default());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    let connected = reach.connect(&me).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Client)));
    assert_eq!(state(&db, &me).await, AccountState::NeedsReauth);
}

#[tokio::test]
async fn a_microsoft_account_with_its_token_connects_to_microsoft() {
    let (db, _dir) = store().await;
    let dana = microsoft_account(&db).await;
    let reach = connector(&db, both_clients());
    reach.secrets.microsoft.save(dana.id, "refresh").unwrap();
    let connected = reach.connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::Ready(services) if matches!(services.identities, AnyIdentities::Microsoft(_))));
}

#[tokio::test]
async fn a_microsoft_account_without_a_token_needs_to_sign_in() {
    let (db, _dir) = store().await;
    let dana = microsoft_account(&db).await;
    let connected = connector(&db, both_clients()).connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
    assert_eq!(state(&db, &dana).await, AccountState::NeedsReauth);
}

#[tokio::test]
async fn a_build_without_a_microsoft_client_asks_its_microsoft_accounts_to_sign_in() {
    let (db, _dir) = store().await;
    let dana = microsoft_account(&db).await;
    let reach = connector(&db, Clients::default());
    reach.secrets.microsoft.save(dana.id, "refresh").unwrap();
    let connected = reach.connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Client)));
}

#[tokio::test]
async fn an_imap_account_with_its_password_connects_to_its_server() {
    let (db, _dir) = store().await;
    let dana = imap_account(&db).await;
    let reach = connector(&db, Clients::default());
    reach.secrets.passwords.save(dana.id, "secret").unwrap();
    let connected = reach.connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Imap(_))));
}

#[tokio::test]
async fn an_imap_account_without_its_password_needs_to_sign_in() {
    let (db, _dir) = store().await;
    let dana = imap_account(&db).await;
    let connected = connector(&db, both_clients()).connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
}

#[tokio::test]
async fn a_pop3_account_with_its_password_connects_to_its_server() {
    let (db, _dir) = store().await;
    let dana = pop3_account(&db).await;
    let reach = connector(&db, Clients::default());
    reach.secrets.passwords.save(dana.id, "secret").unwrap();
    let connected = reach.connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Pop3(_))));
}

#[tokio::test]
async fn a_pop3_account_without_its_password_needs_to_sign_in() {
    let (db, _dir) = store().await;
    let dana = pop3_account(&db).await;
    let connected = connector(&db, both_clients()).connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
}

/// Every store holds a secret for the account's id and address, so a
/// forget that reached into the wrong store would leave its own behind.
async fn forgetting(db: &Db, account: &Account) -> (Option<String>, Option<String>, Option<String>) {
    let reach = connector(db, both_clients());
    reach.secrets.google.save(&account.email, "token").unwrap();
    reach.secrets.passwords.save(account.id, "password").unwrap();
    reach.secrets.microsoft.save(account.id, "refresh").unwrap();
    reach.forget(account).await.unwrap();
    (
        reach.secrets.google.load(&account.email).unwrap(),
        reach.secrets.passwords.load(account.id).unwrap(),
        reach.secrets.microsoft.load(account.id).unwrap(),
    )
}

#[tokio::test]
async fn forgetting_a_google_account_deletes_only_its_refresh_token() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let left = forgetting(&db, &me).await;
    assert_eq!(left, (None, Some("password".into()), Some("refresh".into())));
}

#[tokio::test]
async fn forgetting_a_microsoft_account_deletes_only_its_refresh_token() {
    let (db, _dir) = store().await;
    let dana = microsoft_account(&db).await;
    let left = forgetting(&db, &dana).await;
    assert_eq!(left, (Some("token".into()), Some("password".into()), None));
}

#[tokio::test]
async fn forgetting_an_imap_account_deletes_only_its_password() {
    let (db, _dir) = store().await;
    let dana = imap_account(&db).await;
    let left = forgetting(&db, &dana).await;
    assert_eq!(left, (Some("token".into()), None, Some("refresh".into())));
}

#[tokio::test]
async fn forgetting_a_pop3_account_deletes_only_its_password() {
    let (db, _dir) = store().await;
    let dana = pop3_account(&db).await;
    let left = forgetting(&db, &dana).await;
    assert_eq!(left, (Some("token".into()), None, Some("refresh".into())));
}

#[tokio::test]
async fn forgetting_an_account_with_no_secret_kept_succeeds() {
    let (db, _dir) = store().await;
    let dana = imap_account(&db).await;
    connector(&db, both_clients()).forget(&dana).await.unwrap();
}

#[test]
fn only_google_and_microsoft_accounts_sign_in_in_the_browser() {
    assert_eq!(Browser::of(Provider::Gmail), Some(Browser::Google));
    assert_eq!(Browser::of(Provider::Microsoft), Some(Browser::Microsoft));
    assert_eq!(Browser::of(Provider::Imap), None);
    assert_eq!(Browser::of(Provider::Pop3), None);
}

/// The page a browser sign-in hands the opener, with the run dropped once
/// it has, since nobody comes back from that page in a test.
async fn page_opened(browser: Browser, wanted: Wanted) -> String {
    let (db, _dir) = store().await;
    let reach = connector(&db, both_clients());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let run = in_browser(&reach, browser, wanted, move |url: &str| {
        let _ = tx.send(url.to_string());
    });
    tokio::select! {
        _ = run => panic!("the sign-in ended before anyone came back from the page"),
        url = rx => url.unwrap(),
    }
}

#[tokio::test]
async fn signing_a_google_account_in_again_opens_googles_page() {
    let me = Account {
        id: 1,
        email: "me@gmail.com".into(),
        state: AccountState::NeedsReauth,
        provider: Provider::Gmail,
        provider_name: None,
    };
    let browser = Browser::of(me.provider).unwrap();
    let url = page_opened(browser, Wanted::Only(me.email)).await;
    assert!(url.starts_with("https://accounts.google.com/"), "{url}");
}

#[tokio::test]
async fn signing_a_microsoft_account_in_again_opens_microsofts_page_for_its_address() {
    let dana = Account {
        id: 1,
        email: "dana@outlook.com".into(),
        state: AccountState::NeedsReauth,
        provider: Provider::Microsoft,
        provider_name: Some("Outlook".into()),
    };
    let browser = Browser::of(dana.provider).unwrap();
    let url = page_opened(browser, Wanted::Only(dana.email)).await;
    assert!(url.starts_with("https://login.microsoftonline.com/"), "{url}");
    assert!(url.contains("login_hint=dana%40outlook.com"), "{url}");
}

#[tokio::test]
async fn a_build_without_the_client_opens_no_page() {
    let (db, _dir) = store().await;
    let reach = connector(&db, Clients::default());
    for browser in [Browser::Google, Browser::Microsoft] {
        let refused = in_browser(&reach, browser, Wanted::Anyone, |url: &str| {
            panic!("opened {url}")
        })
        .await;
        assert!(matches!(refused, Err(BrowserSignInError::NoClient(b)) if b == browser));
    }
}
