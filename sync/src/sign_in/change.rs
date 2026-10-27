//! Writing how a Gmail account signs in. Every first sign-in, sign-in
//! again, calendar consent and Online Accounts link goes through
//! [`change_sign_in`], which keeps the store's columns, the servers row and
//! the keyring entries in step, and puts the keyring back when the store
//! refuses the change. An account never changes its mail route here: that
//! takes removing it and adding it again.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use mailrs_domain::{AccountId, AccountState, EpochMillis, GmailSignIn};
use mailrs_store::servers::{self, Servers};
use mailrs_store::{Db, StoreError, accounts};
use rusqlite::Connection;
use tokio::sync::Mutex as AsyncMutex;

use super::own_client::OwnClient;
use crate::passwords::{PasswordError, PasswordStore, Secrets};

/// The account whose sign-in changes.
pub(crate) enum Whose {
    /// An address the store does not hold yet.
    New { email: String, now: EpochMillis },
    /// The account `id`.
    Held(AccountId),
}

/// A Google refresh token a sign-in brought back. No `Debug`: the token
/// must not reach a log line.
pub(crate) struct NewToken {
    pub email: String,
    pub refresh_token: String,
    /// The own client that issued it, `None` for the built-in client.
    pub own: Option<OwnClient>,
    /// The scopes Google granted, as `Granted::to_scope` writes them.
    pub granted: Option<String>,
    /// The scopes the sign-in asked for, joined with spaces.
    pub asked: String,
}

/// An app password Gmail's servers took, and those servers. No `Debug`.
pub(crate) struct NewPassword {
    pub servers: Servers,
    pub password: String,
}

/// What the account signs in as after the change, and the secrets that
/// come with it.
pub(crate) struct Change {
    pub to: GmailSignIn,
    pub token: Option<NewToken>,
    pub password: Option<NewPassword>,
    /// The person signed the account in again, so Needs Sign-In ends.
    pub signed_in_again: bool,
}

impl Change {
    /// A sign-in that brings no secret, as a link to Online Accounts.
    pub fn to(to: GmailSignIn) -> Change {
        Change {
            to,
            token: None,
            password: None,
            signed_in_again: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChangeError {
    /// The keyring refused the Google token or client. Nothing changed.
    #[error("{0}")]
    Token(String),
    /// The keyring refused the app password. Nothing changed.
    #[error(transparent)]
    Password(PasswordError),
    /// A new address turned out to be held when the change came to write
    /// it, by the provider named. Nothing changed.
    #[error("{address} is taken by {provider}")]
    Taken { address: String, provider: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// One lock per account being changed, so two overlapping sign-ins write
/// the keyring and the store one after the other. Keyed by the id of a
/// held account or the address of a new one; an entry lives as long as a
/// change holds its lock.
static LOCKS: LazyLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> =
    LazyLock::new(Mutex::default);

fn lock_for(whose: &Whose) -> Arc<AsyncMutex<()>> {
    let key = match whose {
        Whose::New { email, .. } => format!("new:{}", email.to_lowercase()),
        Whose::Held(id) => format!("held:{id}"),
    };
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Writes how the account `whose` names signs in as `change.to`, and gives
/// back its id.
///
/// A held account's secrets go into the keyring first. Then one
/// transaction writes the sign-in, the consent and the servers row. When
/// the store refuses, the keyring gets back what it held. A new address is written first, since the keyring keeps its
/// secrets under its id, and is deleted again when the keyring refuses.
///
/// A Google token from the built-in client deletes any own client the
/// keyring keeps for the account: a token refreshes only through the
/// client that issued it.
pub(crate) async fn change_sign_in(
    db: &Db,
    secrets: &Secrets,
    whose: Whose,
    change: Change,
) -> Result<AccountId, ChangeError> {
    let Change {
        to,
        token,
        password,
        signed_in_again,
    } = change;
    let lock = lock_for(&whose);
    let _one_at_a_time = lock.lock().await;
    match whose {
        Whose::New { email, now } => {
            let stored = Stored::of(to, token.as_ref(), password.as_ref(), false);
            // The row exists before its secrets do. A crash between the two
            // would leave an account that looks signed in and holds nothing,
            // so the row starts as Needs Sign-In and turns Ok only after the
            // keyring has the secrets. Signing in again then finishes the add.
            let has_secrets = token.is_some() || password.is_some();
            // The check and the insert share the write, so an account added
            // for the address since the caller looked is refused, never
            // written over or deleted below.
            let address = email.clone();
            let id = db
                .write(move |c| {
                    if let Some(held) = accounts::account_by_email(c, &email)? {
                        return Ok(Err(held.provider_name().to_string()));
                    }
                    let id = accounts::insert_account(c, &email, now)?;
                    stored.write(c, id)?;
                    if has_secrets {
                        accounts::set_state(c, id, AccountState::NeedsReauth)?;
                    }
                    Ok(Ok(id))
                })
                .await?
                .map_err(|provider| ChangeError::Taken { address, provider })?;
            if let Err(err) = keep_secrets(secrets, id, token, password).await {
                db.write(move |c| accounts::delete_account(c, id)).await?;
                return Err(err);
            }
            if has_secrets {
                db.write(move |c| accounts::set_state(c, id, AccountState::Ok))
                    .await?;
            }
            Ok(id)
        }
        Whose::Held(id) => {
            let stored = Stored::of(to, token.as_ref(), password.as_ref(), signed_in_again);
            let earlier = keep_secrets(secrets, id, token, password).await?;
            if let Err(err) = db.write(move |c| stored.write(c, id)).await {
                earlier.put_back(secrets, id).await;
                return Err(err.into());
            }
            Ok(id)
        }
    }
}

/// The store's half of a change, owned so it can cross into the write.
struct Stored {
    to: GmailSignIn,
    consent: Option<(Option<String>, String)>,
    servers: Option<Servers>,
    signed_in_again: bool,
}

impl Stored {
    fn of(
        to: GmailSignIn,
        token: Option<&NewToken>,
        password: Option<&NewPassword>,
        signed_in_again: bool,
    ) -> Stored {
        Stored {
            consent: token.map(|token| (token.granted.clone(), token.asked.clone())),
            to,
            servers: password.map(|new| new.servers.clone()),
            signed_in_again,
        }
    }

    fn write(&self, c: &Connection, id: AccountId) -> mailrs_store::Result<()> {
        accounts::set_gmail_sign_in(c, id, &self.to)?;
        if let Some((granted, asked)) = &self.consent {
            if let Some(granted) = granted {
                accounts::set_granted(c, id, granted)?;
            }
            accounts::set_asked(c, id, asked)?;
        }
        if let Some(saved) = &self.servers {
            servers::save(c, id, saved)?;
        }
        if self.signed_in_again
            && accounts::account(c, id)?.is_some_and(|held| held.state == AccountState::NeedsReauth)
        {
            accounts::set_state(c, id, AccountState::Ok)?;
        }
        Ok(())
    }
}

/// What the keyring held before a change wrote over it, so a change the
/// store refuses can put it back. `None` where the change wrote nothing.
#[derive(Default)]
struct Earlier {
    client: Option<Option<String>>,
    token: Option<(String, Option<String>)>,
    passwords: Option<(Option<String>, Option<String>)>,
}

/// Writes the change's secrets: the own client before the token, since a
/// token kept beside the wrong client cannot refresh, then the app
/// password. When one write fails, the ones before it are put back.
async fn keep_secrets(
    secrets: &Secrets,
    id: AccountId,
    token: Option<NewToken>,
    password: Option<NewPassword>,
) -> Result<Earlier, ChangeError> {
    let mut earlier = Earlier::default();
    if let Some(token) = token {
        let clients = Arc::clone(&secrets.google_clients);
        let own = token.own.as_ref().map(OwnClient::encode);
        let client = blocking(move || {
            let before = clients.load(id)?;
            match own {
                Some(own) => clients.save(id, &own)?,
                None => clients.delete(id)?,
            }
            Ok(before)
        })
        .await
        .map_err(|err| ChangeError::Token(err.to_string()))?;
        earlier.client = Some(client);
        let tokens = Arc::clone(&secrets.google);
        let (email, refresh) = (token.email.clone(), token.refresh_token);
        let saved = tokio::task::spawn_blocking(move || {
            let before = tokens.load(&email)?;
            tokens.save(&email, &refresh).map(|()| before)
        })
        .await;
        match saved {
            Ok(Ok(before)) => earlier.token = Some((token.email, before)),
            Ok(Err(err)) => return Err(earlier.undo(secrets, id, err.to_string()).await),
            Err(err) => return Err(earlier.undo(secrets, id, err.to_string()).await),
        }
    }
    if let Some(new) = password {
        let passwords = Arc::clone(&secrets.passwords);
        let saved = blocking(move || {
            let before = (passwords.load(id)?, passwords.load_outgoing(id)?);
            passwords.save(id, &new.password)?;
            passwords.delete_outgoing(id)?;
            Ok(before)
        })
        .await;
        match saved {
            Ok(before) => earlier.passwords = Some(before),
            Err(err) => {
                earlier.put_back(secrets, id).await;
                return Err(ChangeError::Password(err));
            }
        }
    }
    Ok(earlier)
}

impl Earlier {
    /// Puts back what was written and names the write that failed.
    async fn undo(self, secrets: &Secrets, id: AccountId, reason: String) -> ChangeError {
        self.put_back(secrets, id).await;
        ChangeError::Token(reason)
    }

    /// Writes back what the keyring held. A keyring that fails says so in
    /// the log.
    async fn put_back(self, secrets: &Secrets, id: AccountId) {
        let (clients, tokens, passwords) = (
            Arc::clone(&secrets.google_clients),
            Arc::clone(&secrets.google),
            Arc::clone(&secrets.passwords),
        );
        let back = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let text = |err: &dyn std::fmt::Display| err.to_string();
            if let Some(client) = self.client {
                match client {
                    Some(client) => clients.save(id, &client),
                    None => clients.delete(id),
                }
                .map_err(|e| text(&e))?;
            }
            if let Some((email, token)) = self.token {
                match token {
                    Some(token) => tokens.save(&email, &token),
                    None => tokens.delete(&email),
                }
                .map_err(|e| text(&e))?;
            }
            if let Some((incoming, outgoing)) = self.passwords {
                match incoming {
                    Some(password) => passwords.save(id, &password),
                    None => passwords.delete(id),
                }
                .map_err(|e| text(&e))?;
                match outgoing {
                    Some(password) => passwords.save_outgoing(id, &password),
                    None => passwords.delete_outgoing(id),
                }
                .map_err(|e| text(&e))?;
            }
            Ok(())
        })
        .await;
        if !matches!(back, Ok(Ok(()))) {
            tracing::warn!(
                account = id,
                "could not put back the secrets a failed sign-in replaced"
            );
        }
    }
}

/// Runs a keyring call off the runtime.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, PasswordError> + Send + 'static,
) -> Result<T, PasswordError> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))?
}

#[cfg(test)]
mod tests {
    use mailrs_domain::SignInClient;
    use mailrs_store::drafts;
    use mailrs_store::servers::{Saved, Security};

    use super::*;
    use crate::passwords::Passwords;

    const ADDRESS: &str = "me@gmail.com";

    const APP_PASSWORD: GmailSignIn = GmailSignIn::AppPassword {
        google: SignInClient::BuiltIn,
    };

    fn servers_row() -> Servers {
        let saved = |host: &str, port| Saved {
            host: host.into(),
            port,
            security: Security::Tls,
            user_name: ADDRESS.into(),
        };
        Servers {
            imap: saved("imap.gmail.com", 993),
            smtp: saved("smtp.gmail.com", 465),
        }
    }

    fn new_password(password: &str) -> NewPassword {
        NewPassword {
            servers: servers_row(),
            password: password.into(),
        }
    }

    fn token(refresh: &str, own: Option<OwnClient>) -> NewToken {
        NewToken {
            email: ADDRESS.into(),
            refresh_token: refresh.into(),
            own,
            granted: Some("gmail.modify".into()),
            asked: "gmail.modify calendar".into(),
        }
    }

    fn own() -> OwnClient {
        OwnClient::new("own-cid", "own-secret")
    }

    /// A store holding one Gmail account that signs in as `from`, with a
    /// draft row and the sync state "777", and its servers row and app
    /// password when `from` reads with one.
    async fn account_on(from: &GmailSignIn) -> (tempfile::TempDir, Db, Secrets, AccountId) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let secrets = Secrets::memory();
        let (sign_in, password) = (from.clone(), from.on_app_password());
        let id = db
            .write(move |c| {
                let id = accounts::insert_account(c, ADDRESS, 0)?;
                accounts::set_gmail_sign_in(c, id, &sign_in)?;
                accounts::set_sync_state(c, id, "777")?;
                drafts::remember(c, id, "d1", "a")?;
                if password {
                    servers::save(c, id, &servers_row())?;
                }
                Ok(id)
            })
            .await
            .unwrap();
        if password {
            secrets.passwords.save(id, "old-password").unwrap();
        }
        (dir, db, secrets, id)
    }

    /// What the store and the keyring hold for the account after a change.
    #[derive(Debug, PartialEq)]
    struct Held {
        sign_in: GmailSignIn,
        sync_state: Option<String>,
        drafts: i64,
        servers: bool,
        password: Option<String>,
        own_client: bool,
        token: Option<String>,
    }

    async fn held(db: &Db, secrets: &Secrets, id: AccountId) -> Held {
        let (sign_in, sync_state, drafts, servers) = db
            .read(move |c| {
                Ok((
                    accounts::gmail_sign_in(c, id)?,
                    accounts::sync_cursor(c, id)?.state,
                    c.query_row("SELECT COUNT(*) FROM drafts", [], |row| row.get(0))?,
                    servers::load(c, id)?.is_some(),
                ))
            })
            .await
            .unwrap();
        Held {
            sign_in,
            sync_state,
            drafts,
            servers,
            password: secrets.passwords.load(id).unwrap(),
            own_client: secrets.google_clients.load(id).unwrap().is_some(),
            token: secrets.google.load(ADDRESS).unwrap(),
        }
    }

    async fn change(db: &Db, secrets: &Secrets, id: AccountId, change: Change) {
        change_sign_in(db, secrets, Whose::Held(id), change)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn signing_in_again_through_the_own_project_keeps_the_mail_and_takes_the_token() {
        let (_dir, db, secrets, id) = account_on(&GmailSignIn::BuiltInApi).await;
        let to = Change {
            token: Some(token("own-token", Some(own()))),
            ..Change::to(GmailSignIn::OwnProject)
        };
        change(&db, &secrets, id, to).await;
        assert_eq!(
            held(&db, &secrets, id).await,
            Held {
                sign_in: GmailSignIn::OwnProject,
                sync_state: Some("777".into()),
                drafts: 1,
                servers: false,
                password: None,
                own_client: true,
                token: Some("own-token".into()),
            }
        );
        let consent = db.read(move |c| accounts::consent(c, id)).await.unwrap();
        assert_eq!(consent.asked.as_deref(), Some("gmail.modify calendar"));
    }

    #[tokio::test]
    async fn a_token_from_the_built_in_client_takes_the_own_client_with_it() {
        let (_dir, db, secrets, id) = account_on(&GmailSignIn::OwnProject).await;
        secrets.google_clients.save(id, &own().encode()).unwrap();
        let to = Change {
            token: Some(token("built-in-token", None)),
            ..Change::to(GmailSignIn::BuiltInApi)
        };
        change(&db, &secrets, id, to).await;
        assert!(!held(&db, &secrets, id).await.own_client);
    }

    #[tokio::test]
    async fn a_store_that_refuses_the_change_gets_the_keyring_back() {
        let (_dir, db, secrets, id) = account_on(&GmailSignIn::OwnProject).await;
        secrets.google_clients.save(id, &own().encode()).unwrap();
        secrets.google.save(ADDRESS, "own-token").unwrap();
        let before = held(&db, &secrets, id).await;
        // Saving the servers row fails without its table, so the one
        // transaction of the change fails.
        db.write(|c| Ok(c.execute_batch("DROP TABLE account_servers")?))
            .await
            .unwrap();
        let to = Change {
            password: Some(new_password("new-password")),
            ..Change::to(APP_PASSWORD)
        };

        let refused = change_sign_in(&db, &secrets, Whose::Held(id), to).await;

        assert!(matches!(refused, Err(ChangeError::Store(_))), "{refused:?}");
        let sign_in = db
            .read(move |c| accounts::gmail_sign_in(c, id))
            .await
            .unwrap();
        assert_eq!(sign_in, before.sign_in);
        assert_eq!(secrets.passwords.load(id).unwrap(), None);
        assert!(secrets.google_clients.load(id).unwrap().is_some());
    }

    #[tokio::test]
    async fn a_keyring_that_refuses_the_token_changes_nothing() {
        let (_dir, db, secrets, id) = account_on(&GmailSignIn::BuiltInApi).await;
        let before = held(&db, &secrets, id).await;
        let Passwords::Memory(clients) = &*secrets.google_clients else {
            unreachable!("Secrets::memory keeps clients in memory")
        };
        clients.refuse_saves();
        let to = Change {
            token: Some(token("own-token", Some(own()))),
            ..Change::to(GmailSignIn::OwnProject)
        };

        let refused = change_sign_in(&db, &secrets, Whose::Held(id), to).await;

        assert!(matches!(refused, Err(ChangeError::Token(_))), "{refused:?}");
        assert_eq!(held(&db, &secrets, id).await, before);
    }

    #[tokio::test]
    async fn a_new_address_with_its_secrets_ends_signed_in() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let secrets = Secrets::memory();
        let whose = Whose::New {
            email: ADDRESS.into(),
            now: 0,
        };
        let to = Change {
            password: Some(new_password("new-password")),
            ..Change::to(APP_PASSWORD)
        };

        let id = change_sign_in(&db, &secrets, whose, to).await.unwrap();

        let kept = db.read(move |c| accounts::account(c, id)).await.unwrap();
        assert_eq!(kept.unwrap().state, AccountState::Ok);
        assert_eq!(
            secrets.passwords.load(id).unwrap().as_deref(),
            Some("new-password")
        );
    }

    #[tokio::test]
    async fn overlapping_changes_to_one_account_run_one_after_the_other() {
        let whose = || Whose::Held(4242);
        let first = lock_for(&whose());
        let held = first.lock().await;
        let second = lock_for(&whose());
        assert!(Arc::ptr_eq(&first, &second));
        assert!(second.try_lock().is_err());
        drop(held);
        assert!(second.try_lock().is_ok());
        assert!(!Arc::ptr_eq(&first, &lock_for(&Whose::Held(4243))));
    }

    #[tokio::test]
    async fn a_new_address_the_keyring_refuses_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let secrets = Secrets::memory();
        let Passwords::Memory(passwords) = &*secrets.passwords else {
            unreachable!("Secrets::memory keeps passwords in memory")
        };
        passwords.refuse_saves();
        let whose = Whose::New {
            email: ADDRESS.into(),
            now: 0,
        };
        let to = Change {
            password: Some(new_password("new-password")),
            ..Change::to(APP_PASSWORD)
        };

        let refused = change_sign_in(&db, &secrets, whose, to).await;

        assert!(
            matches!(refused, Err(ChangeError::Password(_))),
            "{refused:?}"
        );
        let kept = db
            .read(|c| accounts::account_by_email(c, ADDRESS))
            .await
            .unwrap();
        assert_eq!(kept, None);
    }

    #[tokio::test]
    async fn a_new_address_added_meanwhile_is_refused_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let secrets = Secrets::memory();
        // Another provider's account took the address after the caller
        // looked.
        let other = db
            .write(|c| {
                let id = accounts::insert_account(c, ADDRESS, 0)?;
                c.execute("UPDATE accounts SET provider = 'imap' WHERE id = ?1", [id])?;
                Ok(id)
            })
            .await
            .unwrap();
        let whose = Whose::New {
            email: ADDRESS.into(),
            now: 0,
        };
        let to = Change {
            password: Some(new_password("new-password")),
            ..Change::to(APP_PASSWORD)
        };

        let refused = change_sign_in(&db, &secrets, whose, to).await;

        assert!(
            matches!(refused, Err(ChangeError::Taken { .. })),
            "{refused:?}"
        );
        let kept = db
            .read(|c| accounts::account_by_email(c, ADDRESS))
            .await
            .unwrap()
            .expect("the other account stays");
        assert_eq!(kept.id, other);
        assert_eq!(kept.provider, mailrs_domain::Provider::Imap);
        assert_eq!(secrets.passwords.load(other).unwrap(), None);
    }
}
