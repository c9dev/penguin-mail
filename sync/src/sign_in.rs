//! Which Google client an account signs in with, and keeping an IMAP, POP3
//! or Microsoft account that just signed in.

use std::sync::Arc;

use mailrs_domain::translate::{fill, gettext};
use mailrs_discover::Server;
use mailrs_domain::{
    Account, AccountId, AccountState, EpochMillis, Provider, RemoveSetting, SignInClient,
};
use mailrs_gmail::{OAuthClient, TokenStore};
use mailrs_imap::{CheckError, ImapError};
use mailrs_pop3::{Pop3Api, Pop3Client, Pop3Error};
use mailrs_store::servers::{self, Pop3Servers, Servers};
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

/// [`signed_in`], then the account's refresh token into `tokens`. The
/// token goes in only once the store has taken the address, so a sign-in
/// refused because an IMAP account holds it leaves nothing in the keyring.
#[allow(clippy::too_many_arguments)]
pub async fn google_signed_in(
    db: &Db,
    tokens: Arc<dyn TokenStore>,
    email: &str,
    refresh_token: &str,
    now: EpochMillis,
    granted: Option<&str>,
    asked: &str,
) -> Result<Account, SignInError> {
    let account = signed_in(db, email, now, granted, asked).await?;
    let (email, refresh) = (email.to_string(), refresh_token.to_string());
    tokio::task::spawn_blocking(move || tokens.save(&email, &refresh))
        .await
        .map_err(|e| SignInError::Token(e.to_string()))?
        .map_err(|e| SignInError::Token(e.to_string()))?;
    Ok(account)
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    /// The address belongs to an account another provider serves.
    #[error("{}", taken(.address, .provider))]
    Taken { address: String, provider: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The keyring refused the refresh token.
    #[error("{0}")]
    Token(String),
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

/// A Microsoft account a person just signed in to. No `Debug`: the token
/// must not reach a log line.
pub struct NewMicrosoft {
    pub address: String,
    /// "Outlook" or "Microsoft 365", from the id token's tenant.
    pub provider_name: String,
    pub refresh_token: String,
    /// The scopes the token answer carried, as `Granted::to_scope` writes them.
    pub granted: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MicrosoftSignInError {
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

/// A POP3 account a person just signed in to. No `Debug`: the password
/// must not reach a log line.
pub struct NewPop3 {
    pub address: String,
    pub provider_name: String,
    pub servers: Pop3Servers,
    /// What the account does with mail on the server.
    pub remove: RemoveSetting,
    pub password: String,
}

/// Keeps a POP3 account whose login worked, as [`imap_signed_in`] keeps an
/// IMAP one: adds it, or finds the one already here for the address, with
/// its servers, its removal setting and its password. Signing in again
/// takes the setting Server Settings showed, keeps the mail, and ends
/// Needs Sign-In.
pub async fn pop3_signed_in<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    new: NewPop3,
    now: EpochMillis,
) -> Result<Account, ImapSignInError> {
    let NewPop3 {
        address,
        provider_name,
        servers,
        remove,
        password,
    } = new;
    let email = address.clone();
    let before = db
        .read(move |c| accounts::account_by_email(c, &email))
        .await?;
    let id = match &before {
        Some(held) if held.provider != Provider::Pop3 => {
            return Err(ImapSignInError::Taken {
                address,
                provider: held.provider_name().to_string(),
            });
        }
        Some(held) => {
            save_password(Arc::clone(&passwords), held.id, password).await?;
            held.id
        }
        None => {
            let email = address.clone();
            let (name, kept) = (provider_name.clone(), servers.clone());
            let added = db
                .write(move |c| {
                    let Some(id) = accounts::insert_pop3_account(c, &email, &name, remove, now)?
                    else {
                        // Another provider's account took the address
                        // between the read and this write.
                        let held = accounts::account_by_email(c, &email)?;
                        return Ok(Err(held.map(|a| a.provider_name().to_string())));
                    };
                    servers::save_pop3(c, id, &kept)?;
                    Ok(Ok(id))
                })
                .await?;
            let id = added.map_err(|provider| ImapSignInError::Taken {
                address: address.clone(),
                provider: provider.unwrap_or_default(),
            })?;
            if let Err(err) = save_password(Arc::clone(&passwords), id, password).await {
                db.write(move |c| accounts::delete_account(c, id)).await?;
                return Err(err);
            }
            id
        }
    };
    if let Some(held) = before {
        let email = address.clone();
        db.write(move |c| {
            accounts::insert_pop3_account(c, &email, &provider_name, remove, now)?;
            servers::save_pop3(c, id, &servers)?;
            // `insert_pop3_account` keeps the setting a row already has;
            // signing in again takes the one Server Settings showed.
            accounts::set_pop3_remove(c, id, remove)?;
            if held.state == AccountState::NeedsReauth {
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

/// Signs in to the POP3 server `server` with `password`, trying the user
/// names its rule allows for `login`, and answers the one it took. A
/// failure comes back as `CheckError::Imap`, in IMAP's words, so Add
/// Account names the incoming server with the sentences it already has.
pub async fn check_pop3(
    server: &Server,
    login: &str,
    password: &str,
) -> Result<String, CheckError> {
    let mut refused = None;
    for user in mailrs_imap::user_names(server.user_name, login) {
        let client = Pop3Client::new(server, mailrs_pop3::Login::new(user.as_str(), password));
        match client.connect().await {
            Ok(_) => {
                // The sign-in already worked; a QUIT the server drops
                // changes nothing on it.
                let _ = client.quit().await;
                return Ok(user);
            }
            Err(err @ Pop3Error::Auth { .. }) => refused = Some(err),
            Err(err) => return Err(CheckError::Imap(in_imap_words(err))),
        }
    }
    let refused = refused.unwrap_or(Pop3Error::Auth {
        text: String::new(),
    });
    Err(CheckError::Imap(in_imap_words(refused)))
}

/// A POP3 failure as the IMAP error Add Account already words. A server
/// without UIDL gets a sentence of its own, since Penguin Mail cannot
/// tell which messages it already has.
pub fn in_imap_words(err: Pop3Error) -> ImapError {
    match err {
        Pop3Error::Network(detail) => ImapError::Network(detail),
        Pop3Error::Tls { host, detail } => ImapError::Tls { host, detail },
        Pop3Error::Auth { text } => ImapError::Auth { text },
        Pop3Error::InUse(text) => ImapError::TooManyConnections { text },
        Pop3Error::Refused(text) => ImapError::Refused(text),
        Pop3Error::Protocol(text) => ImapError::Protocol(text),
        Pop3Error::Unsupported("UIDL") => ImapError::Refused(gettext(
            "This server cannot tell its messages apart, so Penguin Mail cannot download from it safely.",
        )),
        Pop3Error::Unsupported(what) => ImapError::Unsupported(what),
        other @ Pop3Error::TooLarge => ImapError::Protocol(other.to_string()),
    }
}

/// Keeps a Microsoft account that just signed in: adds it, or finds the
/// one already here for the address, and stores its refresh token, the
/// scopes granted and the scopes asked. An account signing in again keeps
/// its mail and stops needing a sign-in. As with IMAP, the token goes in
/// before an existing row changes, and a new row goes again when the
/// keyring refuses, so nothing is left that could never start.
pub async fn microsoft_signed_in<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    new: NewMicrosoft,
    now: EpochMillis,
    asked: &str,
) -> Result<Account, MicrosoftSignInError> {
    let NewMicrosoft {
        address,
        provider_name,
        refresh_token,
        granted,
    } = new;
    let email = address.clone();
    let before = db
        .read(move |c| accounts::account_by_email(c, &email))
        .await?;
    if let Some(held) = &before {
        if held.provider != Provider::Microsoft {
            return Err(MicrosoftSignInError::Taken {
                address,
                provider: held.provider_name().to_string(),
            });
        }
        save_token(Arc::clone(&tokens), held.id, refresh_token.clone()).await?;
    }
    let (email, asked_scopes) = (address.clone(), asked.to_string());
    let again = before.as_ref().map(|held| held.state);
    let added = db
        .write(move |c| {
            let Some(id) = accounts::insert_microsoft_account(c, &email, &provider_name, now)?
            else {
                // Another provider's account took the address between the
                // read and this write.
                let held = accounts::account_by_email(c, &email)?;
                return Ok(Err(held.map(|a| a.provider_name().to_string())));
            };
            if let Some(granted) = &granted {
                accounts::set_granted(c, id, granted)?;
            }
            accounts::set_asked(c, id, &asked_scopes)?;
            // Signing in again is what ends Needs Sign-In; the engine
            // reports every other state itself once the account runs.
            if again == Some(AccountState::NeedsReauth) {
                accounts::set_state(c, id, AccountState::Ok)?;
            }
            Ok(Ok(id))
        })
        .await?;
    let id = added.map_err(|provider| MicrosoftSignInError::Taken {
        address: address.clone(),
        provider: provider.unwrap_or_default(),
    })?;
    if before.is_none()
        && let Err(err) = save_token(Arc::clone(&tokens), id, refresh_token).await
    {
        db.write(move |c| accounts::delete_account(c, id)).await?;
        return Err(err);
    }
    db.read(move |c| accounts::account_by_email(c, &address))
        .await?
        .ok_or(MicrosoftSignInError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

async fn save_token<P: PasswordStore + 'static>(
    tokens: Arc<P>,
    id: AccountId,
    token: String,
) -> Result<(), MicrosoftSignInError> {
    tokio::task::spawn_blocking(move || tokens.save(id, &token))
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    Ok(())
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
    use mailrs_gmail::{MemoryTokenStore, TokenStore, client_from};
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

    /// The bug this pins: a refused Google sign-in had already put its
    /// refresh token in the keyring, where nothing read it again.
    #[tokio::test]
    async fn a_refused_google_sign_in_keeps_no_token() {
        let (_dir, db) = store();
        db.write(|c| accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0))
            .await
            .unwrap();
        let tokens: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::default());
        google_signed_in(&db, Arc::clone(&tokens), "dana@fastmail.com", "refresh", 0, None, "")
            .await
            .expect_err("an IMAP account holds the address");
        assert_eq!(tokens.load("dana@fastmail.com").unwrap(), None);

        google_signed_in(&db, Arc::clone(&tokens), "dana@gmail.com", "refresh", 0, None, "")
            .await
            .unwrap();
        assert_eq!(tokens.load("dana@gmail.com").unwrap().as_deref(), Some("refresh"));
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

    fn new_microsoft(address: &str) -> NewMicrosoft {
        NewMicrosoft {
            address: address.into(),
            provider_name: "Outlook".into(),
            refresh_token: "refresh-1".into(),
            granted: Some("mail.readwrite openid".into()),
        }
    }

    #[tokio::test]
    async fn a_microsoft_sign_in_keeps_the_row_the_token_and_the_consent() {
        let (_dir, db) = store();
        let tokens = Arc::new(crate::passwords::MemoryPasswords::default());
        let account = microsoft_signed_in(&db, Arc::clone(&tokens), new_microsoft("dana@outlook.com"), 0, "openid mail.readwrite")
            .await
            .unwrap();
        assert_eq!(account.provider, mailrs_domain::Provider::Microsoft);
        assert_eq!(tokens.load(account.id).unwrap().as_deref(), Some("refresh-1"));
        let id = account.id;
        let consent = db.read(move |c| accounts::consent(c, id)).await.unwrap();
        assert_eq!(consent.granted.as_deref(), Some("mail.readwrite openid"));
        assert_eq!(consent.asked.as_deref(), Some("openid mail.readwrite"));
    }

    #[tokio::test]
    async fn a_microsoft_sign_in_for_a_gmail_address_is_refused_and_keeps_no_token() {
        let (_dir, db) = store();
        db.write(|c| accounts::insert_account(c, "dana@contoso.com", 0)).await.unwrap();
        let tokens = Arc::new(crate::passwords::MemoryPasswords::default());
        let refused = microsoft_signed_in(&db, Arc::clone(&tokens), new_microsoft("dana@contoso.com"), 0, "")
            .await
            .expect_err("Gmail holds the address");
        assert_eq!(refused.to_string(), "dana@contoso.com is already in Penguin Mail as a Gmail account.");
        assert_eq!(tokens.load(1).unwrap(), None);
    }

    #[tokio::test]
    async fn signing_a_microsoft_account_in_again_ends_needs_sign_in() {
        let (_dir, db) = store();
        let tokens = Arc::new(crate::passwords::MemoryPasswords::default());
        let first = microsoft_signed_in(&db, Arc::clone(&tokens), new_microsoft("dana@outlook.com"), 0, "").await.unwrap();
        let id = first.id;
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth)).await.unwrap();
        let again = microsoft_signed_in(
            &db,
            Arc::clone(&tokens),
            NewMicrosoft { refresh_token: "refresh-2".into(), ..new_microsoft("dana@outlook.com") },
            1,
            "",
        )
        .await
        .unwrap();
        assert_eq!(again.id, id);
        assert_eq!(again.state, AccountState::Ok);
        assert_eq!(tokens.load(id).unwrap().as_deref(), Some("refresh-2"));
    }
}
