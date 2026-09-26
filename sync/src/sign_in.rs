//! Which Google client an account signs in with, and keeping an IMAP
//! account that just signed in.

use std::sync::Arc;

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId, AccountState, EpochMillis, Provider, SignInClient};
use mailrs_gmail::OAuthClient;
use mailrs_store::servers::{self, Servers};
use mailrs_store::{Db, StoreError, accounts};

use crate::config::Config;
use crate::passwords::{PasswordError, PasswordStore};

/// The client `account` signs in with, from what the store recorded for
/// it. An account with no client left, such as an own account whose
/// `config.toml` is gone, or a built-in account in a build without the
/// client, is marked as needing a new sign-in and gets `None`. The engine
/// never starts such an account, so nothing else would mark it.
pub async fn account_client(
    db: &Db,
    config: &Config,
    built_in: Option<OAuthClient>,
    account: &Account,
) -> Result<Option<OAuthClient>, StoreError> {
    let id = account.id;
    let kind = db.read(move |c| accounts::sign_in_client(c, id)).await?;
    let client = client_for(kind, config, built_in);
    if client.is_none() {
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await?;
    }
    Ok(client)
}

/// Adds the account `email` signed in as, or finds it when it is already
/// here, and records that it signed in with the built-in client. Every
/// sign-in uses that client, so an own account that signs in again moves
/// over to it. An address another provider's account holds is refused,
/// so Google services never start on an IMAP account's row.
///
/// `granted` is the scopes Google's token answer said it carries, and
/// `asked` the ones this consent asked for: every [`mailrs_gmail::SIGN_IN_SCOPES`]
/// entry, joined with spaces. Both go on the account row so a later
/// feature can tell a scope the person unticked from one nobody has
/// asked for yet.
pub async fn signed_in(
    db: &Db,
    email: &str,
    now: EpochMillis,
    granted: Option<&str>,
    asked: &str,
) -> Result<Account, SignInError> {
    let address = email.to_string();
    let email = address.clone();
    let granted = granted.map(str::to_string);
    let asked = asked.to_string();
    db.write(move |c| {
        if let Some(held) = accounts::account_by_email(c, &email)?
            && held.provider != Provider::Gmail
        {
            return Ok(Err(held.provider_name().to_string()));
        }
        let id = accounts::insert_account(c, &email, now)?;
        accounts::set_sign_in_client(c, id, SignInClient::BuiltIn)?;
        if let Some(granted) = &granted {
            accounts::set_granted(c, id, granted)?;
        }
        accounts::set_asked(c, id, &asked)?;
        // The row was written a line above in the same transaction.
        accounts::account_by_email(c, &email)?
            .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
            .map(Ok)
    })
    .await?
    .map_err(|provider| SignInError::Taken { address, provider })
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    /// The address belongs to an account another provider serves.
    #[error("{}", taken(.address, .provider))]
    Taken { address: String, provider: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// An IMAP account a person just signed in to: the address, who serves
/// it, the servers that took the login, and the password. It has no
/// `Debug`, so the password cannot reach a log line.
pub struct NewImap {
    pub address: String,
    pub provider_name: String,
    pub servers: Servers,
    pub password: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImapSignInError {
    /// The address belongs to an account another provider serves.
    #[error("{}", taken(.address, .provider))]
    Taken { address: String, provider: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Password(#[from] PasswordError),
}

fn taken(address: &str, provider: &str) -> String {
    fill(
        &gettext("{address} is already in Penguin Mail as a {provider} account."),
        &[("address", address), ("provider", provider)],
    )
}

/// Keeps an IMAP account whose login worked: adds it, or finds the one
/// already here for the address, with its servers and its password. An
/// account signing in again keeps its mail and stops needing a sign-in.
/// The keyring takes the password before anything else changes: an
/// account already here stays as it was when the keyring refuses, and a
/// new one goes again, so nothing is left that could never start.
pub async fn imap_signed_in<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    new: NewImap,
    now: EpochMillis,
) -> Result<Account, ImapSignInError> {
    let NewImap {
        address,
        provider_name,
        servers,
        password,
    } = new;
    let email = address.clone();
    let before = db
        .read(move |c| accounts::account_by_email(c, &email))
        .await?;
    let id = match &before {
        Some(held) if held.provider != Provider::Imap => {
            return Err(ImapSignInError::Taken {
                address,
                provider: held.provider_name().to_string(),
            });
        }
        Some(held) => {
            save_password(Arc::clone(&passwords), held.id, password.clone()).await?;
            held.id
        }
        None => {
            let email = address.clone();
            let (name, kept) = (provider_name.clone(), servers.clone());
            let added = db
                .write(move |c| {
                    let Some(id) = accounts::insert_imap_account(c, &email, &name, now)? else {
                        // Another provider's account took the address
                        // between the read and this write.
                        let held = accounts::account_by_email(c, &email)?;
                        return Ok(Err(held.map(|a| a.provider_name().to_string())));
                    };
                    servers::save(c, id, &kept)?;
                    Ok(Ok(id))
                })
                .await?;
            let id = added.map_err(|provider| ImapSignInError::Taken {
                address: address.clone(),
                provider: provider.unwrap_or_default(),
            })?;
            if let Err(err) = save_password(Arc::clone(&passwords), id, password.clone()).await
            {
                db.write(move |c| accounts::delete_account(c, id)).await?;
                return Err(err);
            }
            id
        }
    };
    if before.is_some() {
        let email = address.clone();
        let again = before.as_ref().map(|held| held.state);
        db.write(move |c| {
            accounts::insert_imap_account(c, &email, &provider_name, now)?;
            servers::save(c, id, &servers)?;
            // Signing in again is what ends Needs Sign-In; the engine
            // reports every other state itself once the account runs.
            if again == Some(AccountState::NeedsReauth) {
                accounts::set_state(c, id, AccountState::Ok)?;
            }
            Ok(())
        })
        .await?;
    }
    db.read(move |c| accounts::account_by_email(c, &address))
        .await?
        .ok_or(ImapSignInError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

/// Hands `password` to the keyring off the async runtime, since the
/// Secret Service call blocks.
async fn save_password<P: PasswordStore + 'static>(
    passwords: Arc<P>,
    id: AccountId,
    password: String,
) -> Result<(), ImapSignInError> {
    tokio::task::spawn_blocking(move || passwords.save(id, &password))
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    Ok(())
}

/// The client for an account that signed in with `client`. A built-in
/// account takes `built_in`, the build's own; an own account takes the one
/// in `config.toml`. `None` when that client is missing, which the caller
/// treats as needing a new sign-in, and a new sign-in uses the built-in one.
pub fn client_for(
    client: SignInClient,
    config: &Config,
    built_in: Option<OAuthClient>,
) -> Option<OAuthClient> {
    match client {
        SignInClient::BuiltIn => built_in,
        SignInClient::Own => config
            .oauth
            .as_ref()
            .map(|own| OAuthClient::new(&own.client_id, &own.client_secret)),
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, SignInClient};
    use mailrs_gmail::client_from;
    use mailrs_store::{Db, accounts};

    use super::*;
    use crate::config::{Config, OAuthConfig};

    fn built_in() -> Option<mailrs_gmail::OAuthClient> {
        client_from(Some("built.apps.googleusercontent.com"), Some("GOCSPX-built"))
    }

    fn with_own() -> Config {
        Config {
            oauth: Some(OAuthConfig {
                client_id: "own.apps.googleusercontent.com".into(),
                client_secret: "GOCSPX-own".into(),
            }),
            ..Config::default()
        }
    }

    #[test]
    fn a_built_in_account_takes_the_builds_client() {
        let client = client_for(SignInClient::BuiltIn, &with_own(), built_in()).unwrap();
        assert_eq!(client.id(), "built.apps.googleusercontent.com");
    }

    #[test]
    fn an_own_account_takes_the_client_in_the_config() {
        let client = client_for(SignInClient::Own, &with_own(), built_in()).unwrap();
        assert_eq!(client.id(), "own.apps.googleusercontent.com");
    }

    #[test]
    fn an_own_account_with_no_client_in_the_config_has_none() {
        assert!(client_for(SignInClient::Own, &Config::default(), built_in()).is_none());
    }

    #[test]
    fn a_built_in_account_in_a_build_without_one_has_none() {
        assert!(client_for(SignInClient::BuiltIn, &with_own(), None).is_none());
    }

    fn store() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        (dir, db)
    }

    async fn account(db: &Db, email: &str, client: SignInClient) -> Account {
        let email = email.to_string();
        db.write(move |c| {
            let id = accounts::insert_account(c, &email, 0)?;
            accounts::set_sign_in_client(c, id, client)?;
            Ok(accounts::account_by_email(c, &email)?.unwrap())
        })
        .await
        .unwrap()
    }

    async fn state(db: &Db, email: &str) -> AccountState {
        let email = email.to_string();
        db.read(move |c| accounts::account_by_email(c, &email))
            .await
            .unwrap()
            .unwrap()
            .state
    }

    #[tokio::test]
    async fn an_account_with_its_client_starts_as_it_was() {
        let (_dir, db) = store();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let client = account_client(&db, &with_own(), built_in(), &own)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client.id(), "own.apps.googleusercontent.com");
        assert_ne!(
            state(&db, "own@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn an_own_account_whose_config_went_needs_a_new_sign_in() {
        let (_dir, db) = store();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let client = account_client(&db, &Config::default(), built_in(), &own)
            .await
            .unwrap();
        assert!(client.is_none());
        assert_eq!(
            state(&db, "own@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn a_built_in_account_in_a_build_without_the_client_needs_a_new_sign_in() {
        let (_dir, db) = store();
        let built = account(&db, "built@example.com", SignInClient::BuiltIn).await;
        let client = account_client(&db, &with_own(), None, &built).await.unwrap();
        assert!(client.is_none());
        assert_eq!(
            state(&db, "built@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn a_new_sign_in_records_the_built_in_client_and_its_consent() {
        let (_dir, db) = store();
        let added = signed_in(&db, "new@example.com", 0, Some("gmail.modify"), "gmail.modify settings")
            .await
            .unwrap();
        let id = added.id;
        let kind = db
            .read(move |c| accounts::sign_in_client(c, id))
            .await
            .unwrap();
        assert_eq!(kind, SignInClient::BuiltIn);
        let consent = db.read(move |c| mailrs_store::accounts::consent(c, id)).await.unwrap();
        assert_eq!(consent.granted.as_deref(), Some("gmail.modify"));
        assert_eq!(consent.asked.as_deref(), Some("gmail.modify settings"));
    }

    #[tokio::test]
    async fn a_google_sign_in_for_an_address_an_imap_account_holds_is_refused() {
        let (_dir, db) = store();
        db.write(|c| accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0))
            .await
            .unwrap();
        let refused = signed_in(&db, "dana@fastmail.com", 0, None, "")
            .await
            .expect_err("an IMAP account holds the address");
        assert_eq!(
            refused.to_string(),
            "dana@fastmail.com is already in Penguin Mail as a Fastmail account."
        );
        let kept = db
            .read(|c| accounts::account_by_email(c, "dana@fastmail.com"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.provider, mailrs_domain::Provider::Imap);
    }

    #[tokio::test]
    async fn signing_an_own_account_in_again_moves_it_to_the_built_in_client() {
        let (_dir, db) = store();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let again = signed_in(&db, "own@example.com", 0, None, "").await.unwrap();
        assert_eq!(again.id, own.id);
        let id = own.id;
        let kind = db
            .read(move |c| accounts::sign_in_client(c, id))
            .await
            .unwrap();
        assert_eq!(kind, SignInClient::BuiltIn);
    }
}
