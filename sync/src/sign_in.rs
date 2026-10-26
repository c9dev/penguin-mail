//! Which Google client an account signs in with, signing one in on its
//! provider's page, and keeping an IMAP, POP3, JMAP or Microsoft account
//! that just signed in.

pub mod app_password;
mod browser;
mod browser_grant;
pub(crate) mod change;
pub mod goa_link;
mod jmap_browser;
pub mod own_client;

pub use browser::{
    Browser, BrowserSignInError, ClientChoice, ClientSource, SignedIn, Wanted, calendar_consent,
    consent_through_built_in, google_sign_in, in_browser, keep_chosen_project,
};
pub use browser_grant::{BrowserGrant, browser_grant};
pub use jmap_browser::{BROWSER_WAIT, BrowserReady, JmapBrowser, browser_ready, jmap_in_browser};

use std::sync::Arc;

use mailrs_discover::Server;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{
    Account, AccountId, AccountState, EpochMillis, GmailSignIn, Provider, RemoveSetting,
    SignInClient,
};
use mailrs_gmail::OAuthClient;
use mailrs_imap::{CheckError, ImapError};
use mailrs_pop3::{Pop3Api, Pop3Client, Pop3Error};
use mailrs_store::jmap::{JmapSaved, SessionOwner};
use mailrs_store::servers::{self, Pop3Servers, Servers};
use mailrs_store::services::{self, ServiceKind};
use mailrs_store::{Db, StoreError, accounts};

use crate::config::Config;
use crate::connect::{STEP_WAIT, read_secret};
use crate::passwords::{PasswordError, PasswordStore, Secrets};
use crate::services::finding::{self, ServiceProbe};
use crate::{BackendError, SyncError};
use change::{Change, ChangeError, NewToken, Whose, change_sign_in};
use own_client::OwnClient;

/// The client `account` signs in with, from what the store recorded for
/// it. An own account takes the client `kept` holds under its id, and
/// one added through the old setup page, with nothing kept, the one in
/// `config.toml`; a built-in account takes `built_in`. A new sign-in
/// chooses its client by [`google_sign_in`] instead. An account with no
/// client left, such as an own account whose `config.toml` is gone, or a
/// built-in account in a build without the client, is marked as needing a
/// new sign-in and gets `None`. The engine never starts such an account,
/// so nothing else would mark it.
pub async fn account_client<P: PasswordStore + 'static>(
    db: &Db,
    config: &Config,
    built_in: Option<OAuthClient>,
    kept: Arc<P>,
    account: &Account,
) -> Result<Option<OAuthClient>, SyncError> {
    let client = client_of(db, config, built_in, kept, account).await?;
    if client.is_none() {
        let id = account.id;
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await?;
    }
    Ok(client)
}

/// [`account_client`] without the state write, for an account that can
/// run without the client.
pub(crate) async fn client_of<P: PasswordStore + 'static>(
    db: &Db,
    config: &Config,
    built_in: Option<OAuthClient>,
    kept: Arc<P>,
    account: &Account,
) -> Result<Option<OAuthClient>, SyncError> {
    let id = account.id;
    let sign_in = db.read(move |c| accounts::gmail_sign_in(c, id)).await?;
    // An account on Online Accounts takes its mail from there, and its
    // calendar, contacts and Drive from the built-in client's own token,
    // unless it came from its own project: its Google token is that
    // project's while the keyring keeps the project, since a consent
    // through the built-in client deletes it.
    if sign_in.on_online_accounts() {
        let kept = kept_own(kept, id).await?;
        return Ok(kept.map(|own| own.oauth()).or(built_in));
    }
    Ok(match sign_in.client() {
        SignInClient::Own => own_client_of(config, kept, id)
            .await?
            .map(|own| own.oauth()),
        SignInClient::BuiltIn => built_in,
    })
}

/// What an account signing in again kept of its own client.
pub enum KeptClient {
    /// The address is no Gmail account that signed in with an own client.
    NotOwn,
    /// The own client it signed in with.
    Found(OwnClient),
    /// It signed in with an own client that is no longer on this computer.
    Missing,
}

/// The own client the Gmail account at `address` signed in with, so that
/// signing it in again keeps it: the one its keyring entry holds, else the
/// one in `config.toml`.
pub async fn kept_own_client(
    db: &Db,
    config: &Config,
    secrets: &Secrets,
    address: &str,
) -> Result<KeptClient, SyncError> {
    let address = address.to_string();
    let own = db
        .read(move |c| match accounts::account_by_email(c, &address)? {
            Some(held) if held.provider == Provider::Gmail => {
                let own = accounts::gmail_sign_in(c, held.id)?.client() == SignInClient::Own;
                Ok(own.then_some(held.id))
            }
            _ => Ok(None),
        })
        .await?;
    let Some(id) = own else {
        return Ok(KeptClient::NotOwn);
    };
    Ok(
        match own_client_of(config, Arc::clone(&secrets.google_clients), id).await? {
            Some(own) => KeptClient::Found(own),
            None => KeptClient::Missing,
        },
    )
}

/// An own account's client: the one `kept` holds under `id`, else the one
/// in `config.toml`.
/// The own client the keyring keeps for `id`, without the `config.toml`
/// fallback: for an account on Online Accounts, whose Google token is an
/// own project's only while the keyring keeps the project.
pub(crate) async fn kept_own<P: PasswordStore + 'static>(
    kept: Arc<P>,
    id: AccountId,
) -> Result<Option<OwnClient>, SyncError> {
    Ok(read_secret(kept, id, STEP_WAIT)
        .await?
        .as_deref()
        .and_then(OwnClient::decode))
}

async fn own_client_of<P: PasswordStore + 'static>(
    config: &Config,
    kept: Arc<P>,
    id: AccountId,
) -> Result<Option<OwnClient>, SyncError> {
    let kept = read_secret(kept, id, STEP_WAIT)
        .await?
        .as_deref()
        .and_then(OwnClient::decode);
    Ok(kept.or_else(|| {
        config
            .oauth
            .as_ref()
            .map(|own| OwnClient::new(&own.client_id, &own.client_secret))
    }))
}

/// Adds the account `email` signed in as, or finds it when it is already
/// here, keeps its refresh token, and `own`, the own client it signed in
/// with, under its id. An account that signs in again takes the client of
/// that sign-in; a sign-in with the built-in client deletes any own client
/// kept for the account, which no longer refreshes through it. An address
/// another provider's account holds is refused, so Google services never
/// start on an IMAP account's row, and nothing reaches the keyring.
///
/// `granted` is the scopes Google's token answer said it carries, and
/// `asked` the ones this consent asked for: the asking client's scopes,
/// joined with spaces. Both go on the account row so a later feature can
/// tell a scope the person unticked from one nobody has asked for yet.
#[allow(clippy::too_many_arguments)]
pub async fn google_signed_in(
    db: &Db,
    secrets: &Secrets,
    email: &str,
    refresh_token: &str,
    now: EpochMillis,
    granted: Option<&str>,
    asked: &str,
    own: Option<&OwnClient>,
) -> Result<Account, SignInError> {
    let client = match own {
        Some(_) => SignInClient::Own,
        None => SignInClient::BuiltIn,
    };
    let token = GoogleToken {
        email,
        refresh_token,
        granted,
        asked,
    };
    let to = move |before: &GmailSignIn| before.with_google_token(client);
    google_signed_in_as(db, secrets, token, now, to, own).await
}

/// What a Google sign-in brought back: the address, its refresh token,
/// the scopes Google granted and the ones the sign-in asked for.
#[derive(Clone, Copy)]
pub struct GoogleToken<'a> {
    pub email: &'a str,
    pub refresh_token: &'a str,
    pub granted: Option<&'a str>,
    pub asked: &'a str,
}

// The refresh token signs in as the person, so Debug output, which can end
// up in a log or a panic message, hides it.
impl std::fmt::Debug for GoogleToken<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleToken")
            .field("email", &self.email)
            .field("refresh_token", &"<hidden>")
            .field("granted", &self.granted)
            .field("asked", &self.asked)
            .finish()
    }
}

/// [`google_signed_in`] with `to` saying what the account's sign-in becomes
/// from what it is, for an account whose mail does not sign in with this
/// token: one on the IMAP route that adds its calendar. A new address
/// starts from the built-in client on the API.
pub async fn google_signed_in_as(
    db: &Db,
    secrets: &Secrets,
    token: GoogleToken<'_>,
    now: EpochMillis,
    to: impl FnOnce(&GmailSignIn) -> GmailSignIn,
    own: Option<&OwnClient>,
) -> Result<Account, SignInError> {
    let GoogleToken {
        email,
        refresh_token,
        granted,
        asked,
    } = token;
    let address = email.to_string();
    let held = db
        .read(move |c| match accounts::account_by_email(c, &address)? {
            Some(held) if held.provider != Provider::Gmail => {
                Ok(Err(held.provider_name().to_string()))
            }
            Some(held) => Ok(Ok(Some((held.id, accounts::gmail_sign_in(c, held.id)?)))),
            None => Ok(Ok(None)),
        })
        .await?
        .map_err(|provider| SignInError::Taken {
            address: email.to_string(),
            provider,
        })?;
    let (whose, to) = match held {
        Some((id, from)) => (Whose::Held(id), to(&from)),
        None => {
            let email = email.to_string();
            (Whose::New { email, now }, to(&GmailSignIn::BuiltInApi))
        }
    };
    let change = Change {
        token: Some(NewToken {
            email: email.to_string(),
            refresh_token: refresh_token.to_string(),
            own: own.cloned(),
            granted: granted.map(str::to_string),
            asked: asked.to_string(),
        }),
        ..Change::to(to)
    };
    let id = change_sign_in(db, secrets, whose, change).await?;
    db.read(move |c| accounts::account(c, id))
        .await?
        .ok_or(SignInError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

impl From<ChangeError> for SignInError {
    fn from(err: ChangeError) -> Self {
        match err {
            ChangeError::Token(reason) => SignInError::Token(reason),
            ChangeError::Password(err) => SignInError::Token(err.to_string()),
            ChangeError::Store(err) => SignInError::Store(err),
            ChangeError::Taken { address, provider } => SignInError::Taken { address, provider },
        }
    }
}

impl From<ChangeError> for ImapSignInError {
    fn from(err: ChangeError) -> Self {
        match err {
            ChangeError::Token(reason) => ImapSignInError::Password(PasswordError::Keyring(reason)),
            ChangeError::Password(err) => ImapSignInError::Password(err),
            ChangeError::Store(err) => ImapSignInError::Store(err),
            ChangeError::Taken { address, provider } => taken_error(&address, &provider),
        }
    }
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
    /// The outgoing server's own password, when it takes a different one.
    pub smtp_password: Option<String>,
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

fn different_account(address: &str) -> String {
    fill(
        &gettext("You signed in to a different account than {address}, so nothing changed."),
        &[("address", address)],
    )
}

/// The error for an address another provider's account holds.
fn taken_error(address: &str, provider: &str) -> ImapSignInError {
    ImapSignInError::Taken {
        address: address.to_string(),
        provider: provider.to_string(),
    }
}

fn taken(address: &str, provider: &str) -> String {
    let held = fill(
        &gettext("{address} is already in Penguin Mail as a {provider} account."),
        &[("address", address), ("provider", provider)],
    );
    format!("{held} {}", remove_and_add_again())
}

/// What to do to sign an account in another way than it is.
fn remove_and_add_again() -> String {
    gettext("To sign in another way, remove this account and add it again.")
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
        smtp_password,
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
            save_passwords(
                Arc::clone(&passwords),
                held.id,
                password.clone(),
                smtp_password.clone(),
            )
            .await?;
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
            if let Err(err) =
                save_passwords(Arc::clone(&passwords), id, password.clone(), smtp_password).await
            {
                db.write(move |c| accounts::delete_account(c, id)).await?;
                return Err(err.into());
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

/// A calendar account a person is signing in to: the server's address as
/// they typed it, the user name and the password. No `Debug`: the
/// password must not reach a log line.
pub struct NewCalDav {
    pub url: String,
    pub user_name: String,
    pub password: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CalDavSignInError {
    /// The name belongs to an account another provider serves. `holder`
    /// says which kind: an IMAP or POP3 account can take the server's
    /// calendars itself, and the page says so.
    #[error("{}", taken(.name, .provider))]
    Taken {
        name: String,
        provider: String,
        holder: Provider,
    },
    /// A calendar account already has the name, for a server at another
    /// address. Adding this one would take that account's place, so
    /// nothing was sent.
    #[error("{}", elsewhere(.name, .address))]
    Elsewhere { name: String, address: String },
    /// A calendar account already has the name, signed in to the same
    /// server as `user`, a user name that differs only in case. A server
    /// may tell the two apart, so this login does not take that account's
    /// place, and nothing was sent.
    #[error("{}", other_user(.name, .user))]
    OtherUser { name: String, user: String },
    /// Sign In Again was given a server at another address, or another
    /// user, than the account's own. Taking them would move its calendar
    /// copy, its queued changes and its password there, so nothing was
    /// sent.
    #[error("{}", moves(.name, .address))]
    Moves { name: String, address: String },
    /// The address is under a scheme other than HTTPS. Nothing was sent
    /// to it.
    #[error("{}", finding::https_only())]
    NotHttps,
    /// No calendar is at the address, or the server there did not take
    /// the login.
    #[error(transparent)]
    Server(#[from] SyncError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Password(#[from] PasswordError),
}

/// What a calendar account on the server at `url` is called: its user on
/// its host, as GNOME names one, in lower case, so Dana and dana cannot
/// be two accounts that look alike. The login is the user name as the
/// server took it, case and all (see [`same_login`]). A mail account is
/// called by its address; a name that carries the host cannot be one, so
/// a calendar account never takes a mail account's place even where the
/// person signs in to both with one address. `None` for an address with
/// no host, or no user.
pub(crate) fn calendar_account_name(url: &str, user_name: &str) -> Option<String> {
    let (host, user) = (mailrs_dav::host_of(url)?, user_name.trim().to_lowercase());
    (!user.is_empty()).then(|| format!("{user}@{host}"))
}

fn elsewhere(name: &str, address: &str) -> String {
    fill(
        &gettext(
            "A calendar account named {name} is already here, for the server at {address}. Remove it to add this server under that name.",
        ),
        &[("name", name), ("address", address)],
    )
}

fn other_user(name: &str, user: &str) -> String {
    fill(
        &gettext(
            "A calendar account named {name} is already here, signed in to this server as {user}. Type {user} to sign it in again, or remove it to sign in as someone else.",
        ),
        &[("name", name), ("user", user)],
    )
}

fn moves(name: &str, address: &str) -> String {
    fill(
        &gettext(
            "Sign In Again keeps {name} on the server at {address}. To use another server or user name, add it as a calendar account of its own.",
        ),
        &[("name", name), ("address", address)],
    )
}

/// Where calendar account `held` is kept, for a line that names it: the
/// address of the CalDAV server it keeps, or the host its name carries
/// when it keeps none.
fn kept_at(held: &Account, kept: Option<&services::FoundService>) -> String {
    match kept {
        Some(kept) => kept.url.clone(),
        None => held
            .email
            .rsplit_once('@')
            .map_or(&*held.email, |(_, host)| host)
            .to_string(),
    }
}

/// The CalDAV server calendar account `id` keeps, when it keeps one.
async fn kept_server(
    db: &Db,
    id: AccountId,
) -> Result<Option<services::FoundService>, CalDavSignInError> {
    let kept = db.read(move |c| services::load(c, id)).await?;
    Ok(kept.into_iter().find(|s| s.kind == ServiceKind::CalDav))
}

/// Whether `url` and `user` are the login a calendar account keeps in
/// `kept`: the address of its server, and the user name that server took
/// with its case, since a server such as Radicale tells Robin from robin.
/// An account that keeps no server has no login to match.
fn same_login(kept: Option<&services::FoundService>, url: &str, user: &str) -> bool {
    kept.is_some_and(|kept| mailrs_dav::same_address(&kept.url, url) && kept.user_name == user)
}

/// Why adding `url` as `user` cannot sign in `held`, the account that
/// holds the name, with the server it keeps in `kept`: another kind of
/// account, a server at another address, or another user there. `None`
/// when they are its own login.
fn add_refused(
    held: &Account,
    kept: Option<&services::FoundService>,
    url: &str,
    user: &str,
) -> Option<CalDavSignInError> {
    if held.provider != Provider::CalDav {
        return Some(CalDavSignInError::taken(held.email.clone(), Some(held)));
    }
    if same_login(kept, url, user) {
        return None;
    }
    let name = held.email.clone();
    Some(match kept {
        Some(kept) if mailrs_dav::same_address(&kept.url, url) => CalDavSignInError::OtherUser {
            name,
            user: kept.user_name.clone(),
        },
        _ => CalDavSignInError::Elsewhere {
            address: kept_at(held, kept),
            name,
        },
    })
}

/// Calendar sign-ins in this process run one at a time. Each reads who
/// holds a name, asks the server, then writes; two at once could both
/// find a name free, and the later one take or remove the account the
/// earlier one added. The writes still check what they read, for a
/// change that comes from elsewhere, such as the account's removal.
static SIGNING_IN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Writing a secret for an account read from the store takes turns, in
/// this process, with removing an account and forgetting its secrets.
/// SQLite gives a removed account's id to the next account added, so a
/// password written for an account removed meanwhile would land on the
/// new account's own. A calendar sign-in holds a turn from the last time
/// it reads the account until the store has its write, and removing an
/// account holds one from the delete until its secrets are forgotten.
static SECRETS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A turn at the account secrets; see [`SECRETS`]. Removing an account
/// holds it from the delete until its secrets are forgotten.
pub async fn secrets_turn() -> tokio::sync::MutexGuard<'static, ()> {
    SECRETS.lock().await
}

/// Adds a calendar account once its server took the login, with the
/// server's address, the user name and the password. `probe` tries the
/// login first, so a wrong address or password keeps nothing. The name
/// another kind of account holds is refused, and so is the name of a
/// calendar account kept at another login: a server at another address,
/// or another user there. That happens before the password goes
/// anywhere: adding it would move that account to this login. The same
/// login added again signs its account in again, as Add Account does for
/// an IMAP account. As for an IMAP account, the keyring takes the
/// password before anything else is settled, and an account this sign-in
/// added goes again when it refuses.
pub async fn caldav_signed_in<S: ServiceProbe, P: PasswordStore + 'static>(
    probe: &S,
    db: &Db,
    passwords: Arc<P>,
    new: NewCalDav,
    now: EpochMillis,
) -> Result<Account, CalDavSignInError> {
    let _turn = SIGNING_IN.lock().await;
    let url = finding::https_url(&new.url).ok_or(CalDavSignInError::NotHttps)?;
    let name = calendar_account_name(&url, &new.user_name).ok_or_else(|| {
        SyncError::Backend(BackendError::Refused(
            "the address names no host, or no user was given".into(),
        ))
    })?;
    let user_name = new.user_name.trim();
    let lookup = name.clone();
    let held = db
        .read(move |c| accounts::account_by_email(c, &lookup))
        .await?;
    if let Some(held) = held {
        let kept = kept_server(db, held.id).await?;
        if let Some(refused) = add_refused(&held, kept.as_ref(), &url, user_name) {
            return Err(refused);
        }
        let server =
            finding::typed(probe, ServiceKind::CalDav, url, user_name, &new.password).await?;
        return keep_again(db, passwords, held, server, new.password).await;
    }
    let server = finding::typed(probe, ServiceKind::CalDav, url, user_name, &new.password).await?;
    enum Added {
        New(AccountId),
        Found(Account, services::FoundService),
        Refused(CalDavSignInError),
    }
    // Until the keyring has the password, the account this adds is not
    // to be removed, and its id not given to another.
    let turn = SECRETS.lock().await;
    let added = db
        .write(move |c| {
            if let Some(id) = accounts::insert_caldav_account(c, &name, now)? {
                services::save(c, id, &server)?;
                return Ok(Added::New(id));
            }
            // An account took the name while the server was asked. It is
            // checked as one found before asking would be.
            let held = accounts::account_by_email(c, &name)?
                .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
            let kept = services::load(c, held.id)?
                .into_iter()
                .find(|s| s.kind == ServiceKind::CalDav);
            Ok(
                match add_refused(&held, kept.as_ref(), &server.url, &server.user_name) {
                    Some(refused) => Added::Refused(refused),
                    None => Added::Found(held, server),
                },
            )
        })
        .await?;
    let id = match added {
        Added::New(id) => id,
        Added::Found(held, server) => {
            // `keep_again` takes its own turn, and reads the account again.
            drop(turn);
            return keep_again(db, passwords, held, server, new.password).await;
        }
        Added::Refused(refused) => return Err(refused),
    };
    if let Err(err) = save_passwords(passwords, id, new.password, None).await {
        db.write(move |c| accounts::delete_account(c, id)).await?;
        return Err(err.into());
    }
    drop(turn);
    account_by_id(db, id).await
}

/// Signs calendar account `id` in again, as Sign In Again on its heading
/// asks: tries the login at the address and user `new` gives, which the
/// page fills in from the account, then keeps the password and ends Needs
/// Sign-In. The account keeps its calendars. The address and the user
/// must be its own login (see [`same_login`]); anything else is refused
/// before the password goes anywhere, so an editable page cannot move the
/// account. One that keeps no server has no login to match, and is
/// removed and added again instead.
pub async fn caldav_signed_in_again<S: ServiceProbe, P: PasswordStore + 'static>(
    probe: &S,
    db: &Db,
    passwords: Arc<P>,
    id: AccountId,
    new: NewCalDav,
) -> Result<Account, CalDavSignInError> {
    let _turn = SIGNING_IN.lock().await;
    let NewCalDav {
        url,
        user_name,
        password,
    } = new;
    let url = finding::https_url(&url).ok_or(CalDavSignInError::NotHttps)?;
    let user_name = user_name.trim();
    let held = account_by_id(db, id).await?;
    // The password would land on a mail account's own in the keyring.
    if held.provider != Provider::CalDav {
        return Err(CalDavSignInError::taken(held.email.clone(), Some(&held)));
    }
    let kept = kept_server(db, id).await?;
    if !same_login(kept.as_ref(), &url, user_name) {
        return Err(CalDavSignInError::Moves {
            address: kept_at(&held, kept.as_ref()),
            name: held.email,
        });
    }
    let server = finding::typed(probe, ServiceKind::CalDav, url, user_name, &password).await?;
    keep_again(db, passwords, held, server, password).await
}

/// Keeps the password and the server of a login the server just took
/// again for calendar account `held`, and ends Needs Sign-In. While the
/// server was asked the account may have gone, its id gone to another
/// account, or its login changed, so it is read again before the keyring
/// takes the password, and again in the write. A turn at the secrets is
/// held from that read to the write, so the account cannot be removed,
/// and its id given to another, while the keyring writes. The keyring
/// goes first, so the account stays as it was when it refuses.
async fn keep_again<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    held: Account,
    server: services::FoundService,
    password: String,
) -> Result<Account, CalDavSignInError> {
    let id = held.id;
    let _turn = SECRETS.lock().await;
    let (before, kept) = (held.clone(), server.clone());
    db.read(move |c| still_its(c, &before, &kept)).await??;
    save_passwords(passwords, id, password, None).await?;
    db.write(move |c| {
        let now = match still_its(c, &held, &server)? {
            Ok(now) => now,
            Err(refused) => return Ok(Err(refused)),
        };
        services::save(c, id, &server)?;
        // Signing in again is what ends Needs Sign-In; the calendar copy
        // says the account works once it reads.
        if now.state == AccountState::NeedsReauth {
            accounts::set_state(c, id, AccountState::Ok)?;
        }
        Ok(Ok(()))
    })
    .await??;
    account_by_id(db, id).await
}

/// Calendar account `held` as the store holds it now, while `server` is
/// still its login. The account gone, or its id another account's, is
/// the store error a missing account is; another login kept meanwhile
/// refuses as Sign In Again does.
fn still_its(
    c: &rusqlite::Connection,
    held: &Account,
    server: &services::FoundService,
) -> Result<Result<Account, CalDavSignInError>, StoreError> {
    let now = accounts::account(c, held.id)?
        .filter(|now| now.email == held.email && now.provider == Provider::CalDav);
    let Some(now) = now else {
        return Ok(Err(CalDavSignInError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        ))));
    };
    let kept = services::load(c, held.id)?
        .into_iter()
        .find(|s| s.kind == ServiceKind::CalDav);
    if !same_login(kept.as_ref(), &server.url, &server.user_name) {
        return Ok(Err(CalDavSignInError::Moves {
            address: kept_at(&now, kept.as_ref()),
            name: now.email,
        }));
    }
    Ok(Ok(now))
}

/// The account with this id, whatever its provider, as the store holds it
/// now. One that is gone is a store error: a sign-in only asks for an
/// account it just added or was opened for.
async fn account_by_id(db: &Db, id: AccountId) -> Result<Account, CalDavSignInError> {
    db.read(move |c| accounts::account(c, id))
        .await?
        .ok_or(CalDavSignInError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

impl CalDavSignInError {
    /// The name is `held`'s, an account another provider serves.
    fn taken(name: String, held: Option<&Account>) -> Self {
        CalDavSignInError::Taken {
            name,
            provider: held
                .map(|held| held.provider_name().to_string())
                .unwrap_or_default(),
            holder: held.map(|held| held.provider).unwrap_or_default(),
        }
    }
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
    /// The outgoing server's own password, when it takes a different one.
    pub smtp_password: Option<String>,
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
        smtp_password,
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
            save_passwords(Arc::clone(&passwords), held.id, password, smtp_password).await?;
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
            if let Err(err) =
                save_passwords(Arc::clone(&passwords), id, password, smtp_password).await
            {
                db.write(move |c| accounts::delete_account(c, id)).await?;
                return Err(err.into());
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

/// A JMAP account a person just signed in to. No `Debug`: the secret must
/// not reach a log line.
pub struct NewJmap {
    pub address: String,
    pub provider_name: String,
    pub saved: mailrs_store::jmap::JmapSaved,
    /// The password or the token.
    pub secret: String,
    /// An app password for the account's CalDAV and CardDAV servers, when
    /// the JMAP secret is a token those servers will not take.
    pub dav_password: Option<String>,
    /// Who the session the sign-in reached belongs to.
    pub signed_as: SignedAs,
}

/// Who a JMAP sign-in reached: the session's owner, and the addresses its
/// identities name. No `Debug`: both hold addresses.
#[derive(Clone)]
pub struct SignedAs {
    pub owner: SessionOwner,
    pub sends_as: Vec<String>,
}

impl SignedAs {
    /// The session's `username` and primary mail account, and the
    /// identities' addresses, from a sign-in check.
    pub fn of(checked: &mailrs_jmap::Checked) -> SignedAs {
        let session = &checked.session;
        SignedAs {
            owner: SessionOwner {
                username: session.username.clone(),
                mail_account: session
                    .primary_accounts
                    .get(mailrs_jmap::MAIL)
                    .cloned()
                    .unwrap_or_default(),
            },
            sends_as: checked.addresses.clone(),
        }
    }

    /// Whether this session may hold the account at `address`: its user
    /// name is the address, or a login name that is the address's local
    /// part, or one of its identities sends as the address.
    fn sends_as(&self, address: &str) -> bool {
        let (address, username) = (address.trim(), self.owner.username.trim());
        let local = address.split_once('@').map(|(local, _)| local);
        username.eq_ignore_ascii_case(address)
            || (!username.is_empty()
                && !username.contains('@')
                && local.is_some_and(|l| l.eq_ignore_ascii_case(username)))
            || self
                .sends_as
                .iter()
                .any(|a| a.trim().eq_ignore_ascii_case(address))
    }
}

#[cfg(test)]
impl SignedAs {
    /// A session whose user name is `address`, with mail account `A1`.
    pub(crate) fn address(address: &str) -> SignedAs {
        SignedAs {
            owner: SessionOwner {
                username: address.into(),
                mail_account: "A1".into(),
            },
            sends_as: vec![address.into()],
        }
    }
}

/// Whether a sign-in that reached `signed` at `session_url` may keep the
/// account at `address`, whose row is `held` when it is already here. An
/// account already here must stay on its server and reach the owner it
/// had: the same user name or the same mail account. A row kept before the
/// owner was recorded, and a new account, need a session that sends as the
/// address. A browser signed in to another account, or a session URL typed
/// for someone else's server, then changes nothing.
fn same_owner(
    held: Option<&JmapSaved>,
    address: &str,
    session_url: &str,
    signed: &SignedAs,
) -> bool {
    if let Some(held) = held {
        if !mailrs_jmap::same_server(&held.session_url, session_url) {
            return false;
        }
        if let Some(owner) = &held.owner {
            let user = !owner.username.trim().is_empty()
                && owner
                    .username
                    .trim()
                    .eq_ignore_ascii_case(signed.owner.username.trim());
            let account =
                !owner.mail_account.is_empty() && owner.mail_account == signed.owner.mail_account;
            return user || account;
        }
    }
    signed.sends_as(address)
}

#[derive(Debug, thiserror::Error)]
pub enum JmapSignInError {
    /// The address belongs to an account another provider serves.
    #[error("{}", taken(.address, .provider))]
    Taken { address: String, provider: String },
    /// The session belongs to someone other than the account at the
    /// address, so nothing was kept.
    #[error("{}", different_account(.address))]
    DifferentAccount { address: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Password(#[from] PasswordError),
}

/// Keeps a JMAP account whose sign-in worked: adds it, or finds the one
/// already here for the address, with its session row and its secret. As
/// for IMAP, the keyring takes the secret before anything else changes,
/// and a new row goes again when it refuses. A new row may take the id of
/// an account removed before it, so any password still kept under that id
/// goes first: part 3 would send it to this account's CalDAV server.
pub async fn jmap_signed_in(
    db: &Db,
    secrets: &Secrets,
    new: NewJmap,
    now: EpochMillis,
) -> Result<Account, JmapSignInError> {
    let NewJmap {
        address,
        provider_name,
        saved,
        secret,
        dav_password,
        signed_as,
    } = new;
    let mut saved = saved;
    let email = address.clone();
    let (before, held_row) = db
        .read(move |c| {
            let held = accounts::account_by_email(c, &email)?;
            let row = match &held {
                Some(held) if held.provider == Provider::Jmap => {
                    mailrs_store::jmap::load(c, held.id)?
                }
                _ => None,
            };
            Ok((held, row))
        })
        .await?;
    if let Some(held) = &before
        && held.provider != Provider::Jmap
    {
        return Err(JmapSignInError::Taken {
            address,
            provider: held.provider_name().to_string(),
        });
    }
    if !same_owner(held_row.as_ref(), &address, &saved.session_url, &signed_as) {
        return Err(JmapSignInError::DifferentAccount { address });
    }
    saved.owner = Some(signed_as.owner);
    if let Some(held) = &before {
        // The old session keeps running, and saving, until the new token
        // is in the keyring. Retiring it then waits for any save it had
        // under way and stops later ones. A save that finished between the
        // write and the retire is undone by writing the token once more.
        keep_secret(Arc::clone(&secrets.jmap), held.id, secret.clone()).await?;
        if secrets.jmap_sessions.forget(held.id).await.is_some() {
            keep_secret(Arc::clone(&secrets.jmap), held.id, secret.clone()).await?;
        }
        if let Some(password) = dav_password.clone() {
            keep_secret(Arc::clone(&secrets.passwords), held.id, password).await?;
        }
    }
    let (email, again) = (address.clone(), before.as_ref().map(|held| held.state));
    let added = db
        .write(move |c| {
            let Some(id) = accounts::insert_jmap_account(c, &email, &provider_name, now)? else {
                // Another provider's account took the address between the
                // read and this write.
                let held = accounts::account_by_email(c, &email)?;
                return Ok(Err(held.map(|a| a.provider_name().to_string())));
            };
            mailrs_store::jmap::save(c, id, &saved)?;
            if again == Some(AccountState::NeedsReauth) {
                accounts::set_state(c, id, AccountState::Ok)?;
            }
            Ok(Ok(id))
        })
        .await?;
    let id = added.map_err(|provider| JmapSignInError::Taken {
        address: address.clone(),
        provider: provider.unwrap_or_default(),
    })?;
    if before.is_none()
        && let Err(err) = clear_and_keep(secrets, id, secret, dav_password).await
    {
        db.write(move |c| accounts::delete_account(c, id)).await?;
        return Err(err);
    }
    db.read(move |c| accounts::account_by_email(c, &address))
        .await?
        .ok_or(JmapSignInError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

/// Deletes the passwords a removed account may have left under `id`, then
/// keeps `secret` as the JMAP account's and `dav_password`, when given, as
/// its calendar and contacts servers' password.
async fn clear_and_keep(
    secrets: &Secrets,
    id: AccountId,
    secret: String,
    dav_password: Option<String>,
) -> Result<(), JmapSignInError> {
    let passwords = Arc::clone(&secrets.passwords);
    tokio::task::spawn_blocking(move || {
        crate::passwords::first_error([passwords.delete(id), passwords.delete_outgoing(id)])
    })
    .await
    .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    keep_secret(Arc::clone(&secrets.jmap), id, secret).await?;
    if let Some(password) = dav_password {
        keep_secret(Arc::clone(&secrets.passwords), id, password).await?;
    }
    Ok(())
}

async fn keep_secret<P: PasswordStore + 'static>(
    secrets: Arc<P>,
    id: AccountId,
    secret: String,
) -> Result<(), JmapSignInError> {
    tokio::task::spawn_blocking(move || secrets.save(id, &secret))
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    Ok(())
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

/// Hands `password`, and the outgoing server's own password when there
/// is one, to the keyring off the async runtime, since the Secret Service
/// call blocks. Without one, an outgoing password kept from before goes,
/// so the outgoing server signs in with the incoming password as the
/// form said it would.
pub(crate) async fn save_passwords<P: PasswordStore + 'static>(
    passwords: Arc<P>,
    id: AccountId,
    password: String,
    smtp_password: Option<String>,
) -> Result<(), PasswordError> {
    tokio::task::spawn_blocking(move || {
        passwords.save(id, &password)?;
        match smtp_password {
            Some(outgoing) => passwords.save_outgoing(id, &outgoing),
            None => passwords.delete_outgoing(id),
        }
    })
    .await
    .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, SignInClient};
    use mailrs_gmail::client_from;
    use mailrs_store::{Db, accounts};

    use super::*;
    use crate::config::{Config, OAuthConfig};
    use crate::passwords::{MemoryPasswords, Secrets};
    use crate::{Clients, Connector};

    /// A keyring holding no own client for any account.
    fn kept_none() -> Arc<MemoryPasswords> {
        Arc::new(MemoryPasswords::default())
    }

    #[test]
    fn google_token_debug_hides_the_refresh_token() {
        let token = GoogleToken {
            email: "a@gmail.com",
            refresh_token: "1//secret-refresh",
            granted: None,
            asked: "scope",
        };
        let shown = format!("{token:?}");
        assert!(!shown.contains("secret-refresh"));
        assert!(shown.contains("a@gmail.com"));
    }

    fn built_in() -> Option<mailrs_gmail::OAuthClient> {
        client_from(
            Some("built.apps.googleusercontent.com"),
            Some("GOCSPX-built"),
        )
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

    fn store() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        (dir, db)
    }

    async fn account(db: &Db, email: &str, client: SignInClient) -> Account {
        let email = email.to_string();
        db.write(move |c| {
            let id = accounts::insert_account(c, &email, 0)?;
            accounts::set_gmail_sign_in(c, id, &GmailSignIn::BuiltInApi.with_google_token(client))?;
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
        let client = account_client(&db, &with_own(), built_in(), kept_none(), &own)
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
        let client = account_client(&db, &Config::default(), built_in(), kept_none(), &own)
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
        let client = account_client(&db, &with_own(), None, kept_none(), &built)
            .await
            .unwrap();
        assert!(client.is_none());
        assert_eq!(
            state(&db, "built@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn a_new_sign_in_records_the_built_in_client_and_its_consent() {
        let (_dir, db) = store();
        let added = google_signed_in(
            &db,
            &Secrets::memory(),
            "new@example.com",
            "refresh",
            0,
            Some("gmail.modify"),
            "gmail.modify settings",
            None,
        )
        .await
        .unwrap();
        let id = added.id;
        let kind = db
            .read(move |c| Ok(accounts::gmail_sign_in(c, id)?.client()))
            .await
            .unwrap();
        assert_eq!(kind, SignInClient::BuiltIn);
        let consent = db
            .read(move |c| mailrs_store::accounts::consent(c, id))
            .await
            .unwrap();
        assert_eq!(consent.granted.as_deref(), Some("gmail.modify"));
        assert_eq!(consent.asked.as_deref(), Some("gmail.modify settings"));
    }

    #[tokio::test]
    async fn a_google_sign_in_for_an_address_an_imap_account_holds_is_refused() {
        let (_dir, db) = store();
        db.write(|c| accounts::insert_imap_account(c, "dana@fastmail.com", "Fastmail", 0))
            .await
            .unwrap();
        let secrets = Secrets::memory();
        let refused = google_signed_in(
            &db,
            &secrets,
            "dana@fastmail.com",
            "refresh",
            0,
            None,
            "",
            None,
        )
        .await
        .expect_err("an IMAP account holds the address");
        assert_eq!(
            refused.to_string(),
            "dana@fastmail.com is already in Penguin Mail as a Fastmail account. To sign in another way, remove this account and add it again."
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
        let secrets = Secrets::memory();
        google_signed_in(
            &db,
            &secrets,
            "dana@fastmail.com",
            "refresh",
            0,
            None,
            "",
            Some(&own_client()),
        )
        .await
        .expect_err("an IMAP account holds the address");
        assert_eq!(secrets.google.load("dana@fastmail.com").unwrap(), None);
        assert_eq!(secrets.google_clients.load(1).unwrap(), None);

        google_signed_in(
            &db,
            &secrets,
            "dana@gmail.com",
            "refresh",
            0,
            None,
            "",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            secrets.google.load("dana@gmail.com").unwrap().as_deref(),
            Some("refresh")
        );
    }

    fn own_client() -> own_client::OwnClient {
        own_client::OwnClient::new("own.apps.googleusercontent.com", "GOCSPX-own")
    }

    async fn kind(db: &Db, id: mailrs_domain::AccountId) -> SignInClient {
        db.read(move |c| Ok(accounts::gmail_sign_in(c, id)?.client()))
            .await
            .unwrap()
    }

    fn kept_id(secrets: &Secrets, id: mailrs_domain::AccountId) -> Option<String> {
        let kept = secrets.google_clients.load(id).unwrap()?;
        own_client::OwnClient::decode(&kept).map(|own| own.oauth().id().to_string())
    }

    #[tokio::test]
    async fn a_sign_in_with_an_own_client_keeps_it_beside_the_token() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let own = own_client();
        let added = google_signed_in(
            &db,
            &secrets,
            "me@gmail.com",
            "refresh",
            0,
            None,
            "",
            Some(&own),
        )
        .await
        .unwrap();
        assert_eq!(kind(&db, added.id).await, SignInClient::Own);
        assert_eq!(
            kept_id(&secrets, added.id).as_deref(),
            Some("own.apps.googleusercontent.com")
        );
        assert_eq!(
            secrets.google.load("me@gmail.com").unwrap().as_deref(),
            Some("refresh")
        );
    }

    #[tokio::test]
    async fn signing_in_again_with_the_built_in_client_drops_the_kept_one() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let own = own_client();
        let first = google_signed_in(
            &db,
            &secrets,
            "me@gmail.com",
            "refresh",
            0,
            None,
            "",
            Some(&own),
        )
        .await
        .unwrap();
        let again = google_signed_in(
            &db,
            &secrets,
            "me@gmail.com",
            "refresh-2",
            1,
            None,
            "",
            None,
        )
        .await
        .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(kind(&db, again.id).await, SignInClient::BuiltIn);
        assert_eq!(kept_id(&secrets, again.id), None);
    }

    /// The keyring will not take the own client: the account goes on
    /// signing in as it did, with its client and its token.
    #[tokio::test]
    async fn a_sign_in_whose_own_client_the_keyring_refuses_changes_nothing() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let first = google_signed_in(&db, &secrets, "me@gmail.com", "refresh", 0, None, "", None)
            .await
            .unwrap();
        let crate::passwords::Passwords::Memory(clients) = &*secrets.google_clients else {
            unreachable!("tests keep secrets in memory")
        };
        clients.refuse_saves();
        let refused = google_signed_in(
            &db,
            &secrets,
            "me@gmail.com",
            "refresh-2",
            1,
            None,
            "",
            Some(&own_client()),
        )
        .await;
        assert!(matches!(refused, Err(SignInError::Token(_))), "{refused:?}");
        assert_eq!(kind(&db, first.id).await, SignInClient::BuiltIn);
        assert_eq!(
            secrets.google.load("me@gmail.com").unwrap().as_deref(),
            Some("refresh")
        );
    }

    #[tokio::test]
    async fn a_new_account_whose_own_client_the_keyring_refuses_is_not_kept() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let crate::passwords::Passwords::Memory(clients) = &*secrets.google_clients else {
            unreachable!("tests keep secrets in memory")
        };
        clients.refuse_saves();
        let refused = google_signed_in(
            &db,
            &secrets,
            "me@gmail.com",
            "refresh",
            0,
            None,
            "",
            Some(&own_client()),
        )
        .await;
        assert!(refused.is_err());
        assert!(
            db.read(|c| accounts::account_by_email(c, "me@gmail.com"))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(secrets.google.load("me@gmail.com").unwrap(), None);
    }

    #[tokio::test]
    async fn an_own_account_takes_its_kept_client_over_the_config_file() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let kept = own_client::OwnClient::new("kept.apps.googleusercontent.com", "GOCSPX-kept");
        secrets.google_clients.save(own.id, &kept.encode()).unwrap();
        let client = account_client(
            &db,
            &with_own(),
            built_in(),
            Arc::clone(&secrets.google_clients),
            &own,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.id(), "kept.apps.googleusercontent.com");
    }

    #[tokio::test]
    async fn a_built_in_account_passes_over_a_stray_kept_client() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let built = account(&db, "built@example.com", SignInClient::BuiltIn).await;
        secrets
            .google_clients
            .save(built.id, &own_client().encode())
            .unwrap();
        let client = account_client(
            &db,
            &Config::default(),
            built_in(),
            Arc::clone(&secrets.google_clients),
            &built,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.id(), "built.apps.googleusercontent.com");
    }

    #[tokio::test]
    async fn an_online_accounts_account_from_its_own_project_keeps_the_project() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let linked = account(&db, "goa@example.com", SignInClient::Own).await;
        let id = linked.id;
        db.write(move |c| {
            accounts::set_gmail_sign_in(
                c,
                id,
                &GmailSignIn::OnlineAccounts {
                    goa_id: "account_1".into(),
                },
            )
        })
        .await
        .unwrap();
        let kept = own_client::OwnClient::new("kept.apps.googleusercontent.com", "GOCSPX-kept");
        secrets.google_clients.save(id, &kept.encode()).unwrap();
        let client = account_client(
            &db,
            &Config::default(),
            built_in(),
            Arc::clone(&secrets.google_clients),
            &linked,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.id(), "kept.apps.googleusercontent.com");
        secrets.google_clients.delete(id).unwrap();
        let client = account_client(
            &db,
            &with_own(),
            built_in(),
            Arc::clone(&secrets.google_clients),
            &linked,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            client.id(),
            "built.apps.googleusercontent.com",
            "without a kept project it takes the built-in client, not config.toml's"
        );
    }

    /// The variables are never set in this test: the account's kept
    /// client alone decides which client the refresh goes out under.
    #[tokio::test]
    async fn a_refresh_goes_out_under_the_kept_client() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(
                "client_id=own.apps.googleusercontent.com",
            ))
            .and(body_string_contains("client_secret=GOCSPX-own"))
            .and(body_string_contains("refresh_token=rt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let added = google_signed_in(
            &db,
            &secrets,
            "me@gmail.com",
            "rt",
            0,
            None,
            "",
            Some(&own_client()),
        )
        .await
        .unwrap();
        let client = account_client(
            &db,
            &Config::default(),
            None,
            Arc::clone(&secrets.google_clients),
            &added,
        )
        .await
        .unwrap()
        .expect("the kept client serves a build without the built-in one")
        .with_endpoints(
            format!("{}/auth", server.uri()),
            format!("{}/token", server.uri()),
        );
        assert_eq!(client.refresh("rt").await.unwrap().token, "at");
    }

    /// A connector over `db` and `secrets` whose setup chose `chosen`.
    fn reach(db: &Db, secrets: &Secrets, chosen: Option<own_client::OwnClient>) -> Connector {
        Connector {
            db: db.clone(),
            config: Config::default(),
            clients: Clients::default().with_chosen(chosen),
            secrets: secrets.clone(),
            goa: crate::goa::session(),
        }
    }

    fn authorized(email: &str) -> mailrs_gmail::Authorized {
        mailrs_gmail::Authorized {
            email: email.into(),
            refresh_token: "rt".into(),
            granted: None,
        }
    }

    #[tokio::test]
    async fn a_setup_sign_in_keeps_the_account_the_project_and_refreshes_after_a_restart() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(
                "client_id=own.apps.googleusercontent.com",
            ))
            .and(body_string_contains("refresh_token=rt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let reach = reach(&db, &secrets, Some(own_client()));
        let choice = google_sign_in(&reach.clients, None).unwrap();
        assert_eq!(choice.source, ClientSource::Setup);
        let added = browser::signed_in_through(
            &reach,
            &choice,
            &Wanted::Anyone,
            authorized("me@gmail.com"),
        )
        .await
        .unwrap();
        assert_eq!(kind(&db, added.id).await, SignInClient::Own);
        assert_eq!(
            kept_id(&secrets, added.id).as_deref(),
            Some("own.apps.googleusercontent.com")
        );
        assert_eq!(
            own_client::saved_project(&secrets).await.unwrap().id(),
            "own.apps.googleusercontent.com"
        );
        // A restart reads the client from the keyring alone.
        let client = account_client(
            &db,
            &Config::default(),
            None,
            Arc::clone(&secrets.google_clients),
            &added,
        )
        .await
        .unwrap()
        .expect("the kept client serves a build without the built-in one")
        .with_endpoints(
            format!("{}/auth", server.uri()),
            format!("{}/token", server.uri()),
        );
        assert_eq!(client.refresh("rt").await.unwrap().token, "at");
    }

    #[tokio::test]
    async fn an_environment_sign_in_keeps_no_project() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let clients = Clients {
            own_google: Some(Ok(own_client())),
            ..Clients::default()
        };
        let choice = google_sign_in(&clients, None).unwrap();
        assert_eq!(choice.source, ClientSource::Environment);
        let added = browser::signed_in_through(
            &reach(&db, &secrets, None),
            &choice,
            &Wanted::Anyone,
            authorized("me@gmail.com"),
        )
        .await
        .unwrap();
        assert_eq!(kind(&db, added.id).await, SignInClient::Own);
        assert!(own_client::saved_project(&secrets).await.is_none());
    }

    #[tokio::test]
    async fn a_refused_sign_in_keeps_no_project() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        db.write(|c| accounts::insert_imap_account(c, "me@example.com", "Fastmail", 0))
            .await
            .unwrap();
        let reach = reach(&db, &secrets, Some(own_client()));
        let choice = google_sign_in(&reach.clients, None).unwrap();
        let refused = browser::signed_in_through(
            &reach,
            &choice,
            &Wanted::Anyone,
            authorized("me@example.com"),
        )
        .await;
        assert!(
            matches!(
                refused,
                Err(BrowserSignInError::Kept(SignInError::Taken { .. }))
            ),
            "{refused:?}"
        );
        assert!(own_client::saved_project(&secrets).await.is_none());
    }

    #[tokio::test]
    async fn a_sign_in_as_another_address_keeps_no_project() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let reach = reach(&db, &secrets, Some(own_client()));
        let choice = google_sign_in(&reach.clients, None).unwrap();
        let refused = browser::signed_in_through(
            &reach,
            &choice,
            &Wanted::Only("you@gmail.com".into()),
            authorized("me@gmail.com"),
        )
        .await;
        assert!(
            matches!(refused, Err(BrowserSignInError::OtherAddress { .. })),
            "{refused:?}"
        );
        assert!(own_client::saved_project(&secrets).await.is_none());
    }

    #[tokio::test]
    async fn the_kept_own_client_comes_from_the_keyring_then_the_config() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let kept = account(&db, "kept@gmail.com", SignInClient::Own).await;
        let kept_client = own_client::OwnClient::new("kept.apps.googleusercontent.com", "s");
        secrets
            .google_clients
            .save(kept.id, &kept_client.encode())
            .unwrap();
        account(&db, "config@gmail.com", SignInClient::Own).await;
        let built = account(&db, "built@gmail.com", SignInClient::BuiltIn).await;
        secrets
            .google_clients
            .save(built.id, &own_client().encode())
            .unwrap();
        account(&db, "gone@gmail.com", SignInClient::Own).await;
        let config = with_own();
        let found = |config: &'static Config, address: &'static str| {
            let (db, secrets) = (&db, &secrets);
            async move {
                match kept_own_client(db, config, secrets, address).await.unwrap() {
                    KeptClient::Found(own) => own.id().to_string(),
                    KeptClient::Missing => "missing".to_string(),
                    KeptClient::NotOwn => "not own".to_string(),
                }
            }
        };
        let config: &'static Config = Box::leak(Box::new(config));
        let none: &'static Config = Box::leak(Box::default());
        assert_eq!(
            found(config, "kept@gmail.com").await,
            "kept.apps.googleusercontent.com"
        );
        assert_eq!(
            found(config, "config@gmail.com").await,
            "own.apps.googleusercontent.com"
        );
        assert_eq!(found(none, "gone@gmail.com").await, "missing");
        assert_eq!(found(config, "built@gmail.com").await, "not own");
        assert_eq!(found(config, "nobody@gmail.com").await, "not own");
    }

    #[tokio::test]
    async fn an_imap_account_has_no_kept_own_client() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        db.write(|c| accounts::insert_imap_account(c, "me@example.com", "Fastmail", 0))
            .await
            .unwrap();
        let found = kept_own_client(&db, &with_own(), &secrets, "me@example.com")
            .await
            .unwrap();
        assert!(matches!(found, KeptClient::NotOwn));
    }

    #[tokio::test]
    async fn only_a_setup_choice_keeps_the_project() {
        let secrets = Secrets::memory();
        let env = Clients {
            own_google: Some(Ok(own_client())),
            ..Clients::default()
        };
        keep_chosen_project(&secrets, &google_sign_in(&env, None).unwrap()).await;
        assert!(own_client::saved_project(&secrets).await.is_none());
        let kept = own_client();
        let choice = google_sign_in(&Clients::default(), Some(&kept)).unwrap();
        keep_chosen_project(&secrets, &choice).await;
        assert!(own_client::saved_project(&secrets).await.is_none());
        let setup = Clients::default().with_chosen(Some(own_client()));
        keep_chosen_project(&secrets, &google_sign_in(&setup, None).unwrap()).await;
        assert!(own_client::saved_project(&secrets).await.is_some());
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
        let account = microsoft_signed_in(
            &db,
            Arc::clone(&tokens),
            new_microsoft("dana@outlook.com"),
            0,
            "openid mail.readwrite",
        )
        .await
        .unwrap();
        assert_eq!(account.provider, mailrs_domain::Provider::Microsoft);
        assert_eq!(
            tokens.load(account.id).unwrap().as_deref(),
            Some("refresh-1")
        );
        let id = account.id;
        let consent = db.read(move |c| accounts::consent(c, id)).await.unwrap();
        assert_eq!(consent.granted.as_deref(), Some("mail.readwrite openid"));
        assert_eq!(consent.asked.as_deref(), Some("openid mail.readwrite"));
    }

    #[tokio::test]
    async fn a_microsoft_sign_in_for_a_gmail_address_is_refused_and_keeps_no_token() {
        let (_dir, db) = store();
        db.write(|c| accounts::insert_account(c, "dana@contoso.com", 0))
            .await
            .unwrap();
        let tokens = Arc::new(crate::passwords::MemoryPasswords::default());
        let refused = microsoft_signed_in(
            &db,
            Arc::clone(&tokens),
            new_microsoft("dana@contoso.com"),
            0,
            "",
        )
        .await
        .expect_err("Gmail holds the address");
        assert_eq!(
            refused.to_string(),
            "dana@contoso.com is already in Penguin Mail as a Gmail account. To sign in another way, remove this account and add it again."
        );
        assert_eq!(tokens.load(1).unwrap(), None);
    }

    #[tokio::test]
    async fn signing_a_microsoft_account_in_again_ends_needs_sign_in() {
        let (_dir, db) = store();
        let tokens = Arc::new(crate::passwords::MemoryPasswords::default());
        let first = microsoft_signed_in(
            &db,
            Arc::clone(&tokens),
            new_microsoft("dana@outlook.com"),
            0,
            "",
        )
        .await
        .unwrap();
        let id = first.id;
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await
            .unwrap();
        let again = microsoft_signed_in(
            &db,
            Arc::clone(&tokens),
            NewMicrosoft {
                refresh_token: "refresh-2".into(),
                ..new_microsoft("dana@outlook.com")
            },
            1,
            "",
        )
        .await
        .unwrap();
        assert_eq!(again.id, id);
        assert_eq!(again.state, AccountState::Ok);
        assert_eq!(tokens.load(id).unwrap().as_deref(), Some("refresh-2"));
    }

    #[tokio::test]
    async fn a_setup_sign_in_as_an_account_on_an_app_password_leaves_it_on_imap() {
        let (_dir, db) = store();
        let secrets = Secrets::memory();
        let id = db
            .write(|c| {
                let id = accounts::insert_account(c, "me@gmail.com", 0)?;
                let password = GmailSignIn::AppPassword {
                    google: SignInClient::BuiltIn,
                };
                accounts::set_gmail_sign_in(c, id, &password)?;
                Ok(id)
            })
            .await
            .unwrap();
        let reach = reach(&db, &secrets, Some(own_client()));
        let choice = google_sign_in(&reach.clients, None).unwrap();

        let refused = browser::signed_in_through(
            &reach,
            &choice,
            &Wanted::Anyone,
            authorized("me@gmail.com"),
        )
        .await;

        assert!(
            matches!(refused, Err(BrowserSignInError::AlreadyHeld)),
            "{:?}",
            refused.err()
        );
        let sign_in = db
            .read(move |c| accounts::gmail_sign_in(c, id))
            .await
            .unwrap();
        assert_eq!(
            sign_in,
            GmailSignIn::AppPassword {
                google: SignInClient::BuiltIn
            }
        );
        assert_eq!(kept_id(&secrets, id), None);
        assert_eq!(secrets.google.load("me@gmail.com").unwrap(), None);
    }
}
