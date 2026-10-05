use std::sync::Arc;

use mailrs_domain::{Account, AccountId, AccountState};
use mailrs_gmail::{MemoryTokenStore, OAuthClient, TokenStore};
use mailrs_store::servers::{self, Saved, Security, Servers};
use mailrs_store::{Db, accounts};

use crate::passwords::{MemoryPasswords, PasswordError, PasswordStore};
use crate::sign_in::{ImapSignInError, NewImap, NewPop3, imap_signed_in, in_imap_words, pop3_signed_in};
use mailrs_domain::RemoveSetting;
use crate::{
    BackendError, MailBackend, SyncError, connect_account, connect_imap, server_of, servers_for,
};

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
    };
    let dana = imap_signed_in(&db, passwords, new, 7).await.unwrap();
    let id = dana.id;
    let kept = db.read(move |c| servers::load(c, id)).await.unwrap().unwrap();
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"mail": "dana@outlook.com"})))
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
        .with_endpoints(format!("{}/authorize", server.uri()), format!("{}/token", server.uri()));
    let services = crate::connect_microsoft_at(&db, Arc::clone(&tokens), client, &account, 30, &format!("{}/v1.0/", server.uri()))
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
    let refused = crate::connect_microsoft(&db, Arc::new(MemoryPasswords::default()), client, &account, 30).await;
    assert!(matches!(refused, Err(SyncError::Backend(BackendError::NeedsReauth))));
    let state = db.read(move |c| accounts::account(c, id)).await.unwrap().unwrap().state;
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
    }
}

#[tokio::test]
async fn a_pop3_sign_in_keeps_the_account_its_servers_its_setting_and_its_password() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let dana = pop3_signed_in(&db, Arc::clone(&passwords), new_pop3("pw", RemoveSetting::Days(14)), 7)
        .await
        .unwrap();
    assert_eq!(dana.provider, mailrs_domain::Provider::Pop3);
    let id = dana.id;
    let (kept, remove) = db
        .read(move |c| Ok((servers::load_pop3(c, id)?, accounts::pop3_remove(c, id)?)))
        .await
        .unwrap();
    assert_eq!(kept.map(|k| k.pop3.host), Some("pop.example.org".to_string()));
    assert_eq!(remove, RemoveSetting::Days(14));
    assert_eq!(passwords.load(id).unwrap().as_deref(), Some("pw"));
}

#[tokio::test]
async fn signing_a_pop3_account_in_again_takes_the_new_setting_and_ends_needs_sign_in() {
    let (db, _dir) = store().await;
    let passwords = Arc::new(MemoryPasswords::default());
    let first = pop3_signed_in(&db, Arc::clone(&passwords), new_pop3("old", RemoveSetting::Never), 7)
        .await
        .unwrap();
    let id = first.id;
    db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
        .await
        .unwrap();
    let again = pop3_signed_in(&db, Arc::clone(&passwords), new_pop3("new", RemoveSetting::Downloaded), 8)
        .await
        .unwrap();
    assert_eq!(again.id, id);
    assert_eq!(again.state, AccountState::Ok);
    assert_eq!(
        db.read(move |c| accounts::pop3_remove(c, id)).await.unwrap(),
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
    let refused = pop3_signed_in(&db, Arc::clone(&passwords), new_pop3("pw", RemoveSetting::Never), 7).await;
    assert!(matches!(refused, Err(ImapSignInError::Taken { .. })));
    assert_eq!(passwords.load(1).unwrap(), None, "a refused sign-in keeps no password");
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
