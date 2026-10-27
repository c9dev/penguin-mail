//! Connecting, forgetting and signing in an account through the one
//! interface the app and the CLI share, for every provider.

use std::sync::Arc;

use mailrs_domain::{
    Account, AccountState, GmailSignIn, MailRoute, Provider, RemoveSetting, SignInClient,
};
use mailrs_gmail::OAuthClient;
use mailrs_graph::MicrosoftClient;
use mailrs_store::servers::{self, Pop3Servers, Saved, Security, Servers};
use mailrs_store::services::{self, FoundService, ServiceKind};
use mailrs_store::{Db, accounts};

use crate::config::Config;
use crate::connect::gmail_route::google_side;
use crate::passwords::{PasswordStore, Secrets};
use crate::services::{AnyIdentities, AnyMail};
use crate::sign_in::own_client::{ID_VAR, OwnClient, OwnClientError, SECRET_VAR};
use crate::sign_in::{
    Browser, BrowserSignInError, ClientSource, Wanted, calendar_consent, google_sign_in,
    google_signed_in, in_browser,
};
use crate::{Clients, Connected, Connector, Lacks};

pub(super) async fn store() -> (Db, tempfile::TempDir) {
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
        own_google: None,
        chosen_google: None,
    }
}

fn connector(db: &Db, clients: Clients) -> Connector {
    Connector {
        db: db.clone(),
        config: Config::default(),
        clients,
        secrets: Secrets::memory(),
        goa: crate::goa::session(),
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

pub(super) async fn gmail_account(db: &Db) -> Account {
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
        let id = accounts::insert_pop3_account(
            c,
            "dana@example.org",
            "example.org",
            RemoveSetting::Never,
            0,
        )?
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

async fn calendar_account(db: &Db) -> Account {
    db.write(|c| {
        let id = accounts::insert_caldav_account(c, "robin@cloud.example.org", 0)?
            .expect("nobody holds the name");
        let server = FoundService {
            kind: ServiceKind::CalDav,
            url: "https://cloud.example.org/remote.php/dav/".into(),
            user_name: "robin".into(),
            confirmed: true,
            source: "typed".into(),
        };
        services::save(c, id, &server)
    })
    .await
    .unwrap();
    account(db, "robin@cloud.example.org").await
}

async fn state(db: &Db, account: &Account) -> AccountState {
    let id = account.id;
    db.read(move |c| accounts::account(c, id))
        .await
        .unwrap()
        .unwrap()
        .state
}

#[tokio::test]
async fn a_google_account_with_its_token_connects_to_google() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, both_clients());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    let connected = reach.connect(&me).await.unwrap();
    assert!(
        matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Google(_)))
    );
}

#[tokio::test]
async fn a_gmail_account_on_the_imap_route_connects_to_imap_mail() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| {
        accounts::set_gmail_sign_in(
            c,
            id,
            &GmailSignIn::AppPassword {
                google: SignInClient::BuiltIn,
            },
        )?;
        servers::save(
            c,
            id,
            &Servers {
                imap: saved("imap.gmail.com", 993, "me@gmail.com"),
                smtp: saved("smtp.gmail.com", 465, "me@gmail.com"),
            },
        )
    })
    .await
    .unwrap();
    let reach = connector(&db, both_clients());
    reach
        .secrets
        .passwords
        .save(id, "abcdefghijklmnop")
        .unwrap();

    let connected = reach.connect(&me).await.unwrap();

    let Connected::Ready(services) = connected else {
        panic!("the account connects");
    };
    assert!(matches!(services.mail, AnyMail::GmailImap(_)));
    assert!(services.capabilities().labels);
    assert!(!services.offers().label_colors);
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
    assert!(
        matches!(connected, Connected::Ready(services) if matches!(services.identities, AnyIdentities::Microsoft(_)))
    );
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
    assert!(
        matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Imap(_)))
    );
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
    assert!(
        matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Pop3(_)))
    );
}

#[tokio::test]
async fn a_pop3_account_without_its_password_needs_to_sign_in() {
    let (db, _dir) = store().await;
    let dana = pop3_account(&db).await;
    let connected = connector(&db, both_clients()).connect(&dana).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
}

#[tokio::test]
async fn a_calendar_account_with_its_password_connects_to_a_calendar_and_no_mail() {
    let (db, _dir) = store().await;
    let robin = calendar_account(&db).await;
    let reach = connector(&db, Clients::default());
    reach.secrets.passwords.save(robin.id, "secret").unwrap();
    let connected = reach.connect(&robin).await.unwrap();
    assert!(matches!(
        connected,
        Connected::Ready(services) if matches!(services.mail, AnyMail::NoMail(_)) && services.calendar.is_some()
    ));
}

#[tokio::test]
async fn a_calendar_account_without_its_password_needs_to_sign_in_and_the_store_says_so() {
    let (db, _dir) = store().await;
    let robin = calendar_account(&db).await;
    let connected = connector(&db, Clients::default())
        .connect(&robin)
        .await
        .unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
    assert_eq!(
        state(&db, &robin).await,
        AccountState::NeedsReauth,
        "no loop will run to say so"
    );
}

/// Every store holds a secret for the account's id and address, so a
/// forget that reached into the wrong store would leave its own behind.
async fn forgetting(
    db: &Db,
    account: &Account,
) -> (Option<String>, Option<String>, Option<String>) {
    let reach = connector(db, both_clients());
    reach.secrets.google.save(&account.email, "token").unwrap();
    reach
        .secrets
        .passwords
        .save(account.id, "password")
        .unwrap();
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
    assert_eq!(
        left,
        (None, Some("password".into()), Some("refresh".into()))
    );
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
async fn forgetting_a_calendar_account_deletes_only_its_password() {
    let (db, _dir) = store().await;
    let robin = calendar_account(&db).await;
    let left = forgetting(&db, &robin).await;
    assert_eq!(left, (Some("token".into()), None, Some("refresh".into())));
}

#[tokio::test]
async fn forgetting_a_google_account_deletes_its_own_client_too() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, both_clients());
    reach
        .secrets
        .google_clients
        .save(me.id, &OwnClient::new("own", "secret").encode())
        .unwrap();
    reach.forget(&me).await.unwrap();
    assert_eq!(reach.secrets.google_clients.load(me.id).unwrap(), None);
}

#[tokio::test]
async fn forgetting_an_imap_account_leaves_the_own_client_kept_under_its_id() {
    let (db, _dir) = store().await;
    let dana = imap_account(&db).await;
    let reach = connector(&db, both_clients());
    reach.secrets.google_clients.save(dana.id, "kept").unwrap();
    reach.forget(&dana).await.unwrap();
    assert_eq!(
        reach
            .secrets
            .google_clients
            .load(dana.id)
            .unwrap()
            .as_deref(),
        Some("kept")
    );
}

#[tokio::test]
async fn a_google_account_with_its_own_client_connects_in_a_build_without_one() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| accounts::set_gmail_sign_in(c, id, &GmailSignIn::OwnProject))
        .await
        .unwrap();
    let reach = connector(&db, Clients::default());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    reach
        .secrets
        .google_clients
        .save(me.id, &OwnClient::new("own", "secret").encode())
        .unwrap();
    let connected = reach.connect(&me).await.unwrap();
    assert!(
        matches!(connected, Connected::Ready(services) if matches!(services.mail, AnyMail::Google(_)))
    );
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
    assert_eq!(Browser::of(Provider::Jmap), None);
}

fn new_jmap(address: &str, token: &str) -> crate::sign_in::NewJmap {
    use mailrs_store::jmap::{JmapAuth, JmapSaved};

    crate::sign_in::NewJmap {
        address: address.into(),
        provider_name: "Fastmail".into(),
        saved: JmapSaved {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            auth: JmapAuth::Bearer,
            user_name: None,
            labels: true,
            capabilities: vec!["urn:ietf:params:jmap:mail".into()],
            oauth: None,
            owner: None,
        },
        secret: token.into(),
        dav_password: None,
        signed_as: crate::sign_in::SignedAs::address(address),
    }
}

#[tokio::test]
async fn a_jmap_account_connects_from_its_session_row_and_its_secret() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let new = new_jmap("me@fastmail.com", "fmu1-token");
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new, 0)
        .await
        .unwrap();
    assert_eq!(account.provider, Provider::Jmap);
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(services.mail, AnyMail::Jmap(_)));
    assert!(
        matches!(services.rules, Some(crate::AnyRules::Local(_))),
        "a session without Sieve keeps its rules here"
    );
    secrets.forget(&account).await.unwrap();
    let missing = crate::connect_jmap(&db, &secrets, &account, 30).await;
    assert!(matches!(
        missing,
        Err(crate::SyncError::Backend(crate::BackendError::NeedsReauth))
    ));
    assert_eq!(state(&db, &account).await, AccountState::NeedsReauth);
}

#[tokio::test]
async fn signing_a_jmap_account_in_again_keeps_the_new_token_and_ends_needs_sign_in() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let first =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "old"), 0)
            .await
            .unwrap();
    let id = first.id;
    db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
        .await
        .unwrap();
    let again =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "new"), 1)
            .await
            .unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.state, AccountState::Ok);
    assert_eq!(secrets.jmap.load(id).unwrap().as_deref(), Some("new"));
}

/// A session that belongs to `username`, with mail in `account`, sending
/// only as `username`.
fn reached(username: &str, account: &str) -> crate::sign_in::SignedAs {
    crate::sign_in::SignedAs {
        owner: mailrs_store::jmap::SessionOwner {
            username: username.into(),
            mail_account: account.into(),
        },
        sends_as: vec![username.into()],
    }
}

#[tokio::test]
async fn a_token_sign_in_again_that_reaches_another_account_keeps_the_old_one() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let first =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "old"), 0)
            .await
            .unwrap();
    let mut other = new_jmap("me@fastmail.com", "theirs");
    other.signed_as = reached("other@fastmail.com", "B2");
    let refused = crate::sign_in::jmap_signed_in(&db, &secrets, other, 1).await;
    let Err(err) = refused else {
        panic!("another account's token was kept")
    };
    assert!(matches!(
        err,
        crate::sign_in::JmapSignInError::DifferentAccount { .. }
    ));
    assert_eq!(
        err.to_string(),
        "You signed in to a different account than me@fastmail.com, so nothing changed."
    );
    assert_eq!(secrets.jmap.load(first.id).unwrap().as_deref(), Some("old"));
}

#[tokio::test]
async fn a_sign_in_again_that_reaches_the_same_mail_account_is_kept() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let first =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "old"), 0)
            .await
            .unwrap();
    // The server renamed the login; the mail account is the same.
    let mut renamed = new_jmap("me@fastmail.com", "new");
    renamed.signed_as = reached("me@newname.example", "A1");
    crate::sign_in::jmap_signed_in(&db, &secrets, renamed, 1)
        .await
        .unwrap();
    assert_eq!(secrets.jmap.load(first.id).unwrap().as_deref(), Some("new"));
    let id = first.id;
    let owner = db
        .read(move |c| mailrs_store::jmap::load(c, id))
        .await
        .unwrap()
        .unwrap()
        .owner
        .unwrap();
    assert_eq!(owner.username, "me@newname.example");
}

#[tokio::test]
async fn a_session_url_for_another_server_cannot_take_an_account_already_here() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let first =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "old"), 0)
            .await
            .unwrap();
    // A session URL typed by hand for another server, whose session even
    // names the same user.
    let mut elsewhere = new_jmap("me@fastmail.com", "theirs");
    elsewhere.saved.session_url = "https://jmap.example.net/jmap/session".into();
    let refused = crate::sign_in::jmap_signed_in(&db, &secrets, elsewhere, 1).await;
    assert!(matches!(
        refused,
        Err(crate::sign_in::JmapSignInError::DifferentAccount { .. })
    ));
    let id = first.id;
    let saved = db
        .read(move |c| mailrs_store::jmap::load(c, id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.session_url, "https://api.fastmail.com/jmap/session");
    assert_eq!(secrets.jmap.load(id).unwrap().as_deref(), Some("old"));
}

#[tokio::test]
async fn an_account_kept_before_its_owner_was_recorded_signs_in_again_by_its_address() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let first =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "old"), 0)
            .await
            .unwrap();
    let id = first.id;
    db.write(move |c| {
        let mut saved = mailrs_store::jmap::load(c, id)?.unwrap();
        saved.owner = None;
        mailrs_store::jmap::save(c, id, &saved)
    })
    .await
    .unwrap();
    let mut other = new_jmap("me@fastmail.com", "theirs");
    other.signed_as = reached("other@fastmail.com", "A1");
    let refused = crate::sign_in::jmap_signed_in(&db, &secrets, other, 1).await;
    assert!(
        matches!(
            refused,
            Err(crate::sign_in::JmapSignInError::DifferentAccount { .. })
        ),
        "nothing to compare, so the address decides"
    );
    crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "new"), 2)
        .await
        .unwrap();
    let owner = db
        .read(move |c| mailrs_store::jmap::load(c, id))
        .await
        .unwrap()
        .unwrap()
        .owner;
    assert_eq!(
        owner.map(|o| o.mail_account).as_deref(),
        Some("A1"),
        "the owner is recorded from now on"
    );
}

#[tokio::test]
async fn a_login_name_that_is_the_address_s_local_part_may_add_the_account() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    // Cyrus logs user1 in and names no address in its identity.
    let mut cyrus = new_jmap("user1@example.com", "pw");
    cyrus.signed_as = crate::sign_in::SignedAs {
        sends_as: Vec::new(),
        ..reached("user1", "user1")
    };
    assert!(
        crate::sign_in::jmap_signed_in(&db, &secrets, cyrus, 0)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_new_jmap_account_never_inherits_a_password_left_under_its_id() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    // A removed account whose forget failed left its passwords behind, and
    // ids are reused once the highest row goes.
    secrets.passwords.save(1, "stale").unwrap();
    secrets.passwords.save_outgoing(1, "stale smtp").unwrap();
    let account =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "token"), 0)
            .await
            .unwrap();
    assert_eq!(account.id, 1);
    assert_eq!(secrets.passwords.load(1).unwrap(), None);
    assert_eq!(secrets.passwords.load_outgoing(1).unwrap(), None);
    assert_eq!(secrets.jmap.load(1).unwrap().as_deref(), Some("token"));
}

#[tokio::test]
async fn a_jmap_sign_in_for_an_address_an_imap_account_holds_is_refused() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let dana = imap_account(&db).await;
    let refused =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap(&dana.email, "token"), 0).await;
    assert!(matches!(
        refused,
        Err(crate::sign_in::JmapSignInError::Taken { .. })
    ));
    assert_eq!(
        secrets.jmap.load(dana.id).unwrap(),
        None,
        "nothing reaches the keyring"
    );
}

fn session_row(
    capabilities: &[&str],
    auth: mailrs_store::jmap::JmapAuth,
) -> mailrs_store::jmap::JmapSaved {
    mailrs_store::jmap::JmapSaved {
        session_url: "https://jmap.example.org/jmap/session".into(),
        auth,
        user_name: None,
        labels: true,
        capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
        oauth: None,
        owner: None,
    }
}

async fn jmap_signed_in_with(
    db: &Db,
    secrets: &Secrets,
    saved: mailrs_store::jmap::JmapSaved,
    dav: Option<&str>,
) -> Account {
    let new = crate::sign_in::NewJmap {
        address: "me@example.org".into(),
        provider_name: "example.org".into(),
        saved,
        secret: "secret".into(),
        dav_password: dav.map(str::to_string),
        signed_as: crate::sign_in::SignedAs::address("me@example.org"),
    };
    crate::sign_in::jmap_signed_in(db, secrets, new, 0)
        .await
        .unwrap()
}

#[tokio::test]
async fn jmap_contacts_win_over_a_found_carddav_server() {
    use mailrs_jmap::{CONTACTS, MAIL, SIEVE, VACATION};
    use mailrs_store::jmap::JmapAuth;
    use mailrs_store::services::{FoundService, ServiceKind};

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let account = jmap_signed_in_with(
        &db,
        &secrets,
        session_row(&[MAIL, CONTACTS, SIEVE, VACATION], JmapAuth::Basic),
        None,
    )
    .await;
    let id = account.id;
    db.write(move |c| {
        for (kind, url) in [
            (ServiceKind::CardDav, "https://dav.example.org/card/"),
            (ServiceKind::CalDav, "https://dav.example.org/cal/"),
        ] {
            mailrs_store::services::save(
                c,
                id,
                &FoundService {
                    kind,
                    url: url.into(),
                    user_name: "me@example.org".into(),
                    confirmed: true,
                    source: "table".into(),
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(
        services.contacts,
        Some(crate::AnyContacts::Jmap(_))
    ));
    assert!(
        matches!(services.calendar, Some(crate::AnyCalendar::Dav(_))),
        "the calendar comes from CalDAV, with the account password"
    );
    assert!(matches!(
        services.rules,
        Some(crate::AnyRules::JmapSieve(_))
    ));
    assert!(
        matches!(services.auto_reply, Some(crate::AnyAutoReply::JmapSieve(_))),
        "the reply lives in the rules script"
    );
}

#[tokio::test]
async fn jmaps_own_reply_serves_a_session_without_sieve() {
    use mailrs_jmap::{MAIL, VACATION};
    use mailrs_store::jmap::JmapAuth;

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let account = jmap_signed_in_with(
        &db,
        &secrets,
        session_row(&[MAIL, VACATION], JmapAuth::Basic),
        None,
    )
    .await;
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(
        services.auto_reply,
        Some(crate::AnyAutoReply::Jmap(_))
    ));
}

#[tokio::test]
async fn a_sieve_session_without_jmaps_reply_still_offers_one() {
    use mailrs_jmap::{MAIL, SIEVE};
    use mailrs_store::jmap::JmapAuth;

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let account = jmap_signed_in_with(
        &db,
        &secrets,
        session_row(&[MAIL, SIEVE], JmapAuth::Basic),
        None,
    )
    .await;
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(
        services.auto_reply,
        Some(crate::AnyAutoReply::JmapSieve(_))
    ));
}

#[tokio::test]
async fn jmap_calendars_win_over_a_found_caldav_server() {
    use mailrs_jmap::{CALENDARS, MAIL};
    use mailrs_store::jmap::JmapAuth;
    use mailrs_store::services::{FoundService, ServiceKind};

    for (capabilities, jmap) in [(&[MAIL, CALENDARS][..], true), (&[MAIL][..], false)] {
        let (db, _dir) = store().await;
        let secrets = Secrets::memory();
        let account = jmap_signed_in_with(
            &db,
            &secrets,
            session_row(capabilities, JmapAuth::Basic),
            None,
        )
        .await;
        let id = account.id;
        db.write(move |c| {
            mailrs_store::services::save(
                c,
                id,
                &FoundService {
                    kind: ServiceKind::CalDav,
                    url: "https://dav.example.org/cal/".into(),
                    user_name: "me@example.org".into(),
                    confirmed: true,
                    source: "table".into(),
                },
            )
        })
        .await
        .unwrap();
        let services = crate::connect_jmap(&db, &secrets, &account, 30)
            .await
            .unwrap();
        if jmap {
            assert!(
                matches!(services.calendar, Some(crate::AnyCalendar::Jmap(_))),
                "JMAP Calendars, and CalDAV left alone"
            );
        } else {
            assert!(
                matches!(services.calendar, Some(crate::AnyCalendar::Dav(_))),
                "no JMAP Calendars, so CalDAV"
            );
        }
    }
}

#[tokio::test]
async fn a_token_account_reaches_dav_only_with_its_app_password() {
    use mailrs_jmap::MAIL;
    use mailrs_store::jmap::JmapAuth;
    use mailrs_store::services::{FoundService, ServiceKind};

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let account =
        jmap_signed_in_with(&db, &secrets, session_row(&[MAIL], JmapAuth::Bearer), None).await;
    let id = account.id;
    db.write(move |c| {
        mailrs_store::services::save(
            c,
            id,
            &FoundService {
                kind: ServiceKind::CalDav,
                url: "https://caldav.example.org/".into(),
                user_name: "me@example.org".into(),
                confirmed: true,
                source: "table".into(),
            },
        )
    })
    .await
    .unwrap();
    let without = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(without.calendar.is_none(), "a token goes nowhere but JMAP");
    assert!(matches!(without.rules, Some(crate::AnyRules::Local(_))));
    let with = jmap_signed_in_with(
        &db,
        &secrets,
        session_row(&[MAIL], JmapAuth::Bearer),
        Some("app-password"),
    )
    .await;
    let services = crate::connect_jmap(&db, &secrets, &with, 30).await.unwrap();
    assert!(services.calendar.is_some());
}

#[tokio::test]
async fn a_new_jmap_account_keeps_its_calendar_password_beside_its_token() {
    use mailrs_store::jmap::JmapAuth;

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let account = jmap_signed_in_with(
        &db,
        &secrets,
        session_row(&[mailrs_jmap::MAIL], JmapAuth::Bearer),
        Some("app-password"),
    )
    .await;
    assert_eq!(
        secrets.passwords.load(account.id).unwrap().as_deref(),
        Some("app-password")
    );
    assert_eq!(
        secrets.jmap.load(account.id).unwrap().as_deref(),
        Some("secret")
    );
}

/// The page a browser sign-in hands the opener, with the run dropped once
/// it has, since nobody comes back from that page in a test.
async fn page_opened(browser: Browser, wanted: Wanted) -> String {
    page_opened_with(both_clients(), browser, wanted).await
}

async fn page_opened_with(clients: Clients, browser: Browser, wanted: Wanted) -> String {
    let (db, _dir) = store().await;
    page_opened_by(&connector(&db, clients), browser, wanted).await
}

async fn page_opened_by(reach: &Connector, browser: Browser, wanted: Wanted) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let run = in_browser(reach, browser, wanted, move |url: &str| {
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
    assert!(
        url.starts_with("https://login.microsoftonline.com/"),
        "{url}"
    );
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

#[tokio::test]
async fn an_own_client_in_the_environment_signs_in_over_the_built_in_one() {
    let clients = Clients {
        own_google: Some(Ok(OwnClient::new("own-cid", "own-secret"))),
        ..both_clients()
    };
    let url = page_opened_with(clients, Browser::Google, Wanted::Anyone).await;
    assert!(url.contains("client_id=own-cid"), "{url}");
}

#[tokio::test]
async fn an_own_client_signs_in_from_a_build_without_the_built_in_one() {
    let clients = Clients {
        own_google: Some(Ok(OwnClient::new("own-cid", "own-secret"))),
        ..Clients::default()
    };
    let url = page_opened_with(clients, Browser::Google, Wanted::Anyone).await;
    assert!(url.contains("client_id=own-cid"), "{url}");
}

#[tokio::test]
async fn half_an_own_client_opens_no_page_and_says_which_value_is_missing() {
    let (db, _dir) = store().await;
    let half = OwnClientError::Half {
        set: ID_VAR,
        missing: SECRET_VAR,
    };
    let clients = Clients {
        own_google: Some(Err(half.clone())),
        ..both_clients()
    };
    let refused = in_browser(
        &connector(&db, clients),
        Browser::Google,
        Wanted::Anyone,
        |url: &str| panic!("opened {url}"),
    )
    .await;
    assert!(matches!(refused, Err(BrowserSignInError::OwnClient(err)) if err == half));
}

#[test]
fn the_sign_in_says_which_client_it_uses() {
    let env = Clients {
        own_google: Some(Ok(OwnClient::new("env-cid", "s"))),
        ..both_clients()
    }
    .with_chosen(Some(OwnClient::new("setup-cid", "s")));
    let kept = OwnClient::new("kept-cid", "s");
    let setup = both_clients().with_chosen(Some(OwnClient::new("setup-cid", "s")));
    let cases = [
        (&env, Some(&kept), ClientSource::Environment, "env-cid"),
        (&setup, Some(&kept), ClientSource::Setup, "setup-cid"),
        (&both_clients(), Some(&kept), ClientSource::Kept, "kept-cid"),
        (&both_clients(), None, ClientSource::BuiltIn, "cid"),
    ];
    for (clients, kept, source, id) in cases {
        let choice = google_sign_in(clients, kept).unwrap();
        assert_eq!(choice.source, source);
        assert_eq!(choice.oauth.id(), id, "{source:?}");
        let kind = match source {
            ClientSource::BuiltIn => SignInClient::BuiltIn,
            _ => SignInClient::Own,
        };
        assert_eq!(choice.kind(), kind, "{source:?}");
        assert_eq!(
            choice.own.is_some(),
            kind == SignInClient::Own,
            "{source:?}"
        );
    }
    assert!(matches!(
        google_sign_in(&Clients::default(), None),
        Err(BrowserSignInError::NoClient(Browser::Google))
    ));
}

/// An own Google account `me@gmail.com` whose kept client is `kept-cid`,
/// in a store `reach` reads.
async fn own_account_kept(reach: &Connector) {
    google_signed_in(
        &reach.db,
        &reach.secrets,
        "me@gmail.com",
        "rt",
        0,
        None,
        "",
        Some(&OwnClient::new("kept-cid", "s")),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn signing_an_own_account_in_again_keeps_its_own_client() {
    let (db, _dir) = store().await;
    let reach = connector(&db, both_clients());
    own_account_kept(&reach).await;
    let url = page_opened_by(&reach, Browser::Google, Wanted::Only("me@gmail.com".into())).await;
    assert!(url.contains("client_id=kept-cid"), "{url}");
}

#[tokio::test]
async fn the_environment_wins_over_a_kept_client() {
    let (db, _dir) = store().await;
    let clients = Clients {
        own_google: Some(Ok(OwnClient::new("env-cid", "s"))),
        ..both_clients()
    };
    let reach = connector(&db, clients);
    own_account_kept(&reach).await;
    let url = page_opened_by(&reach, Browser::Google, Wanted::Only("me@gmail.com".into())).await;
    assert!(url.contains("client_id=env-cid"), "{url}");
}

#[tokio::test]
async fn the_setup_client_signs_in_over_the_built_in_one() {
    let clients = both_clients().with_chosen(Some(OwnClient::new("setup-cid", "s")));
    let url = page_opened_with(clients, Browser::Google, Wanted::Anyone).await;
    assert!(url.contains("client_id=setup-cid"), "{url}");
}

#[tokio::test]
async fn an_own_client_asks_for_every_permission() {
    let clients = both_clients().with_chosen(Some(OwnClient::new("setup-cid", "s")));
    let url = page_opened_with(clients, Browser::Google, Wanted::Anyone).await;
    assert!(url.contains("https%3A%2F%2Fmail.google.com%2F"), "{url}");
    assert!(url.contains("gmail.settings.basic"), "{url}");
}

#[tokio::test]
async fn an_own_account_signs_in_again_in_a_build_without_the_built_in_client() {
    let (db, _dir) = store().await;
    let reach = connector(&db, Clients::default());
    own_account_kept(&reach).await;
    let url = page_opened_by(&reach, Browser::Google, Wanted::Only("me@gmail.com".into())).await;
    assert!(url.contains("client_id=kept-cid"), "{url}");
}

/// A Gmail account at `me@gmail.com` that signed in with `client`, with
/// nothing in the keyring.
async fn gmail_account_of(db: &Db, client: SignInClient) -> Account {
    db.write(move |c| {
        let id = accounts::insert_account(c, "me@gmail.com", 0)?;
        accounts::set_gmail_sign_in(c, id, &GmailSignIn::BuiltInApi.with_google_token(client))?;
        Ok(accounts::account_by_email(c, "me@gmail.com")?.unwrap())
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_own_account_whose_key_is_gone_takes_no_other_client() {
    let (db, _dir) = store().await;
    let me = gmail_account_of(&db, SignInClient::Own).await;
    let refused = in_browser(
        &connector(&db, both_clients()),
        Browser::Google,
        Wanted::Only(me.email.clone()),
        |url: &str| panic!("opened {url}"),
    )
    .await;
    assert!(matches!(refused, Err(BrowserSignInError::OwnClientMissing)));
    let client = db
        .read(move |c| Ok(accounts::gmail_sign_in(c, me.id)?.client()))
        .await
        .unwrap();
    assert_eq!(client, SignInClient::Own);
}

#[tokio::test]
async fn an_own_account_whose_key_is_gone_signs_in_through_the_environment() {
    let (db, _dir) = store().await;
    gmail_account_of(&db, SignInClient::Own).await;
    let clients = Clients {
        own_google: Some(Ok(OwnClient::new("env-cid", "s"))),
        ..both_clients()
    };
    let reach = connector(&db, clients);
    let url = page_opened_by(&reach, Browser::Google, Wanted::Only("me@gmail.com".into())).await;
    assert!(url.contains("client_id=env-cid"), "{url}");
}

#[tokio::test]
async fn a_built_in_account_signs_in_again_through_the_built_in_client() {
    let (db, _dir) = store().await;
    gmail_account_of(&db, SignInClient::BuiltIn).await;
    let reach = connector(&db, both_clients());
    let url = page_opened_by(&reach, Browser::Google, Wanted::Only("me@gmail.com".into())).await;
    assert!(url.contains("client_id=cid&"), "{url}");
}

/// A built-in client that asks for no mail, as a release build carries.
fn mail_free_built_in() -> Clients {
    Clients {
        google: Some(OAuthClient::new("cid", "secret").asking(mailrs_gmail::ScopeList::BuiltIn)),
        ..both_clients()
    }
}

#[tokio::test]
async fn an_own_account_signs_in_again_beside_a_built_in_client_that_reads_no_mail() {
    let (db, _dir) = store().await;
    let reach = connector(&db, mail_free_built_in());
    own_account_kept(&reach).await;
    let url = page_opened_by(&reach, Browser::Google, Wanted::Only("me@gmail.com".into())).await;
    assert!(url.contains("client_id=kept-cid"), "{url}");
}

/// A switch to JMAP that crashed after saving its secret leaves it under
/// the IMAP or POP3 account's id.
#[tokio::test]
async fn forgetting_an_imap_or_pop3_account_deletes_a_jmap_secret_too() {
    let (db, _dir) = store().await;
    for account in [imap_account(&db).await, pop3_account(&db).await] {
        let reach = connector(&db, both_clients());
        reach.secrets.jmap.save(account.id, "token").unwrap();
        reach.forget(&account).await.unwrap();
        assert_eq!(
            reach.secrets.jmap.load(account.id).unwrap(),
            None,
            "{:?}",
            account.provider
        );
    }
}

fn oauth_row(session_url: &str, capabilities: &[&str]) -> mailrs_store::jmap::JmapSaved {
    use mailrs_store::jmap::{JmapAuth, JmapSaved, SavedOAuth};

    JmapSaved {
        session_url: session_url.into(),
        auth: JmapAuth::OAuth,
        user_name: None,
        labels: true,
        capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
        oauth: Some(SavedOAuth {
            client_id: "c1".into(),
            token_endpoint: "https://api.fastmail.com/oauth/refresh".into(),
        }),
        owner: None,
    }
}

fn new_oauth(saved: mailrs_store::jmap::JmapSaved) -> crate::sign_in::NewJmap {
    crate::sign_in::NewJmap {
        address: "me@fastmail.com".into(),
        provider_name: "Fastmail".into(),
        saved,
        secret: "refresh-1".into(),
        dav_password: None,
        signed_as: crate::sign_in::SignedAs::address("me@fastmail.com"),
    }
}

#[tokio::test]
async fn an_oauth_jmap_account_connects_from_its_refresh_token() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved), 0)
        .await
        .unwrap();
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(services.mail, AnyMail::Jmap(_)));
}

#[tokio::test]
async fn an_oauth_row_without_its_client_asks_for_a_sign_in() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let mut saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    saved.oauth = None;
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved), 0)
        .await
        .unwrap();
    let refused = crate::connect_jmap(&db, &secrets, &account, 30).await;
    assert!(matches!(
        refused,
        Err(crate::SyncError::Backend(crate::BackendError::NeedsReauth))
    ));
    assert_eq!(state(&db, &account).await, AccountState::NeedsReauth);
}

#[tokio::test]
async fn an_oauth_sign_in_the_keyring_refuses_keeps_nothing() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let crate::passwords::Passwords::Memory(jmap) = &*secrets.jmap else {
        unreachable!("tests keep secrets in memory")
    };
    jmap.refuse_saves();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let refused = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved), 0).await;
    assert!(matches!(
        refused,
        Err(crate::sign_in::JmapSignInError::Password(_))
    ));
    let left = db
        .read(|c| accounts::account_by_email(c, "me@fastmail.com"))
        .await
        .unwrap();
    assert!(left.is_none(), "no account without its refresh token");
    assert_eq!(
        db.read(|c| mailrs_store::jmap::load(c, 1)).await.unwrap(),
        None
    );
}

/// A session that signed in a moment ago, whose access token `a1` lives
/// another hour, so nothing in the test asks the token endpoint.
fn signed_in_session(
    session_url: &str,
    refresh_token: Option<&str>,
) -> Arc<mailrs_jmap::oauth::OAuthSession> {
    use mailrs_jmap::oauth::{OAuthClient, OAuthSession, Tokens};

    let client = OAuthClient {
        client_id: "c1".into(),
        token_endpoint: "https://127.0.0.1:9/oauth/refresh".into(),
        resource: session_url.into(),
    };
    let tokens = Tokens {
        access_token: "a1".into(),
        expires_in: 3600,
        refresh_token: refresh_token.map(str::to_string),
    };
    Arc::new(OAuthSession::signed_in(client, tokens))
}

#[tokio::test]
async fn one_oauth_session_signs_in_mail_and_the_calendar() {
    use crate::services::MailBackend;
    use mailrs_dav::{DavApi, DavClient, Kind, Login};
    use mailrs_store::services::{FoundService, ServiceKind};
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let session_url = format!("{}/jmap/session", server.uri());
    let saved = oauth_row(&session_url, &[mailrs_jmap::MAIL, mailrs_jmap::CONTACTS]);
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    let id = account.id;
    db.write(move |c| {
        mailrs_store::services::save(
            c,
            id,
            &FoundService {
                kind: ServiceKind::CalDav,
                url: "https://127.0.0.1/dav/".into(),
                user_name: "me@fastmail.com".into(),
                confirmed: true,
                source: "table".into(),
            },
        )
    })
    .await
    .unwrap();
    let session = signed_in_session(&session_url, Some("r1"));
    let sign_in = crate::connect::JmapSignIn::OAuth(Arc::clone(&session));
    let services = crate::connect::jmap_services(
        &db,
        Arc::clone(&secrets.passwords),
        &account,
        saved,
        sign_in,
        30,
        crate::connect::STEP_WAIT,
    )
    .await
    .unwrap();
    assert!(matches!(
        services.contacts,
        Some(crate::AnyContacts::Jmap(_))
    ));
    assert!(
        matches!(services.calendar, Some(crate::AnyCalendar::Dav(_))),
        "CalDAV serves the calendar the session lacks"
    );
    let _ = services.mail.mailboxes().await;
    // The calendar's client takes the session as its token source. Only a
    // client over plain http can reach the mock, so the test builds one.
    let calendar = DavClient::over(
        format!("{}/cal/", server.uri()).parse().unwrap(),
        Kind::Calendar,
        Login::new("me@fastmail.com", ""),
    )
    .unwrap()
    .with_bearer(crate::connect::session_tokens(session));
    let _ = calendar.homes().await;
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().any(|r| r.url.path() == "/jmap/session"),
        "mail asked the session"
    );
    assert!(
        requests.iter().any(|r| r.url.path().starts_with("/cal")),
        "the calendar asked its server"
    );
    for request in &requests {
        let auth = request
            .headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_string());
        assert_eq!(auth.as_deref(), Some("Bearer a1"), "{}", request.url.path());
    }
}

#[tokio::test]
async fn a_rotated_refresh_token_is_written_to_the_keyring_once() {
    let secrets = Secrets::memory();
    let keep = crate::connect::keep_rotations(Arc::clone(&secrets.jmap), 7);
    keep("r2".into()).await.unwrap();
    assert_eq!(secrets.jmap.load(7).unwrap().as_deref(), Some("r2"));
}

#[tokio::test]
async fn an_oauth_account_the_server_still_refuses_stops_and_asks_for_a_sign_in() {
    use std::time::Duration;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let (db, _dir) = store().await;
    let id = db
        .write(|c| accounts::insert_jmap_account(c, "me@fastmail.com", "Fastmail", 0))
        .await
        .unwrap()
        .unwrap();
    let session_url = format!("{}/jmap/session", server.uri());
    // No refresh token stands in for a grant the server revoked: the
    // session answers `Unauthorized` once the access token is refused.
    let auth = mailrs_jmap::Auth::OAuth(signed_in_session(&session_url, None));
    let client = mailrs_jmap::JmapClient::new(&session_url, auth).unwrap();
    let settings = crate::JmapSettings {
        address: "me@fastmail.com".into(),
        provider_name: "Fastmail".into(),
        window_days: 30,
        labels: true,
        capabilities: vec![mailrs_jmap::MAIL.into()],
        auto_reply_needs_sign_in: false,
    };
    let config = crate::EngineConfig {
        max_backoff: Duration::from_millis(20),
        restart_after: Duration::from_millis(20),
        ..crate::EngineConfig::default()
    };
    let (engine, events) = crate::SyncEngine::new(db.clone(), config);
    engine.start_account(id, crate::AccountServices::jmap(client, settings));
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(event) = events.recv().await {
            if matches!(
                event,
                mailrs_domain::ChangeEvent::AccountStateChanged {
                    state: AccountState::NeedsReauth,
                    ..
                }
            ) {
                return;
            }
        }
    })
    .await
    .expect("the account never asked for a sign-in");
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.is_running(id) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the account kept syncing");
    let asked = server.received_requests().await.unwrap().len();
    // Every backoff and restart the engine could still have set fires
    // here, well past them all, without a real wait.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        asked,
        "nothing calls the server after the stop"
    );
}

#[tokio::test]
async fn a_dav_server_on_another_site_never_sees_the_oauth_token() {
    use mailrs_store::services::{FoundService, ServiceKind};

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved), 0)
        .await
        .unwrap();
    let id = account.id;
    db.write(move |c| {
        for (kind, url) in [
            (ServiceKind::CalDav, "https://dav.example.org/cal/"),
            (
                ServiceKind::CardDav,
                "https://fastmail.com@dav.example.org/card/",
            ),
        ] {
            mailrs_store::services::save(
                c,
                id,
                &FoundService {
                    kind,
                    url: url.into(),
                    user_name: "me@fastmail.com".into(),
                    confirmed: true,
                    source: "table".into(),
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(
        services.calendar.is_none(),
        "a calendar server on another site is left out"
    );
    assert!(
        services.contacts.is_none(),
        "a contacts server on another site is left out"
    );
    db.write(move |c| {
        mailrs_store::services::save(
            c,
            id,
            &FoundService {
                kind: ServiceKind::CalDav,
                url: "https://caldav.fastmail.com/dav/".into(),
                user_name: "me@fastmail.com".into(),
                confirmed: true,
                source: "table".into(),
            },
        )
    })
    .await
    .unwrap();
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(
        matches!(services.calendar, Some(crate::AnyCalendar::Dav(_))),
        "the session's own site gets the token"
    );
}

#[tokio::test]
async fn calendar_calls_refused_together_share_the_sessions_refresh() {
    let session = signed_in_session("https://api.fastmail.com/jmap/session", Some("r1"));
    let tokens = crate::connect::session_tokens(session);
    assert_eq!(tokens.token(None).await.unwrap(), "a1");
    // Another call already moved past the refused token: no refresh.
    assert_eq!(tokens.token(Some("a0")).await.unwrap(), "a1");
    // The held token itself was refused: only now does a refresh go out,
    // here to a token endpoint nothing answers.
    assert!(tokens.token(Some("a1")).await.is_err());
}

#[test]
fn an_oauth_row_never_sends_its_refresh_token_as_an_access_token() {
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    assert!(crate::auth_of(&saved, "me@fastmail.com", "refresh-1".into()).is_err());
}

#[tokio::test]
async fn a_restart_reuses_the_accounts_oauth_session() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved), 0)
        .await
        .unwrap();
    crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    let first = secrets
        .jmap_sessions
        .held(account.id)
        .expect("the start keeps its session");
    crate::connect_jmap(&db, &secrets.clone(), &account, 30)
        .await
        .unwrap();
    let again = secrets.jmap_sessions.held(account.id).unwrap();
    assert!(
        Arc::ptr_eq(&first, &again),
        "a second session would race the first to rotate"
    );
    secrets.forget(&account).await.unwrap();
    assert!(
        secrets.jmap_sessions.held(account.id).is_none(),
        "a removed account leaves no session"
    );
    assert!(
        first.retired(),
        "a task still holding the session would save under a removed account"
    );
}

#[tokio::test]
async fn removing_a_browser_account_hands_its_newest_refresh_token_to_revoke() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved), 0)
        .await
        .unwrap();
    crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    let session = secrets.jmap_sessions.held(account.id).unwrap();
    let row = crate::jmap_grant(&db, &account)
        .await
        .expect("a browser account has a grant to revoke");
    // Reading the grant retires nothing: the account's row may still fail
    // to go, and the account then keeps running on this session.
    assert!(
        !session.retired(),
        "the session was retired before its account went"
    );
    let grant = row.retire(&secrets).await.expect("the grant's token");
    assert!(
        session.retired(),
        "the session is retired before its token is revoked"
    );
    assert_eq!(
        (
            grant.session_url.as_str(),
            grant.client_id.as_str(),
            grant.refresh_token.as_str()
        ),
        ("https://api.fastmail.com/jmap/session", "c1", "refresh-1")
    );
}

#[tokio::test]
async fn a_token_account_has_no_grant_to_revoke() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let account =
        crate::sign_in::jmap_signed_in(&db, &secrets, new_jmap("me@fastmail.com", "fmu1-token"), 0)
            .await
            .unwrap();
    assert!(crate::jmap_grant(&db, &account).await.is_none());
}

#[tokio::test]
async fn signing_in_again_retires_the_session_it_replaces() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    let old = secrets.jmap_sessions.held(account.id).unwrap();
    let mut again = new_oauth(saved);
    again.secret = "refresh-2".into();
    crate::sign_in::jmap_signed_in(&db, &secrets, again, 1)
        .await
        .unwrap();
    assert!(
        old.retired(),
        "the old session could write its token over the new grant's"
    );
    assert_eq!(
        secrets.jmap.load(account.id).unwrap().as_deref(),
        Some("refresh-2")
    );
}

#[tokio::test]
async fn a_sign_in_again_the_keyring_refuses_leaves_the_old_session_saving() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    let old = secrets.jmap_sessions.held(account.id).unwrap();
    let crate::passwords::Passwords::Memory(kept) = &*secrets.jmap else {
        unreachable!("tests keep secrets in memory")
    };
    kept.refuse_saves();
    let mut again = new_oauth(saved);
    again.secret = "refresh-2".into();
    let refused = crate::sign_in::jmap_signed_in(&db, &secrets, again, 1).await;
    assert!(matches!(
        refused,
        Err(crate::sign_in::JmapSignInError::Password(_))
    ));
    assert!(
        !old.retired(),
        "the account still runs on the old grant and must keep saving it"
    );
    assert!(Arc::ptr_eq(
        &old,
        &secrets.jmap_sessions.held(account.id).unwrap()
    ));
}

/// What the account keeps for `kind`, if anything.
async fn kept(
    db: &Db,
    account: &Account,
    kind: mailrs_store::services::ServiceKind,
) -> Option<mailrs_store::services::FoundService> {
    let id = account.id;
    db.read(move |c| mailrs_store::services::load(c, id))
        .await
        .unwrap()
        .into_iter()
        .find(|f| f.kind == kind)
}

#[tokio::test]
async fn an_oauth_fastmail_account_reaches_the_tables_calendar_and_contacts_with_its_token() {
    use mailrs_store::services::ServiceKind;

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    let (found, changed) =
        crate::services::finding::keep_oauth_table(&db, &account, &saved.session_url)
            .await
            .unwrap();
    assert!(changed, "new servers restart the account's services");
    let calendar = kept(&db, &account, ServiceKind::CalDav)
        .await
        .expect("the table's calendar server is kept");
    let contacts = kept(&db, &account, ServiceKind::CardDav)
        .await
        .expect("the table's contacts server is kept");
    assert_eq!(calendar.url, "https://caldav.fastmail.com/dav/calendars");
    assert_eq!(
        contacts.url,
        "https://carddav.fastmail.com/dav/addressbooks"
    );
    assert!(calendar.confirmed && contacts.confirmed);
    assert!(found.caldav.is_some() && found.carddav.is_some());
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(
        services.calendar,
        Some(crate::AnyCalendar::Dav(_))
    ));
    assert!(matches!(
        services.contacts,
        Some(crate::AnyContacts::Dav(_))
    ));
    let plan = crate::connect::dav_plan(vec![calendar, contacts], &saved, true, None, None, &[]);
    assert_eq!(plan.len(), 2);
    assert!(
        plan.iter()
            .all(|(_, sign)| matches!(sign, crate::connect::DavSign::Bearer)),
        "both take the session's token"
    );
}

#[tokio::test]
async fn a_dav_host_off_the_table_waits_for_a_yes_on_an_oauth_account() {
    use mailrs_store::services::{FoundService, ServiceKind};

    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://jmap.example.org/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let mut new = new_oauth(saved.clone());
    new.provider_name = "jmap.example.org".into();
    new.address = "me@example.org".into();
    new.signed_as = crate::sign_in::SignedAs::address("me@example.org");
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new, 0)
        .await
        .unwrap();
    let id = account.id;
    db.write(move |c| {
        mailrs_store::services::save(
            c,
            id,
            &FoundService {
                kind: ServiceKind::CalDav,
                url: "https://dav.example.org/cal/".into(),
                user_name: "me@example.org".into(),
                confirmed: false,
                source: "well-known".into(),
            },
        )
    })
    .await
    .unwrap();
    let (found, changed) =
        crate::services::finding::keep_oauth_table(&db, &account, &saved.session_url)
            .await
            .unwrap();
    assert!(
        !changed && found.caldav.is_none(),
        "the table names no server for this provider"
    );
    assert!(
        !kept(&db, &account, ServiceKind::CalDav)
            .await
            .unwrap()
            .confirmed,
        "a found host still waits for a yes"
    );
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(
        services.calendar.is_none(),
        "an unconfirmed host gets no token"
    );
}

#[tokio::test]
async fn the_tables_hosts_on_another_site_than_the_session_are_not_kept() {
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    // Fastmail's name, but a session somewhere else.
    let saved = oauth_row(
        "https://jmap.example.org/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    let (found, changed) =
        crate::services::finding::keep_oauth_table(&db, &account, &saved.session_url)
            .await
            .unwrap();
    assert!(!changed && found.caldav.is_none() && found.carddav.is_none());
}

/// A token source that hands out `a1`, and `a2` after one refresh.
struct Refreshing(std::sync::atomic::AtomicUsize);

impl mailrs_dav::TokenSource for Refreshing {
    fn token<'a>(
        &'a self,
        refused: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            match refused {
                Some(_) => {
                    self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("a2".to_string())
                }
                None => Ok("a1".to_string()),
            }
        })
    }
}

#[tokio::test]
async fn a_calendar_server_that_refuses_the_fresh_token_asks_for_an_app_password() {
    use mailrs_dav::{DavApi, DavClient, Kind, Login};
    use mailrs_store::services::{Miss, ServiceKind};
    use std::time::Duration;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    crate::services::finding::keep_oauth_table(&db, &account, &saved.session_url)
        .await
        .unwrap();
    let refreshing = Arc::new(Refreshing(std::sync::atomic::AtomicUsize::new(0)));
    let source = crate::connect::dav_bearer(
        Arc::clone(&refreshing) as Arc<dyn mailrs_dav::TokenSource>,
        db.clone(),
        account.id,
        ServiceKind::CalDav,
    );
    let calendar = DavClient::over(
        format!("{}/cal/", server.uri()).parse().unwrap(),
        Kind::Calendar,
        Login::new("me@fastmail.com", ""),
    )
    .unwrap()
    .with_bearer(source);
    assert!(calendar.homes().await.is_err());
    let id = account.id;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let misses = db.read(mailrs_store::services::all_misses).await.unwrap();
            if misses.contains(&(id, ServiceKind::CalDav, Miss::Refused)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the refusal was never kept");
    let asked = server.received_requests().await.unwrap().len();
    assert!(calendar.homes().await.is_err());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        asked,
        "a refused server is not asked again"
    );
    assert_eq!(
        refreshing.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one refresh and no more"
    );
    // The next start leaves the calendar out instead of refreshing again.
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(
        services.calendar.is_none(),
        "the refused calendar waits for an app password"
    );
    assert!(
        matches!(services.contacts, Some(crate::AnyContacts::Dav(_))),
        "contacts still take the token"
    );
    // An app password brings it back, over Basic.
    secrets.passwords.save(account.id, "app-password").unwrap();
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(matches!(
        services.calendar,
        Some(crate::AnyCalendar::Dav(_))
    ));
    let calendar_row = kept(&db, &account, ServiceKind::CalDav).await.unwrap();
    let plan = crate::connect::dav_plan(
        vec![calendar_row],
        &saved,
        true,
        Some("app-password".into()),
        None,
        &[ServiceKind::CalDav],
    );
    assert!(matches!(&plan[0].1, crate::connect::DavSign::Password(p) if p == "app-password"));
}

#[tokio::test]
async fn a_calendar_that_refuses_a_new_grant_too_asks_for_the_app_password_again() {
    use mailrs_dav::{DavApi, DavClient, Kind, Login};
    use mailrs_store::services::{Miss, ServiceKind};
    use std::time::Duration;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let (db, _dir) = store().await;
    let secrets = Secrets::memory();
    let saved = oauth_row(
        "https://api.fastmail.com/jmap/session",
        &[mailrs_jmap::MAIL],
    );
    let account = crate::sign_in::jmap_signed_in(&db, &secrets, new_oauth(saved.clone()), 0)
        .await
        .unwrap();
    crate::services::finding::keep_oauth_table(&db, &account, &saved.session_url)
        .await
        .unwrap();
    let id = account.id;
    db.write(move |c| {
        mailrs_store::services::save_miss(c, id, ServiceKind::CalDav, Some(Miss::Refused))
    })
    .await
    .unwrap();
    // What a browser sign-in does with its new grant.
    crate::connect::try_the_token_again(&db, id).await.unwrap();
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(
        matches!(services.calendar, Some(crate::AnyCalendar::Dav(_))),
        "the calendar gets one more try with the token"
    );
    let refreshing = Arc::new(Refreshing(std::sync::atomic::AtomicUsize::new(0)));
    let source = crate::connect::dav_bearer(
        Arc::clone(&refreshing) as Arc<dyn mailrs_dav::TokenSource>,
        db.clone(),
        id,
        ServiceKind::CalDav,
    );
    let calendar = DavClient::over(
        format!("{}/cal/", server.uri()).parse().unwrap(),
        Kind::Calendar,
        Login::new("me@fastmail.com", ""),
    )
    .unwrap()
    .with_bearer(source);
    assert!(calendar.homes().await.is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !db
            .read(mailrs_store::services::all_misses)
            .await
            .unwrap()
            .contains(&(id, ServiceKind::CalDav, Miss::Refused))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the refusal was never kept again");
    assert_eq!(
        refreshing.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one refresh for the new grant"
    );
    let services = crate::connect_jmap(&db, &secrets, &account, 30)
        .await
        .unwrap();
    assert!(
        services.calendar.is_none(),
        "the calendar waits for an app password again"
    );
}

/// A JMAP account over Basic whose session offers Sieve, as one switched
/// from IMAP is, with `local` rules kept in the store.
async fn sieve_account_with(
    local: &[mailrs_domain::Filter],
) -> (crate::AccountServices, tempfile::TempDir) {
    use mailrs_store::jmap::{JmapAuth, JmapSaved};

    let (db, dir) = store().await;
    let id = db
        .write(|c| accounts::insert_jmap_account(c, "me@example.com", "Stalwart", 0))
        .await
        .unwrap()
        .unwrap();
    let local = local.to_vec();
    db.write(move |c| {
        local
            .iter()
            .try_for_each(|rule| mailrs_store::local_rules::add(c, id, rule))
    })
    .await
    .unwrap();
    let account = db
        .read(move |c| accounts::account(c, id))
        .await
        .unwrap()
        .unwrap();
    let saved = JmapSaved {
        session_url: "https://127.0.0.1:9/jmap/session".into(),
        auth: JmapAuth::Basic,
        user_name: None,
        labels: true,
        capabilities: vec![mailrs_jmap::MAIL.into(), mailrs_jmap::SIEVE.into()],
        oauth: None,
        owner: None,
    };
    let sign_in = crate::connect::JmapSignIn::Secret("pw".into());
    let services = crate::connect::jmap_services(
        &db,
        Arc::clone(&Secrets::memory().passwords),
        &account,
        saved,
        sign_in,
        30,
        crate::connect::STEP_WAIT,
    )
    .await
    .unwrap();
    (services, dir)
}

#[tokio::test]
async fn a_sieve_session_keeps_local_rules_beside_its_script() {
    let rule = mailrs_domain::Filter {
        id: Some("local-1".into()),
        ..mailrs_domain::Filter::block("pest@example.com")
    };
    let (services, _dir) = sieve_account_with(&[rule]).await;
    let rules = services.rules.expect("rules");
    assert!(matches!(rules, crate::AnyRules::Beside(_)));
    assert!(
        rules.runs_here(),
        "the local rules still run on this computer"
    );
    assert!(
        matches!(services.auto_reply, Some(crate::AnyAutoReply::JmapSieve(_))),
        "the reply stays in the script"
    );
}

#[tokio::test]
async fn a_sieve_session_with_no_local_rules_runs_the_script_alone() {
    let (services, _dir) = sieve_account_with(&[]).await;
    assert!(matches!(
        services.rules,
        Some(crate::AnyRules::JmapSieve(_))
    ));
}

/// A build whose only Google client is the built-in one: it asks for no
/// mail, so a sign-in through it cannot read any.
fn built_in_clients() -> Clients {
    Clients {
        google: Some(OAuthClient::new("cid", "secret").asking(mailrs_gmail::ScopeList::BuiltIn)),
        ..both_clients()
    }
}

#[tokio::test]
async fn the_built_in_client_never_signs_in_an_api_account() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, built_in_clients());
    let mut opened = false;
    let refused = in_browser(
        &reach,
        Browser::Google,
        Wanted::Only(me.email.clone()),
        |_| opened = true,
    )
    .await;
    assert!(matches!(refused, Err(BrowserSignInError::NeedsRoute)));
    assert!(!opened, "the browser never opened");
}

#[tokio::test]
async fn the_built_in_client_adds_no_new_gmail_account() {
    let (db, _dir) = store().await;
    let reach = connector(&db, built_in_clients());
    let mut opened = false;
    let refused = in_browser(&reach, Browser::Google, Wanted::Anyone, |_| opened = true).await;
    assert!(matches!(refused, Err(BrowserSignInError::NeedsRoute)));
    assert!(!opened, "the browser never opened");
    let held = db.read(accounts::list_accounts).await.unwrap();
    assert!(held.is_empty(), "{held:?}");
}

#[tokio::test]
async fn the_built_in_client_refuses_an_address_with_no_gmail_account() {
    let (db, _dir) = store().await;
    let imap = imap_account(&db).await;
    let reach = connector(&db, built_in_clients());
    for wanted in [
        Wanted::Only("nobody@gmail.com".into()),
        Wanted::Only(imap.email),
    ] {
        let refused = in_browser(&reach, Browser::Google, wanted, |url: &str| {
            panic!("opened {url}")
        })
        .await;
        assert!(matches!(refused, Err(BrowserSignInError::NeedsRoute)));
    }
}

fn id_token(email: &str) -> String {
    use base64::Engine;
    let claims = serde_json::json!({"email": email, "email_verified": true});
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("e30.{body}.sig")
}

/// The built-in client against a token endpoint that answers with an ID
/// token for `email`. The "browser" is a request to the redirect the
/// sign-in listens on.
pub(super) async fn built_in_clients_answering(
    server: &wiremock::MockServer,
    email: &str,
) -> Clients {
    use wiremock::matchers::{method, path};
    wiremock::Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-1",
                "expires_in": 3600,
                "refresh_token": "rt-new",
                "scope": mailrs_gmail::BUILT_IN_SCOPES[2..].join(" "),
                "id_token": id_token(email),
            })),
        )
        .mount(server)
        .await;
    let built_in = OAuthClient::new("cid", "secret")
        .with_endpoints(
            format!("{}/auth", server.uri()),
            format!("{}/token", server.uri()),
        )
        .asking(mailrs_gmail::ScopeList::BuiltIn);
    Clients {
        google: Some(built_in),
        ..both_clients()
    }
}

pub(super) fn come_back_from_google(consent_url: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let value = |key: &str| {
        let (_, rest) = consent_url.split_once(&format!("{key}=")).unwrap();
        rest.split('&').next().unwrap().to_string()
    };
    let redirect = value("redirect_uri")
        .trim_start_matches("http%3A%2F%2F")
        .replace("%3A", ":");
    let state = value("state");
    tokio::spawn(async move {
        let mut stream = tokio::net::TcpStream::connect(&redirect).await.unwrap();
        let request = format!("GET /?code=c&state={state} HTTP/1.1\r\nHost: {redirect}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
    });
}

#[tokio::test]
async fn the_calendar_consent_keeps_the_route_and_records_the_built_in_list() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| {
        accounts::set_gmail_sign_in(
            c,
            id,
            &GmailSignIn::AppPassword {
                google: SignInClient::BuiltIn,
            },
        )
    })
    .await
    .unwrap();
    let server = wiremock::MockServer::start().await;
    // The ID token names the address in another case, as Google may.
    let reach = connector(
        &db,
        built_in_clients_answering(&server, "Me@Gmail.com").await,
    );
    let signed = calendar_consent(&reach, &me, come_back_from_google)
        .await
        .unwrap();
    assert_eq!(signed.id, me.id);
    let (route, client, consent) = db
        .read(move |c| {
            Ok((
                accounts::gmail_sign_in(c, id)?.route(),
                accounts::gmail_sign_in(c, id)?.client(),
                accounts::consent(c, id)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(route, MailRoute::Imap);
    assert_eq!(client, SignInClient::BuiltIn);
    assert_eq!(
        consent.asked.as_deref(),
        Some(mailrs_gmail::BUILT_IN_SCOPES.join(" ").as_str())
    );
    assert_eq!(
        reach
            .secrets
            .google
            .load("me@gmail.com")
            .unwrap()
            .as_deref(),
        Some("rt-new")
    );
}

/// An IMAP-route account signed in through the built-in client starts on
/// its route's services, never on Gmail API mail over a token that holds
/// no mail scope. Its route has no mail adapter yet, so it waits for one.
#[tokio::test]
async fn the_built_in_sign_in_starts_an_imap_route_account_on_its_route() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    on_imap_route(&db, &me).await;
    let server = wiremock::MockServer::start().await;
    let reach = connector(
        &db,
        built_in_clients_answering(&server, "me@gmail.com").await,
    );
    let signed = in_browser(
        &reach,
        Browser::Google,
        Wanted::Only(me.email.clone()),
        come_back_from_google,
    )
    .await
    .unwrap();
    assert_eq!(signed.account.id, me.id);
    assert!(
        matches!(signed.services, Connected::NeedsSignIn(Lacks::Secret)),
        "{:?}",
        matches!(&signed.services, Connected::Ready(s) if matches!(s.mail, AnyMail::Google(_)))
    );
}

#[tokio::test]
async fn the_calendar_consent_refuses_an_api_account_before_the_browser_opens() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, built_in_clients());
    let refused = calendar_consent(&reach, &me, |url: &str| panic!("opened {url}")).await;
    assert!(matches!(refused, Err(BrowserSignInError::NeedsRoute)));
}

#[tokio::test]
async fn the_calendar_consent_is_refused_for_another_address() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| {
        accounts::set_gmail_sign_in(
            c,
            id,
            &GmailSignIn::AppPassword {
                google: SignInClient::BuiltIn,
            },
        )
    })
    .await
    .unwrap();
    let server = wiremock::MockServer::start().await;
    let reach = connector(
        &db,
        built_in_clients_answering(&server, "other@gmail.com").await,
    );
    let refused = calendar_consent(&reach, &me, come_back_from_google).await;
    assert!(
        matches!(refused, Err(BrowserSignInError::OtherAddress { .. })),
        "{:?}",
        refused.err()
    );
}

async fn on_imap_route(db: &Db, me: &Account) {
    let id = me.id;
    db.write(move |c| {
        accounts::set_gmail_sign_in(
            c,
            id,
            &GmailSignIn::AppPassword {
                google: SignInClient::BuiltIn,
            },
        )
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn an_api_account_gets_every_google_service() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let reach = connector(&db, both_clients());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    let Connected::Ready(services) = reach.connect(&me).await.unwrap() else {
        panic!("not ready")
    };
    assert!(matches!(services.mail, AnyMail::Google(_)));
    assert!(matches!(
        services.calendar,
        Some(crate::AnyCalendar::Google(_))
    ));
    assert!(matches!(
        services.contacts,
        Some(crate::AnyContacts::Google(_))
    ));
    assert!(matches!(services.rules, Some(crate::AnyRules::Google(_))));
    assert!(matches!(
        services.auto_reply,
        Some(crate::AnyAutoReply::Google(_))
    ));
}

#[tokio::test]
async fn an_imap_route_account_waits_for_its_mail_adapter() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    on_imap_route(&db, &me).await;
    let reach = connector(&db, both_clients());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    let connected = reach.connect(&me).await.unwrap();
    assert!(matches!(connected, Connected::NeedsSignIn(Lacks::Secret)));
}

#[tokio::test]
async fn the_google_side_of_an_imap_route_account_comes_from_its_token() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    on_imap_route(&db, &me).await;
    let reach = connector(&db, both_clients());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    let google = google_side(&reach, &me).await.unwrap();
    assert!(google.is_some());
}

#[tokio::test]
async fn an_imap_route_account_without_a_calendar_token_has_no_google_side_and_keeps_its_state() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    on_imap_route(&db, &me).await;
    let before = state(&db, &me).await;
    let reach = connector(&db, both_clients());
    assert!(google_side(&reach, &me).await.unwrap().is_none());
    assert_eq!(state(&db, &me).await, before);
}

#[tokio::test]
async fn an_imap_route_account_in_a_build_without_a_client_has_no_google_side_and_keeps_its_state()
{
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    on_imap_route(&db, &me).await;
    let before = state(&db, &me).await;
    let reach = connector(&db, Clients::default());
    reach.secrets.google.save(&me.email, "rt").unwrap();
    assert!(google_side(&reach, &me).await.unwrap().is_none());
    assert_eq!(state(&db, &me).await, before);
}

/// The page `calendar_consent` opens for `me`, read before anyone comes
/// back from it.
async fn consent_page_for(reach: &Connector, me: &Account) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let run = calendar_consent(reach, me, move |url: &str| {
        let _ = tx.send(url.to_string());
    });
    tokio::select! {
        ended = run => panic!("the consent ended before anyone came back: {:?}", ended.err()),
        url = rx => url.unwrap(),
    }
}

#[tokio::test]
async fn the_calendar_consent_of_an_app_password_account_on_its_own_project_asks_that_project() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| {
        let own_imap = GmailSignIn::AppPassword {
            google: SignInClient::Own,
        };
        accounts::set_gmail_sign_in(c, id, &own_imap)
    })
    .await
    .unwrap();
    let reach = connector(&db, both_clients());
    reach
        .secrets
        .google_clients
        .save(id, &OwnClient::new("kept-cid", "s").encode())
        .unwrap();

    let url = consent_page_for(&reach, &me).await;

    assert!(url.contains("client_id=kept-cid"), "{url}");
}

#[tokio::test]
async fn the_calendar_consent_of_an_account_whose_project_went_asks_the_built_in_client() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    let id = me.id;
    db.write(move |c| {
        let own_imap = GmailSignIn::AppPassword {
            google: SignInClient::Own,
        };
        accounts::set_gmail_sign_in(c, id, &own_imap)
    })
    .await
    .unwrap();

    let url = consent_page_for(&connector(&db, both_clients()), &me).await;

    assert!(url.contains("client_id=cid"), "{url}");
}

#[tokio::test]
async fn signing_an_app_password_account_in_again_through_the_setup_asks_only_for_its_calendar() {
    let (db, _dir) = store().await;
    let me = gmail_account(&db).await;
    on_imap_route(&db, &me).await;
    let clients = both_clients().with_chosen(Some(OwnClient::new("setup-cid", "s")));
    let reach = connector(&db, clients);

    let url = page_opened_by(&reach, Browser::Google, Wanted::Only(me.email.clone())).await;

    assert!(url.contains("client_id=cid"), "{url}");
    assert!(!url.contains("setup-cid"), "{url}");
}
