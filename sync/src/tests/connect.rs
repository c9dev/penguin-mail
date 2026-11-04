use std::sync::Arc;

use mailrs_dav::Kind;
use mailrs_domain::calendar::Calendar;
use mailrs_domain::{Account, AccountId, AccountState, Provider};
use mailrs_gmail::{MemoryTokenStore, OAuthClient, TokenStore};
use mailrs_sieve::script::Extensions;
use mailrs_store::servers::{self, Saved, Security, Servers};
use mailrs_store::services::{self, FoundService, ServiceKind};
use mailrs_store::{Db, accounts, calendar as store_calendar};

use crate::passwords::{MemoryPasswords, PasswordError, PasswordStore};
use crate::services::finding::ServiceProbe;
use crate::sign_in::{
    CalDavSignInError, ImapSignInError, NewCalDav, NewImap, NewPop3, caldav_signed_in,
    caldav_signed_in_again, calendar_account_name, imap_signed_in, in_imap_words, pop3_signed_in,
};
use crate::{
    BackendError, MailBackend, SyncError, connect_account, connect_imap, server_of, servers_for,
    smtp_login,
};
use mailrs_domain::RemoveSetting;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_microsoft_refresh_cannot_overwrite_a_replacement_accounts_secret_or_grant() {
    late_microsoft_refresh(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signing_in_again_retires_earlier_microsoft_refresh_callbacks() {
    late_microsoft_refresh(true).await;
}

async fn late_microsoft_refresh(signed_in_again: bool) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let (db, _dir) = store().await;
    let owner = db
        .write(|c| {
            let id = accounts::insert_microsoft_account(c, "old@contoso.com", "Microsoft 365", 0)?
                .unwrap();
            accounts::account(c, id)
        })
        .await
        .unwrap()
        .unwrap();
    let tokens = Arc::new(MemoryPasswords::default());
    tokens.save(owner.id, "old-refresh").unwrap();
    let server = MockServer::start().await;
    let checking = Arc::new(tokio::sync::Notify::new());
    let reached = checking.clone();
    let (release, resumed) = std::sync::mpsc::channel();
    let resumed = std::sync::Mutex::new(resumed);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |_: &wiremock::Request| {
            reached.notify_one();
            resumed
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token":"old-access", "refresh_token":"old-rotated", "expires_in":3600,
                "scope":"Mail.ReadWrite.Shared"
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"value":[]})))
        .mount(&server)
        .await;
    let client = mailrs_graph::MicrosoftClient::new("test").with_endpoints(
        format!("{}/authorize", server.uri()),
        format!("{}/token", server.uri()),
    );
    let graph = crate::connect::microsoft_graph_within(
        &db,
        tokens.clone(),
        client,
        &owner,
        &format!("{}/v1.0/", server.uri()),
        std::time::Duration::from_secs(10),
    )
    .await
    .unwrap();
    let reading = tokio::spawn(async move { graph.folders(None, None).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), checking.notified())
        .await
        .unwrap();
    let turn = crate::sign_in::secrets_turn().await;
    let id = owner.id;
    let replacement = db
        .write(move |c| {
            let replacement = if signed_in_again {
                accounts::renew_microsoft_session(c, id)?;
                id
            } else {
                accounts::delete_account(c, id)?;
                let id =
                    accounts::insert_microsoft_account(c, "new@contoso.com", "Microsoft 365", 1)?
                        .unwrap();
                accounts::ensure_microsoft_session(c, id)?;
                id
            };
            accounts::set_granted(c, replacement, "Contacts.ReadWrite")?;
            Ok(replacement)
        })
        .await
        .unwrap();
    assert_eq!(replacement, id);
    tokens.save(replacement, "replacement-refresh").unwrap();
    release.send(()).unwrap();
    reading.await.unwrap().unwrap();
    drop(turn);
    // Rotation callbacks save off the HTTP task. Wait for their writes,
    // including a wrong write from the retired session, to reach the store.
    for _ in 0..100 {
        if tokens.load(id).unwrap().as_deref() == Some("old-rotated") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        tokens.load(id).unwrap().as_deref(),
        Some("replacement-refresh")
    );
    assert_eq!(
        db.read(move |c| accounts::consent(c, id))
            .await
            .unwrap()
            .granted
            .as_deref(),
        Some("Contacts.ReadWrite")
    );
}

#[tokio::test]
async fn connecting_needs_a_stored_refresh_token() {
    let (db, _dir) = store().await;
    let id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let tokens: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::default());
    let account = Account {
        id,
        email: "me@example.com".into(),
        state: AccountState::Ok,
        provider: mailrs_domain::Provider::Gmail,
        provider_name: None,
    };
    let oauth = OAuthClient::new("cid", "secret");
    assert!(matches!(
        connect_account(oauth.clone(), Arc::clone(&tokens), &account, &db).await,
        Err(SyncError::Backend(crate::BackendError::NeedsReauth))
    ));
    tokens.save("me@example.com", "rt").unwrap();
    let client = connect_account(oauth, tokens, &account, &db).await.unwrap();
    assert_eq!(client.account_id, 1);
}

/// A refresh token loaded for an account whose store row already holds
/// scopes seeds the client, so [`AccountServices::withheld`] reads the
/// right answer before the first call, rather than everything until one
/// refresh has happened.
#[tokio::test]
async fn a_stored_grant_seeds_the_client() {
    let (db, _dir) = store().await;
    let tokens: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::default());
    tokens.save("me@example.com", "rt").unwrap();
    let id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    db.write(move |c| {
        accounts::set_granted(
            c,
            id,
            "https://www.googleapis.com/auth/gmail.modify \
             https://www.googleapis.com/auth/gmail.settings.basic",
        )
    })
    .await
    .unwrap();
    let account = account(&db, "me@example.com").await;
    let oauth = OAuthClient::new("cid", "secret");
    let client = connect_account(oauth, tokens, &account, &db).await.unwrap();
    assert!(client.client.granted().unwrap().reads_mail());
}

fn fastmail() -> Servers {
    let saved = |host: &str, port| Saved {
        host: host.into(),
        port,
        security: Security::Tls,
        user_name: "dana@fastmail.com".into(),
    };
    Servers {
        imap: saved("imap.fastmail.com", 993),
        smtp: saved("smtp.fastmail.com", 465),
    }
}

/// iCloud's IMAP server takes the part before @ and its SMTP server the
/// whole address, so the two rows keep different user names.
fn icloud() -> Servers {
    Servers {
        imap: Saved {
            host: "imap.mail.me.com".into(),
            port: 993,
            security: Security::Tls,
            user_name: "dana".into(),
        },
        smtp: Saved {
            host: "smtp.mail.me.com".into(),
            port: 587,
            security: Security::StartTls,
            user_name: "dana@icloud.com".into(),
        },
    }
}

fn new_fastmail(password: &str) -> NewImap {
    NewImap {
        address: "dana@fastmail.com".into(),
        provider_name: "Fastmail".into(),
        servers: fastmail(),
        password: password.into(),
        smtp_password: None,
    }
}

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

/// A keyring that is locked, or that the desktop has none of.
struct Refusing;

impl PasswordStore for Refusing {
    fn load(&self, _: AccountId) -> Result<Option<String>, PasswordError> {
        Ok(None)
    }
    fn save(&self, _: AccountId, _: &str) -> Result<(), PasswordError> {
        Err(PasswordError::Keyring("the collection is locked".into()))
    }
    fn delete(&self, _: AccountId) -> Result<(), PasswordError> {
        Ok(())
    }
    fn load_outgoing(&self, _: AccountId) -> Result<Option<String>, PasswordError> {
        Ok(None)
    }
    fn save_outgoing(&self, _: AccountId, _: &str) -> Result<(), PasswordError> {
        Err(PasswordError::Keyring("the collection is locked".into()))
    }
    fn delete_outgoing(&self, _: AccountId) -> Result<(), PasswordError> {
        Ok(())
    }
}

/// A keyring that never answers, like one waiting on an unlock prompt
/// that nobody sees because only the tray is running. It answers once
/// the test drops `release`, so the runtime can shut down.
struct Silent(std::sync::Mutex<std::sync::mpsc::Receiver<()>>);

fn silent() -> (Arc<Silent>, std::sync::mpsc::Sender<()>) {
    let (release, held) = std::sync::mpsc::channel();
    (Arc::new(Silent(std::sync::Mutex::new(held))), release)
}

impl PasswordStore for Silent {
    fn load(&self, _: AccountId) -> Result<Option<String>, PasswordError> {
        let _ = self.0.lock().unwrap().recv();
        Ok(None)
    }
    fn save(&self, _: AccountId, _: &str) -> Result<(), PasswordError> {
        Ok(())
    }
    fn delete(&self, _: AccountId) -> Result<(), PasswordError> {
        Ok(())
    }
    fn load_outgoing(&self, _: AccountId) -> Result<Option<String>, PasswordError> {
        let _ = self.0.lock().unwrap().recv();
        Ok(None)
    }
    fn save_outgoing(&self, _: AccountId, _: &str) -> Result<(), PasswordError> {
        Ok(())
    }
    fn delete_outgoing(&self, _: AccountId) -> Result<(), PasswordError> {
        Ok(())
    }
}

const SHORT: std::time::Duration = std::time::Duration::from_millis(50);

#[tokio::test]
async fn a_microsoft_connect_gives_up_on_a_keyring_that_never_answers() {
    let (db, _dir) = store().await;
    db.write(|c| accounts::insert_microsoft_account(c, "dana@outlook.com", "Outlook", 0))
        .await
        .unwrap()
        .unwrap();
    let account = account(&db, "dana@outlook.com").await;
    let client = mailrs_graph::MicrosoftClient::new("00000000-0000-0000-0000-000000000000");
    let (keyring, _release) = silent();
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::connect::connect_microsoft_within(
            &db,
            keyring,
            client,
            &account,
            30,
            mailrs_graph::GRAPH_BASE,
            SHORT,
        ),
    )
    .await
    .expect("the connect gave up by itself");
    assert!(matches!(started, Err(SyncError::NoAnswer("the keyring"))));
}

#[tokio::test]
async fn an_imap_connect_gives_up_on_a_keyring_that_never_answers() {
    let (db, _dir) = store().await;
    db.write(|c| {
        let id = accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0)?
            .expect("nobody holds the address");
        servers::save(c, id, &fastmail())
    })
    .await
    .unwrap();
    let dana = account(&db, "dana@fastmail.com").await;
    let (keyring, _release) = silent();
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::connect::connect_imap_within(&db, keyring, &dana, 30, SHORT),
    )
    .await
    .expect("the connect gave up by itself");
    assert!(matches!(started, Err(SyncError::NoAnswer("the keyring"))));
}

#[tokio::test]
async fn a_pop3_connect_gives_up_on_a_keyring_that_never_answers() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let dana = pop3_signed_in(&db, passwords, new_pop3("pw", RemoveSetting::Never), 7)
        .await
        .unwrap();
    let (keyring, _release) = silent();
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::connect::connect_pop3_within(&db, keyring, &dana, SHORT),
    )
    .await
    .expect("the connect gave up by itself");
    assert!(matches!(started, Err(SyncError::NoAnswer("the keyring"))));
}

/// Google's refresh tokens through the same silent keyring.
impl TokenStore for Silent {
    fn load(&self, _: &str) -> Result<Option<String>, mailrs_gmail::GmailError> {
        let _ = self.0.lock().unwrap().recv();
        Ok(None)
    }
    fn save(&self, _: &str, _: &str) -> Result<(), mailrs_gmail::GmailError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<(), mailrs_gmail::GmailError> {
        Ok(())
    }
}

#[tokio::test]
async fn a_gmail_connect_gives_up_on_a_keyring_that_never_answers() {
    let (db, _dir) = store().await;
    let id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let account = Account {
        id,
        email: "me@example.com".into(),
        state: AccountState::Ok,
        provider: mailrs_domain::Provider::Gmail,
        provider_name: None,
    };
    let (keyring, _release) = silent();
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::connect::connect_account_within(
            OAuthClient::new("cid", "secret"),
            keyring,
            &account,
            &db,
            SHORT,
        ),
    )
    .await
    .expect("the connect gave up by itself");
    assert!(matches!(started, Err(SyncError::NoAnswer("the keyring"))));
}

#[tokio::test]
async fn an_imap_account_without_a_password_needs_to_sign_in() {
    let (db, _dir) = store().await;
    db.write(|c| {
        let id = accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0)?
            .expect("nobody holds the address");
        servers::save(c, id, &fastmail())
    })
    .await
    .unwrap();
    let dana = account(&db, "dana@fastmail.com").await;
    let started = connect_imap(&db, Arc::new(MemoryPasswords::default()), &dana, 30).await;
    assert!(matches!(
        started,
        Err(SyncError::Backend(BackendError::NeedsReauth))
    ));
    assert_eq!(
        account(&db, "dana@fastmail.com").await.state,
        AccountState::NeedsReauth
    );
}

/// A "Set up manually" account saved before it consulted the table keeps
/// its address's domain as `provider_name`. Sync must still find Yahoo's
/// own sent-copy rule from there, or a message filed by Yahoo gets a
/// second copy from the app.
#[tokio::test]
async fn a_saved_account_under_its_domain_still_gets_its_real_sent_copy_rule() {
    let (db, _dir) = store().await;
    let yahoo = Servers {
        imap: Saved {
            host: "imap.mail.yahoo.com".into(),
            port: 993,
            security: Security::Tls,
            user_name: "dana@yahoo.com".into(),
        },
        smtp: Saved {
            host: "smtp.mail.yahoo.com".into(),
            port: 465,
            security: Security::Tls,
            user_name: "dana@yahoo.com".into(),
        },
    };
    db.write(move |c| {
        let id = accounts::insert_imap_account(c, "dana@yahoo.com", "yahoo.com", 0)?
            .expect("nobody holds the address");
        servers::save(c, id, &yahoo)
    })
    .await
    .unwrap();
    let dana = account(&db, "dana@yahoo.com").await;
    let passwords = Arc::new(MemoryPasswords::default());
    passwords.save(dana.id, "secret").unwrap();
    let services = connect_imap(&db, passwords, &dana, 30).await.unwrap();
    assert!(
        services.mail.capabilities().files_sent_mail,
        "Yahoo files its own Sent copy, so the app must not file a second one"
    );
}

#[tokio::test]
async fn an_imap_account_without_servers_needs_to_sign_in() {
    let (db, _dir) = store().await;
    db.write(|c| accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0))
        .await
        .unwrap();
    let dana = account(&db, "dana@fastmail.com").await;
    let passwords = Arc::new(MemoryPasswords::default());
    passwords.save(dana.id, "secret").unwrap();
    let started = connect_imap(&db, passwords, &dana, 30).await;
    assert!(matches!(
        started,
        Err(SyncError::Backend(BackendError::NeedsReauth))
    ));
}

#[test]
fn servers_kept_in_the_store_come_back_as_the_same_servers() {
    let kept = icloud();
    let (imap, smtp) = (server_of(&kept.imap), server_of(&kept.smtp));
    assert_eq!(servers_for(&imap, "dana", &smtp, "dana@icloud.com"), kept);
}

#[tokio::test]
async fn each_server_keeps_the_user_name_it_took() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let new = NewImap {
        address: "dana@icloud.com".into(),
        provider_name: "iCloud Mail".into(),
        servers: icloud(),
        password: "pw".into(),
        smtp_password: None,
    };
    let dana = imap_signed_in(&db, passwords, new, 7).await.unwrap();
    let id = dana.id;
    let kept = db
        .read(move |c| servers::load(c, id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.imap.user_name, "dana");
    assert_eq!(kept.smtp.user_name, "dana@icloud.com");
}

#[tokio::test]
async fn signing_in_keeps_the_account_its_servers_and_its_password() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    // App passwords are often shown in groups with spaces; the spaces are
    // part of what the person typed and go to the keyring as they are.
    let dana = imap_signed_in(&db, Arc::clone(&passwords), new_fastmail(" abcd efgh "), 7)
        .await
        .unwrap();
    assert_eq!(dana.provider, mailrs_domain::Provider::Imap);
    assert_eq!(dana.provider_name(), "Fastmail");
    let id = dana.id;
    assert_eq!(
        db.read(move |c| servers::load(c, id)).await.unwrap(),
        Some(fastmail())
    );
    assert_eq!(passwords.load(id).unwrap().as_deref(), Some(" abcd efgh "));
}

#[tokio::test]
async fn an_address_a_gmail_account_holds_is_refused() {
    let (db, _dir) = store().await;
    db.write(|c| accounts::insert_account(c, "dana@fastmail.com", 0))
        .await
        .unwrap();
    let passwords = Arc::new(MemoryPasswords::default());
    let refused = imap_signed_in(&db, Arc::clone(&passwords), new_fastmail("pw"), 7).await;
    assert!(matches!(refused, Err(ImapSignInError::Taken { .. })));
    let kept = account(&db, "dana@fastmail.com").await;
    assert_eq!(kept.provider, mailrs_domain::Provider::Gmail);
    assert_eq!(passwords.load(kept.id).unwrap(), None);
}

#[tokio::test]
async fn a_keyring_that_refuses_leaves_no_new_account_behind() {
    let (db, _dir) = store().await;
    let refused = imap_signed_in(&db, Arc::new(Refusing), new_fastmail("pw"), 7).await;
    assert!(matches!(refused, Err(ImapSignInError::Password(_))));
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_keyring_that_refuses_leaves_an_account_signing_in_again_as_it_was() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let first = imap_signed_in(&db, passwords, new_fastmail("old"), 7)
        .await
        .unwrap();
    let id = first.id;
    db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
        .await
        .unwrap();
    let moved = NewImap {
        provider_name: "iCloud Mail".into(),
        servers: icloud(),
        ..new_fastmail("new")
    };
    let refused = imap_signed_in(&db, Arc::new(Refusing), moved, 8).await;
    assert!(matches!(refused, Err(ImapSignInError::Password(_))));
    let kept = account(&db, "dana@fastmail.com").await;
    assert_eq!(kept.state, AccountState::NeedsReauth);
    assert_eq!(kept.provider_name(), "Fastmail");
    assert_eq!(
        db.read(move |c| servers::load(c, id)).await.unwrap(),
        Some(fastmail())
    );
}

#[tokio::test]
async fn signing_in_again_keeps_the_account_and_ends_needs_sign_in() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let first = imap_signed_in(&db, Arc::clone(&passwords), new_fastmail("old"), 7)
        .await
        .unwrap();
    let id = first.id;
    db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
        .await
        .unwrap();
    let again = imap_signed_in(&db, Arc::clone(&passwords), new_fastmail("new"), 8)
        .await
        .unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.state, AccountState::Ok);
    assert_eq!(passwords.load(id).unwrap().as_deref(), Some("new"));
}

#[tokio::test]
async fn a_rotated_token_goes_to_the_keyring() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "a", "expires_in": 3600, "refresh_token": "refresh-2",
            "scope": "https://graph.microsoft.com/Mail.ReadWrite",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"mail": "dana@outlook.com"})),
        )
        .mount(&server)
        .await;
    let (db, _dir) = store().await;
    let tokens = Arc::new(MemoryPasswords::default());
    let account = crate::sign_in::microsoft_signed_in(
        &db,
        Arc::clone(&tokens),
        crate::sign_in::NewMicrosoft {
            address: "dana@outlook.com".into(),
            provider_name: "Outlook".into(),
            refresh_token: "refresh-1".into(),
            granted: None,
        },
        0,
        "",
    )
    .await
    .unwrap();
    let client = mailrs_graph::MicrosoftClient::new("00000000-0000-0000-0000-000000000000")
        .with_endpoints(
            format!("{}/authorize", server.uri()),
            format!("{}/token", server.uri()),
        );
    let services = crate::connect_microsoft_at(
        &db,
        Arc::clone(&tokens),
        client,
        &account,
        30,
        &format!("{}/v1.0/", server.uri()),
    )
    .await
    .unwrap();
    let crate::AnyIdentities::Microsoft(adapter) = &services.identities else {
        panic!("a Microsoft account")
    };
    adapter.probe().await.unwrap();
    // The save runs off the runtime; give it a moment.
    for _ in 0..50 {
        if tokens.load(account.id).unwrap().as_deref() == Some("refresh-2") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the rotated token never reached the keyring");
}

#[tokio::test]
async fn a_microsoft_account_with_no_token_needs_to_sign_in() {
    let (db, _dir) = store().await;
    let id = db
        .write(|c| accounts::insert_microsoft_account(c, "dana@outlook.com", "Outlook", 0))
        .await
        .unwrap()
        .unwrap();
    let account = account(&db, "dana@outlook.com").await;
    let client = mailrs_graph::MicrosoftClient::new("00000000-0000-0000-0000-000000000000");
    let refused = crate::connect_microsoft(
        &db,
        Arc::new(MemoryPasswords::default()),
        client,
        &account,
        30,
    )
    .await;
    assert!(matches!(
        refused,
        Err(SyncError::Backend(BackendError::NeedsReauth))
    ));
    let state = db
        .read(move |c| accounts::account(c, id))
        .await
        .unwrap()
        .unwrap()
        .state;
    assert_eq!(state, AccountState::NeedsReauth);
}

fn new_pop3(password: &str, remove: RemoveSetting) -> NewPop3 {
    let saved = |host: &str, port| Saved {
        host: host.into(),
        port,
        security: Security::Tls,
        user_name: "dana@example.org".into(),
    };
    NewPop3 {
        address: "dana@example.org".into(),
        provider_name: "example.org".into(),
        servers: servers::Pop3Servers {
            pop3: saved("pop.example.org", 995),
            smtp: saved("smtp.example.org", 465),
        },
        remove,
        password: password.into(),
        smtp_password: None,
    }
}

#[tokio::test]
async fn a_pop3_sign_in_keeps_the_account_its_servers_its_setting_and_its_password() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let dana = pop3_signed_in(
        &db,
        Arc::clone(&passwords),
        new_pop3("pw", RemoveSetting::Days(14)),
        7,
    )
    .await
    .unwrap();
    assert_eq!(dana.provider, mailrs_domain::Provider::Pop3);
    let id = dana.id;
    let (kept, remove) = db
        .read(move |c| Ok((servers::load_pop3(c, id)?, accounts::pop3_remove(c, id)?)))
        .await
        .unwrap();
    assert_eq!(
        kept.map(|k| k.pop3.host),
        Some("pop.example.org".to_string())
    );
    assert_eq!(remove, RemoveSetting::Days(14));
    assert_eq!(passwords.load(id).unwrap().as_deref(), Some("pw"));
}

#[tokio::test]
async fn signing_a_pop3_account_in_again_takes_the_new_setting_and_ends_needs_sign_in() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let first = pop3_signed_in(
        &db,
        Arc::clone(&passwords),
        new_pop3("old", RemoveSetting::Never),
        7,
    )
    .await
    .unwrap();
    let id = first.id;
    db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
        .await
        .unwrap();
    let again = pop3_signed_in(
        &db,
        Arc::clone(&passwords),
        new_pop3("new", RemoveSetting::Downloaded),
        8,
    )
    .await
    .unwrap();
    assert_eq!(again.id, id);
    assert_eq!(again.state, AccountState::Ok);
    assert_eq!(
        db.read(move |c| accounts::pop3_remove(c, id))
            .await
            .unwrap(),
        RemoveSetting::Downloaded
    );
    assert_eq!(passwords.load(id).unwrap().as_deref(), Some("new"));
}

#[tokio::test]
async fn a_pop3_sign_in_for_an_address_an_imap_account_holds_is_refused() {
    let (db, _dir) = store().await;
    db.write(|c| accounts::insert_imap_account(c, "dana@example.org", "example.org", 0))
        .await
        .unwrap();
    let passwords = Arc::new(MemoryPasswords::default());
    let refused = pop3_signed_in(
        &db,
        Arc::clone(&passwords),
        new_pop3("pw", RemoveSetting::Never),
        7,
    )
    .await;
    assert!(matches!(refused, Err(ImapSignInError::Taken { .. })));
    assert_eq!(
        passwords.load(1).unwrap(),
        None,
        "a refused sign-in keeps no password"
    );
}

#[test]
fn a_pop3_failure_reads_as_the_incoming_servers() {
    use mailrs_imap::ImapError;
    use mailrs_pop3::Pop3Error;
    assert_eq!(
        in_imap_words(Pop3Error::Unsupported("UIDL")),
        ImapError::Refused(
            "This server cannot tell its messages apart, so Penguin Mail cannot download from it safely.".into()
        )
    );
    assert_eq!(
        in_imap_words(Pop3Error::Auth { text: "no".into() }),
        ImapError::Auth { text: "no".into() }
    );
    assert_eq!(
        in_imap_words(Pop3Error::Network("gone".into())),
        ImapError::Network("gone".into())
    );
    assert!(matches!(
        in_imap_words(Pop3Error::InUse("[IN-USE]".into())),
        ImapError::TooManyConnections { .. }
    ));
}

const CLOUD: &str = "https://cloud.example.org/remote.php/dav/";
const ROBIN: &str = "robin@cloud.example.org";

/// A CalDAV server on cloud.example.org that takes the password
/// `app-password` and records each address it is tried at. Any other host
/// answers as no calendar server.
#[derive(Default)]
struct Cloud(std::sync::Mutex<Vec<String>>);

impl ServiceProbe for Cloud {
    async fn dav(
        &self,
        url: &str,
        _user: &str,
        password: &str,
        _kind: Kind,
    ) -> Result<(), BackendError> {
        self.0.lock().unwrap().push(url.to_string());
        match (url.starts_with("https://cloud.example.org/"), password) {
            (false, _) => Err(BackendError::Unsupported),
            (true, "app-password") => Ok(()),
            (true, _) => Err(BackendError::NeedsReauth),
        }
    }

    async fn sieve(&self, _: &str, _: u16, _: &str, _: &str) -> Result<Extensions, BackendError> {
        Err(BackendError::Unsupported)
    }
}

/// A calendar sign-in at `url` as `user`, with the password the cloud
/// takes.
fn at(url: &str, user: &str) -> NewCalDav {
    NewCalDav {
        url: url.into(),
        user_name: user.into(),
        password: "app-password".into(),
    }
}

/// Signs in to the cloud with a keyring nobody reads afterwards.
async fn calendar_sign_in(db: &Db, new: NewCalDav) -> Result<Account, CalDavSignInError> {
    caldav_signed_in(
        &Cloud::default(),
        db,
        Arc::new(MemoryPasswords::default()),
        new,
        7,
    )
    .await
}

#[test]
fn a_calendar_account_is_named_by_its_user_and_its_host() {
    assert_eq!(
        calendar_account_name(CLOUD, "robin").as_deref(),
        Some(ROBIN)
    );
    assert_eq!(
        calendar_account_name("https://Cloud.Example.org:8443/dav", " robin ").as_deref(),
        Some(ROBIN),
        "the host alone, in lower case, and the user name without its spaces"
    );
    assert_eq!(
        calendar_account_name(CLOUD, "Robin").as_deref(),
        Some(ROBIN),
        "the user name in lower case too"
    );
    assert_eq!(
        calendar_account_name(CLOUD, "me@example.org").as_deref(),
        Some("me@example.org@cloud.example.org"),
        "a user name that is an address keeps the host, so it never names a mail account"
    );
    assert_eq!(calendar_account_name("not an address", "robin"), None);
    assert_eq!(calendar_account_name(CLOUD, "  "), None);
}

#[tokio::test]
async fn a_calendar_sign_in_keeps_the_account_its_server_and_its_password() {
    // The address as the server gives it, then a bare host with spaces
    // around it and around the user name.
    for (typed, user, kept_as) in [
        (CLOUD, "robin", CLOUD),
        (
            "  cloud.example.org ",
            " robin ",
            "https://cloud.example.org/",
        ),
    ] {
        let (db, _dir) = store().await;
        let passwords = Arc::new(MemoryPasswords::default());
        let account = caldav_signed_in(
            &Cloud::default(),
            &db,
            Arc::clone(&passwords),
            at(typed, user),
            7,
        )
        .await
        .unwrap();
        assert_eq!(
            (account.email.as_str(), account.provider),
            (ROBIN, Provider::CalDav)
        );
        let id = account.id;
        let server = FoundService {
            kind: ServiceKind::CalDav,
            url: kept_as.into(),
            user_name: "robin".into(),
            // The person typed the address, so the password may go to it.
            confirmed: true,
            source: "typed".into(),
        };
        assert_eq!(
            db.read(move |c| services::load(c, id)).await.unwrap(),
            [server]
        );
        assert_eq!(passwords.load(id).unwrap().as_deref(), Some("app-password"));
    }
}

/// What the cloud refuses `new` with, once nothing was kept of it.
async fn refused_and_kept_nothing(new: NewCalDav) -> BackendError {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let refused = caldav_signed_in(&Cloud::default(), &db, Arc::clone(&passwords), new, 7).await;
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
    assert_eq!(passwords.load(1).unwrap(), None);
    match refused {
        Err(CalDavSignInError::Server(SyncError::Backend(why))) => why,
        other => panic!(
            "the server did not refuse it: {:?}",
            other.map(|account| account.email)
        ),
    }
}

#[tokio::test]
async fn a_calendar_sign_in_the_server_does_not_take_keeps_nothing() {
    let wrong_password = NewCalDav {
        password: "wrong".into(),
        ..at(CLOUD, "robin")
    };
    assert!(matches!(
        refused_and_kept_nothing(wrong_password).await,
        BackendError::NeedsReauth
    ));
    let no_calendar_there = at("https://example.org/", "robin");
    assert!(matches!(
        refused_and_kept_nothing(no_calendar_there).await,
        BackendError::Unsupported
    ));
}

#[tokio::test]
async fn any_scheme_but_https_is_refused_and_nothing_is_tried() {
    let (db, _dir) = store().await;
    let cloud = Cloud::default();
    for address in [
        "http://cloud.example.org/",
        "ftp://cloud.example.org/",
        "webcal://cloud.example.org/home.ics",
    ] {
        let refused = caldav_signed_in(
            &cloud,
            &db,
            Arc::new(MemoryPasswords::default()),
            at(address, "robin"),
            7,
        )
        .await;
        assert!(
            matches!(refused, Err(CalDavSignInError::NotHttps)),
            "{address}"
        );
    }
    assert!(
        cloud.0.lock().unwrap().is_empty(),
        "the password went nowhere"
    );
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_user_name_that_is_an_address_does_not_take_a_mail_accounts_name() {
    let (db, _dir) = store().await;
    db.write(|c| accounts::insert_imap_account(c, "me@example.org", "example.org", 1))
        .await
        .unwrap();
    let account = calendar_sign_in(&db, at(CLOUD, "me@example.org"))
        .await
        .unwrap();
    assert_eq!(account.email, "me@example.org@cloud.example.org");
    assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_calendar_name_another_kind_of_account_holds_is_refused() {
    let (db, _dir) = store().await;
    db.write(|c| accounts::insert_imap_account(c, ROBIN, "example.org", 1))
        .await
        .unwrap();
    let refused = calendar_sign_in(&db, at(CLOUD, "robin")).await;
    // The page words its way out by who holds the name.
    assert!(
        matches!(refused, Err(CalDavSignInError::Taken { name, holder: Provider::Imap, .. }) if name == ROBIN)
    );
}

#[tokio::test]
async fn signing_a_calendar_account_in_again_keeps_its_calendars_and_ends_needs_sign_in() {
    let (db, _dir) = store().await;
    let id = calendar_sign_in(&db, at(CLOUD, "robin")).await.unwrap().id;
    db.write(move |c| {
        let personal = Calendar {
            id: "personal".into(),
            name: "Personal".into(),
            ..Default::default()
        };
        store_calendar::save_calendars(c, id, &[personal])?;
        accounts::set_state(c, id, AccountState::NeedsReauth)
    })
    .await
    .unwrap();
    let passwords = Arc::new(MemoryPasswords::default());
    let again = caldav_signed_in_again(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        id,
        at(CLOUD, "robin"),
    )
    .await
    .unwrap();
    assert_eq!(
        (again.id, again.state),
        (id, AccountState::Ok),
        "one account, not two"
    );
    assert_eq!(
        passwords.load(id).unwrap().as_deref(),
        Some("app-password"),
        "the keyring has the new password"
    );
    assert_eq!(
        db.read(move |c| store_calendar::calendars(c, id))
            .await
            .unwrap()
            .len(),
        1,
        "what the copy held is still there"
    );
}

#[tokio::test]
async fn adding_the_same_server_again_as_the_same_user_signs_its_account_in_again() {
    let (db, _dir) = store().await;
    let first = calendar_sign_in(&db, at(CLOUD, "Robin")).await.unwrap();
    assert_eq!(first.email, ROBIN);
    let again = calendar_sign_in(&db, at("cloud.example.org/remote.php/dav", " Robin "))
        .await
        .unwrap();
    assert_eq!(
        again.id, first.id,
        "one account syncing the calendars, not two"
    );
    assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_user_name_that_differs_only_in_case_never_takes_the_account() {
    // Radicale, for one, tells Robin from robin: two logins, one name.
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let id = caldav_signed_in(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        at(CLOUD, "Robin"),
        7,
    )
    .await
    .unwrap()
    .id;
    let kept = db.read(move |c| services::load(c, id)).await.unwrap();
    let cloud = Cloud::default();
    let new = NewCalDav {
        password: "other-password".into(),
        ..at(CLOUD, "robin")
    };
    let refused = caldav_signed_in(&cloud, &db, Arc::clone(&passwords), new, 8).await;
    assert!(
        matches!(&refused, Err(CalDavSignInError::OtherUser { name, user }) if name == ROBIN && user == "Robin"),
        "{:?}",
        refused.map(|account| account.email)
    );
    assert!(
        cloud.0.lock().unwrap().is_empty(),
        "the password went nowhere"
    );
    assert_eq!(
        db.read(move |c| services::load(c, id)).await.unwrap(),
        kept,
        "the account keeps its user"
    );
    assert_eq!(
        passwords.load(id).unwrap().as_deref(),
        Some("app-password"),
        "and its password"
    );
}

#[tokio::test]
async fn a_second_server_on_the_same_host_does_not_take_the_first_ones_place() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let first = caldav_signed_in(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        at(CLOUD, "robin"),
        7,
    )
    .await
    .unwrap();
    let id = first.id;
    let kept = db.read(move |c| services::load(c, id)).await.unwrap();
    for elsewhere in [
        "https://cloud.example.org/radicale/",
        "https://cloud.example.org:5232/",
    ] {
        let cloud = Cloud::default();
        let new = NewCalDav {
            password: "other-password".into(),
            ..at(elsewhere, "robin")
        };
        let refused = caldav_signed_in(&cloud, &db, Arc::clone(&passwords), new, 8).await;
        assert!(
            matches!(&refused, Err(CalDavSignInError::Elsewhere { name, address }) if name == ROBIN && address == CLOUD),
            "{elsewhere}: {:?}",
            refused.map(|account| account.email)
        );
        assert!(
            cloud.0.lock().unwrap().is_empty(),
            "{elsewhere}: the password went nowhere"
        );
    }
    assert_eq!(
        db.read(move |c| services::load(c, id)).await.unwrap(),
        kept,
        "the first account keeps its server"
    );
    assert_eq!(
        passwords.load(id).unwrap().as_deref(),
        Some("app-password"),
        "and its password"
    );
    assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 1);
}

#[tokio::test]
async fn signing_in_again_never_moves_the_account_to_another_server_or_user() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let id = caldav_signed_in(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        at(CLOUD, "robin"),
        7,
    )
    .await
    .unwrap()
    .id;
    let kept = db.read(move |c| services::load(c, id)).await.unwrap();
    // The page greys out the address and the user once it has read them,
    // and leaves them open when it could not.
    for (url, user) in [
        ("https://cloud.example.org/radicale/", "robin"),
        ("https://cloud.example.org:5232/", "robin"),
        (CLOUD, "dana"),
        (CLOUD, "Robin"),
    ] {
        let cloud = Cloud::default();
        let new = NewCalDav {
            password: "other-password".into(),
            ..at(url, user)
        };
        let refused = caldav_signed_in_again(&cloud, &db, Arc::clone(&passwords), id, new).await;
        assert!(
            matches!(&refused, Err(CalDavSignInError::Moves { name, address }) if name == ROBIN && address == CLOUD),
            "{url} as {user}: {:?}",
            refused.map(|account| account.email)
        );
        assert!(
            cloud.0.lock().unwrap().is_empty(),
            "{url} as {user}: the password went nowhere"
        );
    }
    assert_eq!(
        db.read(move |c| services::load(c, id)).await.unwrap(),
        kept,
        "the account keeps its server"
    );
    assert_eq!(
        passwords.load(id).unwrap().as_deref(),
        Some("app-password"),
        "and its password"
    );

    let typed_otherwise = at("cloud.example.org/remote.php/dav", " robin ");
    let again = caldav_signed_in_again(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        id,
        typed_otherwise,
    )
    .await
    .unwrap();
    assert_eq!(
        again.id, id,
        "the same server and user, typed another way, sign it in again"
    );
}

#[tokio::test]
async fn a_calendar_account_that_lost_its_server_is_named_by_its_host() {
    let (db, _dir) = store().await;
    let id = db
        .write(|c| accounts::insert_caldav_account(c, ROBIN, 1))
        .await
        .unwrap()
        .unwrap();
    let refused = calendar_sign_in(&db, at(CLOUD, "robin"))
        .await
        .map(|account| account.email);
    assert!(
        matches!(&refused, Err(CalDavSignInError::Elsewhere { address, .. }) if address == "cloud.example.org"),
        "{refused:?}"
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("for the server at cloud.example.org.")
    );

    // Nothing says which login its calendars and queued changes belong
    // to, so its own Sign In Again cannot give them to one either.
    let cloud = Cloud::default();
    let refused = caldav_signed_in_again(
        &cloud,
        &db,
        Arc::new(MemoryPasswords::default()),
        id,
        at(CLOUD, "robin"),
    )
    .await;
    assert!(
        matches!(&refused, Err(CalDavSignInError::Moves { address, .. }) if address == "cloud.example.org"),
        "{:?}",
        refused.map(|account| account.email)
    );
    assert!(
        cloud.0.lock().unwrap().is_empty(),
        "the password went nowhere"
    );
    assert!(
        db.read(move |c| services::load(c, id))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn signing_in_again_never_puts_a_calendar_password_on_a_mail_account() {
    let (db, _dir) = store().await;
    let mail = db
        .write(|c| accounts::insert_imap_account(c, "me@example.org", "example.org", 1))
        .await
        .unwrap()
        .unwrap();
    let passwords = Arc::new(MemoryPasswords::default());
    let refused = caldav_signed_in_again(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        mail,
        at(CLOUD, "robin"),
    )
    .await;
    assert!(matches!(
        refused,
        Err(CalDavSignInError::Taken {
            holder: Provider::Imap,
            ..
        })
    ));
    assert_eq!(passwords.load(mail).unwrap(), None);
}

#[tokio::test]
async fn a_calendar_connect_gives_up_on_a_keyring_that_never_answers() {
    let (db, _dir) = store().await;
    let account = calendar_sign_in(&db, at(CLOUD, "robin")).await.unwrap();
    let (keyring, _release) = silent();
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::connect::connect_caldav_within(&db, keyring, &account, SHORT),
    )
    .await
    .expect("the connect gave up by itself");
    assert!(matches!(started, Err(SyncError::NoAnswer("the keyring"))));
}

#[tokio::test]
async fn a_keyring_that_refuses_leaves_no_new_calendar_account_behind() {
    let (db, _dir) = store().await;
    let refused = caldav_signed_in(
        &Cloud::default(),
        &db,
        Arc::new(Refusing),
        at(CLOUD, "robin"),
        7,
    )
    .await;
    assert!(matches!(refused, Err(CalDavSignInError::Password(_))));
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
}

type Change = Box<dyn FnOnce(&rusqlite::Connection) -> mailrs_store::Result<()> + Send>;

/// The cloud, with a change made to the store while a sign-in asks it:
/// another sign-in getting there first, or the account going.
struct Meanwhile {
    db: Db,
    change: std::sync::Mutex<Option<Change>>,
}

fn meanwhile(
    db: &Db,
    change: impl FnOnce(&rusqlite::Connection) -> mailrs_store::Result<()> + Send + 'static,
) -> Meanwhile {
    Meanwhile {
        db: db.clone(),
        change: std::sync::Mutex::new(Some(Box::new(change))),
    }
}

impl ServiceProbe for Meanwhile {
    async fn dav(
        &self,
        url: &str,
        user: &str,
        password: &str,
        kind: Kind,
    ) -> Result<(), BackendError> {
        let change = self.change.lock().unwrap().take();
        if let Some(change) = change {
            self.db.write(change).await.unwrap();
        }
        Cloud::default().dav(url, user, password, kind).await
    }

    async fn sieve(&self, _: &str, _: u16, _: &str, _: &str) -> Result<Extensions, BackendError> {
        Err(BackendError::Unsupported)
    }
}

/// Keeps robin@cloud.example.org as a sign-in that finished first would:
/// at `url` as `user`, with a change to an event waiting to go.
fn added_first(c: &rusqlite::Connection, url: &str, user: &str) -> mailrs_store::Result<()> {
    let id = accounts::insert_caldav_account(c, ROBIN, 1)?.expect("nobody holds the name yet");
    let server = FoundService {
        kind: ServiceKind::CalDav,
        url: url.into(),
        user_name: user.into(),
        confirmed: true,
        source: "typed".into(),
    };
    services::save(c, id, &server)?;
    let walk = mailrs_domain::calendar::Event {
        calendar: "personal".into(),
        id: "walk".into(),
        ..Default::default()
    };
    store_calendar::enqueue(c, id, store_calendar::ChangeKind::Save, &walk)
}

#[tokio::test]
async fn a_sign_in_never_deletes_an_account_that_arrived_while_it_asked_the_server() {
    let (db, _dir) = store().await;
    let cloud = meanwhile(&db, |c| added_first(c, CLOUD, "robin"));
    // The keyring refuses the later sign-in's password.
    let refused = caldav_signed_in(&cloud, &db, Arc::new(Refusing), at(CLOUD, "robin"), 8).await;
    assert!(
        matches!(refused, Err(CalDavSignInError::Password(_))),
        "{:?}",
        refused.map(|account| account.email)
    );
    let id = db
        .read(|c| accounts::account_by_email(c, ROBIN))
        .await
        .unwrap()
        .expect("the first account stays")
        .id;
    let queued = db
        .read(move |c| store_calendar::queued(c, id))
        .await
        .unwrap();
    assert_eq!(queued.len(), 1, "with the change it had waiting");
}

#[tokio::test]
async fn a_sign_in_never_moves_an_account_that_arrived_while_it_asked_the_server() {
    for (url, user) in [
        ("https://cloud.example.org:5232/", "robin"),
        (CLOUD, "Robin"),
    ] {
        let (db, _dir) = store().await;
        let cloud = meanwhile(&db, move |c| added_first(c, url, user));
        let passwords = Arc::new(MemoryPasswords::default());
        let refused =
            caldav_signed_in(&cloud, &db, Arc::clone(&passwords), at(CLOUD, "robin"), 8).await;
        assert!(
            matches!(&refused, Err(CalDavSignInError::Elsewhere { name, .. } | CalDavSignInError::OtherUser { name, .. }) if name == ROBIN),
            "{url} as {user}: {:?}",
            refused.map(|account| account.email)
        );
        let id = db
            .read(|c| accounts::account_by_email(c, ROBIN))
            .await
            .unwrap()
            .unwrap()
            .id;
        let kept = db.read(move |c| services::load(c, id)).await.unwrap();
        assert_eq!(
            (kept[0].url.as_str(), kept[0].user_name.as_str()),
            (url, user),
            "it keeps its server and its user"
        );
        assert_eq!(passwords.load(id).unwrap(), None, "and gets no password");
    }
}

#[tokio::test]
async fn the_same_login_arriving_while_the_server_is_asked_is_signed_in_again() {
    let (db, _dir) = store().await;
    let cloud = meanwhile(&db, |c| added_first(c, CLOUD, "robin"));
    let passwords = Arc::new(MemoryPasswords::default());
    let account = caldav_signed_in(&cloud, &db, Arc::clone(&passwords), at(CLOUD, "robin"), 8)
        .await
        .unwrap();
    assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 1);
    assert_eq!(
        passwords.load(account.id).unwrap().as_deref(),
        Some("app-password")
    );
}

#[tokio::test]
async fn signing_in_again_stops_when_the_account_goes_while_the_server_is_asked() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let id = caldav_signed_in(
        &Cloud::default(),
        &db,
        Arc::clone(&passwords),
        at(CLOUD, "robin"),
        7,
    )
    .await
    .unwrap()
    .id;
    // The account is removed, and a mail account added in its place takes
    // its id, as SQLite gives out the highest one again.
    let theirs = Arc::clone(&passwords);
    let cloud = meanwhile(&db, move |c| {
        accounts::delete_account(c, id)?;
        accounts::insert_imap_account(c, "dana@example.org", "example.org", 9)?;
        theirs.save(id, "dana's password").unwrap();
        Ok(())
    });
    let refused =
        caldav_signed_in_again(&cloud, &db, Arc::clone(&passwords), id, at(CLOUD, "robin")).await;
    assert!(
        refused.is_err(),
        "{:?}",
        refused.map(|account| account.email)
    );
    let dana = db
        .read(|c| accounts::account_by_email(c, "dana@example.org"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dana.id, id,
        "the mail account has the calendar account's id"
    );
    assert_eq!(
        passwords.load(id).unwrap().as_deref(),
        Some("dana's password"),
        "and keeps its own password"
    );
    assert!(
        db.read(move |c| services::load(c, id))
            .await
            .unwrap()
            .is_empty(),
        "and gets no calendar server"
    );
}

/// A keyring whose save of `paused` waits until the test lets it go, as
/// one waiting on an unlock prompt would. `entered` hears that it waits.
struct Paused {
    kept: MemoryPasswords,
    paused: &'static str,
    entered: tokio::sync::Notify,
    go: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PasswordStore for Paused {
    fn load(&self, id: AccountId) -> Result<Option<String>, PasswordError> {
        self.kept.load(id)
    }
    fn save(&self, id: AccountId, password: &str) -> Result<(), PasswordError> {
        if password == self.paused {
            self.entered.notify_one();
            let _ = self.go.lock().unwrap().recv();
        }
        self.kept.save(id, password)
    }
    fn delete(&self, id: AccountId) -> Result<(), PasswordError> {
        self.kept.delete(id)
    }
    fn load_outgoing(&self, id: AccountId) -> Result<Option<String>, PasswordError> {
        self.kept.load_outgoing(id)
    }
    fn save_outgoing(&self, id: AccountId, password: &str) -> Result<(), PasswordError> {
        self.kept.save_outgoing(id, password)
    }
    fn delete_outgoing(&self, id: AccountId) -> Result<(), PasswordError> {
        self.kept.delete_outgoing(id)
    }
}

#[tokio::test]
async fn removing_an_account_waits_for_its_password_to_be_written() {
    let (db, _dir) = store().await;
    let id = calendar_sign_in(&db, at(CLOUD, "robin")).await.unwrap().id;
    let (go, wait) = std::sync::mpsc::channel();
    let keyring = Arc::new(Paused {
        kept: MemoryPasswords::default(),
        paused: "app-password",
        entered: tokio::sync::Notify::new(),
        go: std::sync::Mutex::new(wait),
    });
    let cloud = Cloud::default();
    let again = caldav_signed_in_again(&cloud, &db, Arc::clone(&keyring), id, at(CLOUD, "robin"));
    let replaced = async {
        keyring.entered.notified().await;
        // While the keyring writes, the app removes the account and a
        // mail account is added, which SQLite gives the same id.
        let mut replace = std::pin::pin!(async {
            let _turn = crate::sign_in::secrets_turn().await;
            db.write(move |c| {
                accounts::delete_account(c, id)?;
                accounts::insert_imap_account(c, "dana@example.org", "example.org", 9).map(drop)
            })
            .await
            .unwrap();
            keyring.save(id, "dana's password").unwrap();
            keyring
                .save_outgoing(id, "dana's outgoing password")
                .unwrap();
        });
        let done = tokio::time::timeout(std::time::Duration::from_millis(200), &mut replace)
            .await
            .is_ok();
        go.send(()).unwrap();
        if !done {
            replace.await;
        }
    };
    let (again, ()) = tokio::join!(again, replaced);
    let dana = db
        .read(|c| accounts::account_by_email(c, "dana@example.org"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dana.id, id,
        "the mail account has the calendar account's id"
    );
    assert_eq!(
        keyring.load(id).unwrap().as_deref(),
        Some("dana's password")
    );
    assert_eq!(
        keyring.load_outgoing(id).unwrap().as_deref(),
        Some("dana's outgoing password")
    );
    assert!(
        again.is_ok(),
        "the account was still there when its password went in"
    );
}

/// The cloud, answering a login only once the test says so. `asked`
/// hears that a sign-in is waiting on it.
#[derive(Default)]
struct Waiting {
    asked: tokio::sync::Notify,
    answer: tokio::sync::Notify,
}

impl ServiceProbe for Waiting {
    async fn dav(
        &self,
        url: &str,
        user: &str,
        password: &str,
        kind: Kind,
    ) -> Result<(), BackendError> {
        self.asked.notify_one();
        self.answer.notified().await;
        Cloud::default().dav(url, user, password, kind).await
    }

    async fn sieve(&self, _: &str, _: u16, _: &str, _: &str) -> Result<Extensions, BackendError> {
        Err(BackendError::Unsupported)
    }
}

#[tokio::test]
async fn calendar_sign_ins_take_turns() {
    let (db, _dir) = store().await;
    let (waiting, cloud) = (Waiting::default(), Cloud::default());
    let passwords = Arc::new(MemoryPasswords::default());
    // The first one's password the keyring will refuse, so it removes
    // the account it added.
    let first = caldav_signed_in(&waiting, &db, Arc::new(Refusing), at(CLOUD, "robin"), 7);
    let second = async {
        waiting.asked.notified().await;
        let mut second = std::pin::pin!(caldav_signed_in(
            &cloud,
            &db,
            Arc::clone(&passwords),
            at(CLOUD, "robin"),
            8
        ));
        let early = tokio::time::timeout(std::time::Duration::from_millis(200), &mut second).await;
        assert!(early.is_err(), "the second waits for the first to end");
        assert!(
            cloud.0.lock().unwrap().is_empty(),
            "and asks nothing meanwhile"
        );
        waiting.answer.notify_one();
        second.await
    };
    let (first, second) = tokio::join!(first, second);
    assert!(matches!(first, Err(CalDavSignInError::Password(_))));
    let second = second.unwrap();
    let ids: Vec<_> = db
        .read(accounts::list_accounts)
        .await
        .unwrap()
        .iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(ids, [second.id]);
    assert_eq!(
        passwords.load(second.id).unwrap().as_deref(),
        Some("app-password")
    );
}

#[tokio::test]
async fn an_outgoing_password_of_its_own_goes_beside_the_incoming_one() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let new = NewImap {
        smtp_password: Some("outgoing".into()),
        ..new_fastmail("incoming")
    };
    let dana = imap_signed_in(&db, Arc::clone(&passwords), new, 7)
        .await
        .unwrap();
    assert_eq!(
        passwords.load(dana.id).unwrap().as_deref(),
        Some("incoming")
    );
    assert_eq!(
        passwords.load_outgoing(dana.id).unwrap().as_deref(),
        Some("outgoing")
    );
}

/// An empty Outgoing Password means the same as the incoming one, so a
/// sign-in without one must not leave an old outgoing password to win.
#[tokio::test]
async fn signing_in_again_without_an_outgoing_password_forgets_the_old_one() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let first = NewImap {
        smtp_password: Some("outgoing".into()),
        ..new_fastmail("incoming")
    };
    let dana = imap_signed_in(&db, Arc::clone(&passwords), first, 7)
        .await
        .unwrap();
    imap_signed_in(&db, Arc::clone(&passwords), new_fastmail("new"), 8)
        .await
        .unwrap();
    assert_eq!(passwords.load_outgoing(dana.id).unwrap(), None);
}

#[tokio::test]
async fn a_pop3_account_keeps_its_outgoing_password_too() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let new = NewPop3 {
        smtp_password: Some("outgoing".into()),
        ..new_pop3("incoming", RemoveSetting::Never)
    };
    let dana = pop3_signed_in(&db, Arc::clone(&passwords), new, 7)
        .await
        .unwrap();
    assert_eq!(
        passwords.load(dana.id).unwrap().as_deref(),
        Some("incoming")
    );
    assert_eq!(
        passwords.load_outgoing(dana.id).unwrap().as_deref(),
        Some("outgoing")
    );
}

#[test]
fn the_outgoing_server_signs_in_with_its_own_password_when_there_is_one() {
    let login = smtp_login(
        "dana@fastmail.com",
        "incoming".into(),
        Some("outgoing".into()),
    );
    assert_eq!(
        (login.user.as_str(), login.password.as_str()),
        ("dana@fastmail.com", "outgoing")
    );
    let login = smtp_login("dana@fastmail.com", "incoming".into(), None);
    assert_eq!(login.password, "incoming");
}

#[tokio::test]
async fn an_account_with_an_outgoing_password_starts() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let new = NewImap {
        smtp_password: Some("outgoing".into()),
        ..new_fastmail("incoming")
    };
    let dana = imap_signed_in(&db, Arc::clone(&passwords), new, 7)
        .await
        .unwrap();
    assert!(connect_imap(&db, passwords, &dana, 30).await.is_ok());
}
