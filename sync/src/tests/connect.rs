use std::sync::Arc;

use mailrs_domain::{Account, AccountId, AccountState};
use mailrs_gmail::{MemoryTokenStore, OAuthClient, TokenStore};
use mailrs_store::servers::{self, Saved, Security, Servers};
use mailrs_store::{Db, accounts};

use crate::passwords::{MemoryPasswords, PasswordError, PasswordStore};
use crate::sign_in::{ImapSignInError, NewImap, imap_signed_in};
use crate::{BackendError, SyncError, connect_account, connect_imap, server_of, servers_for};

#[tokio::test]
async fn connecting_needs_a_stored_refresh_token() {
    let tokens: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::default());
    let account = Account {
        id: 1,
        email: "me@example.com".into(),
        state: AccountState::Ok,
        provider: mailrs_domain::Provider::Gmail,
        provider_name: None,
    };
    let oauth = OAuthClient::new("cid", "secret");
    assert!(matches!(
        connect_account(oauth.clone(), Arc::clone(&tokens), &account).await,
        Err(SyncError::Backend(crate::BackendError::NeedsReauth))
    ));
    tokens.save("me@example.com", "rt").unwrap();
    let client = connect_account(oauth, tokens, &account).await.unwrap();
    assert_eq!(client.account_id, 1);
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
