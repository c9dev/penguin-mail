use std::sync::Arc;
use std::future::Future;
use std::time::Duration;

use mailrs_dav::{DavClient, Kind as DavKind, Login as DavLogin};
use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::{Account, AccountId, AccountState, Provider};
use mailrs_gmail::{Granted, GmailClient, GmailError, OAuthClient, TokenStore};
use mailrs_graph::MicrosoftClient;
use mailrs_imap::{ImapClient, Login, SmtpClient};
use mailrs_pop3::Pop3Client;
use mailrs_sieve::client::{Login as SieveLogin, ManageSieveClient};
use mailrs_store::servers::{self, Pop3Servers, Saved, Servers};
use mailrs_store::services::{FoundService, ServiceKind};
use mailrs_store::{Db, accounts};

use crate::config::Config;
use crate::passwords::{PasswordError, PasswordStore, Secrets};
use crate::sign_in::account_client;
use crate::{
    AccountClient, AccountServices, AnyAutoReply, AnyCalendar, AnyContacts, AnyRules, BackendError,
    CalDav, CardDav, ImapSettings, LocalRules, MicrosoftSettings, Pop3Settings, SieveRules, SyncError,
};

/// How long starting an account waits on one step, such as a keyring
/// read. Twenty seconds is far past a keyring that answers, and short
/// enough that the account is reported and tried again soon after.
pub const STEP_WAIT: Duration = Duration::from_secs(20);

/// The step a keyring read names in `SyncError::NoAnswer`, which the app
/// reads to say the keyring is at fault rather than the provider.
pub const KEYRING: &str = "the keyring";

/// `work`, given up after `wait` with a log line that names `step` and
/// the account. Starting an account waits on the keyring and the store,
/// and a step that never answers would otherwise hold the account, with
/// nothing in the log to say where.
async fn bounded<T, E: Into<SyncError>>(
    account: AccountId,
    step: &'static str,
    wait: Duration,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, SyncError> {
    match tokio::time::timeout(wait, work).await {
        Ok(done) => done.map_err(Into::into),
        Err(_) => {
            tracing::warn!(account, step, waited_secs = wait.as_secs_f32(), "a step of starting the account did not answer");
            Err(SyncError::NoAnswer(step))
        }
    }
}

/// A secret read from `store` off the runtime, given up after `wait`.
/// The keyring can wait without end: its Secret Service client holds one
/// lock for the whole process while an unlock prompt is on screen, so a
/// prompt nobody answers holds every later read too. The blocking thread
/// stays behind until the keyring answers; the account does not.
async fn read_secret<P: PasswordStore + 'static>(
    store: Arc<P>,
    account: AccountId,
    wait: Duration,
) -> Result<Option<String>, SyncError> {
    let read = async move {
        tokio::task::spawn_blocking(move || store.load(account))
            .await
            .map_err(|err| PasswordError::Keyring(err.to_string()))?
    };
    bounded(account, KEYRING, wait, read).await
}

/// The build's own sign-in clients. A copy built from source without the
/// release values has neither, and its Google and Microsoft accounts
/// cannot refresh their tokens.
#[derive(Clone, Default)]
pub struct Clients {
    pub google: Option<OAuthClient>,
    pub microsoft: Option<MicrosoftClient>,
}

impl Clients {
    /// The clients compiled into this build.
    pub fn built_in() -> Self {
        Clients {
            google: mailrs_gmail::built_in_client(),
            microsoft: mailrs_graph::built_in_client(),
        }
    }
}

/// What connecting an account came to, short of an error.
// One of these is made per account start and moved to the engine at once,
// so the unboxed services cost nothing worth a box.
#[expect(clippy::large_enum_variant)]
pub enum Connected {
    /// The services to run the account's sync on.
    Ready(AccountServices),
    /// Only a new sign-in helps. The store already says so for every
    /// account but a Google one with no token, which the caller records.
    NeedsSignIn(Lacks),
}

/// What an account that needs a new sign-in lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lacks {
    /// The client it signed in with: this build has none, or an own
    /// Google client left `config.toml`.
    Client,
    /// Its refresh token or password, or an IMAP or POP3 account's servers.
    Secret,
}

/// What connecting any account needs besides the account: the store, the
/// settings, the build's clients and where secrets live. The app and the
/// CLI both connect and forget accounts through it, so a new provider
/// adds its case here.
#[derive(Clone)]
pub struct Connector {
    pub db: Db,
    pub config: Config,
    pub clients: Clients,
    pub secrets: Secrets,
}

impl Connector {
    /// The account's services, or what it lacks to have them. Nothing
    /// here talks to a server; the clients log in when sync first asks.
    pub async fn connect(&self, account: &Account) -> Result<Connected, SyncError> {
        let db = &self.db;
        let window_days = self.config.engine_config().window_days;
        let started = match account.provider {
            Provider::Gmail => {
                let Some(oauth) = account_client(db, &self.config, self.clients.google.clone(), account).await? else {
                    tracing::warn!(account = %account.email, "no Google client for this account");
                    return Ok(Connected::NeedsSignIn(Lacks::Client));
                };
                connect_account(oauth, Arc::clone(&self.secrets.google), account, db)
                    .await
                    .map(AccountServices::google)
            }
            Provider::Imap => connect_imap(db, Arc::clone(&self.secrets.passwords), account, window_days).await,
            Provider::Pop3 => connect_pop3(db, Arc::clone(&self.secrets.passwords), account).await,
            Provider::Microsoft => {
                let Some(client) = self.clients.microsoft.clone() else {
                    tracing::warn!(account = %account.email, "no Microsoft client in this build");
                    return Ok(Connected::NeedsSignIn(Lacks::Client));
                };
                connect_microsoft(db, Arc::clone(&self.secrets.microsoft), client, account, window_days).await
            }
        };
        match started {
            Ok(services) => Ok(Connected::Ready(services)),
            Err(SyncError::Backend(BackendError::NeedsReauth)) => Ok(Connected::NeedsSignIn(Lacks::Secret)),
            Err(err) => Err(err),
        }
    }

    /// Deletes what signs `account` in. See [`Secrets::forget`].
    pub async fn forget(&self, account: &Account) -> Result<(), PasswordError> {
        self.secrets.forget(account).await
    }
}

/// A Gmail client for `account`, built from its refresh token in `tokens`
/// and seeded with the scopes `db` last recorded for it. Fails with
/// `NeedsReauth` when no token is stored. A later refresh that reports a
/// different set of scopes saves them back to `db` in the background,
/// off the runtime a caller is waiting on.
pub async fn connect_account(
    oauth: OAuthClient,
    tokens: Arc<dyn TokenStore>,
    account: &Account,
    db: &Db,
) -> Result<AccountClient, SyncError> {
    connect_account_within(oauth, tokens, account, db, STEP_WAIT).await
}

pub(crate) async fn connect_account_within(
    oauth: OAuthClient,
    tokens: Arc<dyn TokenStore>,
    account: &Account,
    db: &Db,
    wait: Duration,
) -> Result<AccountClient, SyncError> {
    let (id, email) = (account.id, account.email.clone());
    // The same keyring as `read_secret`, under Google's own key.
    let read = async move {
        tokio::task::spawn_blocking(move || tokens.load(&email))
            .await
            .map_err(|e| GmailError::Keyring(e.to_string()))?
    };
    let stored = bounded(id, KEYRING, wait, read).await?;
    let refresh_token = stored.ok_or(GmailError::NeedsReauth)?;
    let consent = bounded(id, "the mail store", wait, db.read(move |c| accounts::consent(c, id))).await?;
    let granted = consent.granted.as_deref().map(Granted::parse);
    let db = db.clone();
    let client = GmailClient::for_account(oauth, refresh_token, &account.email)
        .with_granted(granted)
        .on_granted(move |granted| {
            let scope = granted.to_scope();
            let db = db.clone();
            tokio::spawn(async move {
                if let Err(err) = db.write(move |c| accounts::set_granted(c, id, &scope)).await {
                    tracing::warn!(account = id, %err, "could not save the granted scopes");
                }
            });
        });
    Ok(AccountClient {
        account_id: account.id,
        address: account.email.clone(),
        client,
    })
}

/// The services for a Microsoft account, from the refresh token under its
/// id and the scopes the store last recorded. No token means the account
/// needs to sign in again, which the store records before this answers.
/// Each token Microsoft rotates goes back to the keyring off the runtime,
/// and each change of scopes back to the store.
pub async fn connect_microsoft<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    account: &Account,
    window_days: i64,
) -> Result<AccountServices, SyncError> {
    connect_microsoft_at(db, tokens, client, account, window_days, mailrs_graph::GRAPH_BASE).await
}

/// [`connect_microsoft`] against `base`, which tests point at a mock.
pub async fn connect_microsoft_at<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    account: &Account,
    window_days: i64,
    base: &str,
) -> Result<AccountServices, SyncError> {
    connect_microsoft_within(db, tokens, client, account, window_days, base, STEP_WAIT).await
}

pub(crate) async fn connect_microsoft_within<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    account: &Account,
    window_days: i64,
    base: &str,
    wait: Duration,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let Some(refresh) = read_secret(Arc::clone(&tokens), id, wait).await? else {
        let marked = db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth));
        bounded(id, "the mail store", wait, marked).await?;
        return Err(BackendError::NeedsReauth.into());
    };
    let consent = bounded(id, "the mail store", wait, db.read(move |c| accounts::consent(c, id))).await?;
    let granted = consent.granted.as_deref().map(mailrs_graph::Granted::parse);
    let saver = Arc::clone(&tokens);
    let store = db.clone();
    let session = mailrs_graph::Session::new(client, refresh)
        .with_granted(granted)
        .on_rotated(move |token| {
            let saver = Arc::clone(&saver);
            tokio::spawn(async move {
                let saved = tokio::task::spawn_blocking(move || saver.save(id, &token)).await;
                if !matches!(saved, Ok(Ok(()))) {
                    tracing::warn!(account = id, "could not keep Microsoft's new refresh token");
                }
            });
        })
        .on_granted(move |granted| {
            let (store, scope) = (store.clone(), granted.to_scope());
            tokio::spawn(async move {
                if let Err(err) = store.write(move |c| accounts::set_granted(c, id, &scope)).await {
                    tracing::warn!(account = id, %err, "could not save the granted scopes");
                }
            });
        });
    let graph = mailrs_graph::Graph::with_base(Arc::new(session), base)
        .map_err(crate::services::microsoft::backend)?;
    Ok(AccountServices::microsoft(
        graph,
        MicrosoftSettings {
            address: account.email.clone(),
            provider_name: account.provider_name().to_string(),
            window_days,
        },
    ))
}

/// The services for an IMAP account, from the servers the store keeps
/// for it and the password in `passwords`. Without either, the account
/// needs to sign in again: the store records that before this answers
/// `NeedsReauth`, since the engine never runs the account to say so.
/// It adds the calendar, contacts and rules servers found for the account,
/// each only once confirmed; an account whose server runs no rules keeps
/// them on this computer. Nothing here connects; the clients log in when
/// sync first asks. The
/// provider's sent-copy rule comes from the provider table at each start,
/// so a corrected table reaches accounts added before the correction.
pub async fn connect_imap<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
    window_days: i64,
) -> Result<AccountServices, SyncError> {
    connect_imap_within(db, passwords, account, window_days, STEP_WAIT).await
}

pub(crate) async fn connect_imap_within<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
    window_days: i64,
    wait: Duration,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let saved = bounded(id, "the mail store", wait, db.read(move |c| servers::load(c, id))).await?;
    let password = read_secret(passwords, id, wait).await?;
    let (Some(saved), Some(password)) = (saved, password) else {
        let marked = db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth));
        bounded(id, "the mail store", wait, marked).await?;
        return Err(BackendError::NeedsReauth.into());
    };
    // A saved account's provider_name may be a domain "Set up manually"
    // guessed before it consulted the table; resolve that back to the
    // table's own name first, so a listed provider is still found.
    let provider_name = mailrs_discover::resolved_provider_name(account.provider_name());
    // A custom server is not in the provider table, and Sent then gets a
    // copy of each message from the app, which is right for a server
    // nobody has checked.
    let files_sent_mail = mailrs_discover::provider_named(&provider_name)
        .is_some_and(|provider| provider.files_sent_mail);
    let imap = ImapClient::new(
        server_of(&saved.imap),
        Login::new(saved.imap.user_name.as_str(), password.as_str()),
    );
    // Building the SMTP client refuses only a host name lettre cannot
    // use, which a saved server never has; the error still reaches the
    // log through the engine rather than a panic.
    let secret = password.clone();
    let smtp = SmtpClient::new(
        &server_of(&saved.smtp),
        &Login::new(saved.smtp.user_name.as_str(), password),
    )
    .map_err(BackendError::from)?;
    let mut services = AccountServices::imap(
        imap,
        smtp,
        ImapSettings {
            address: account.email.clone(),
            provider_name: provider_name.clone(),
            files_sent_mail,
            window_days,
        },
    );
    let found = bounded(id, "the mail store", wait, db.read(move |c| mailrs_store::services::load(c, id))).await?;
    // The password goes only to a server the person confirmed.
    for service in found.into_iter().filter(|f| f.confirmed) {
        services = attach(services, &service, &secret, account, &provider_name);
    }
    if services.rules.is_none() {
        services = services.with_rules(AnyRules::Local(LocalRules::new(db.clone(), id)));
    }
    Ok(services)
}

/// The services for a POP3 account, from the servers the store keeps for
/// it and the password in `passwords`. Without either, the store records
/// that the account needs to sign in again before this answers
/// `NeedsReauth`. Its rules stay on this computer; a confirmed CalDAV or
/// CardDAV server found for it serves its calendar and contacts. A Sieve
/// row is passed over: POP3 mail never reaches server rules. Nothing here
/// connects.
pub async fn connect_pop3<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
) -> Result<AccountServices, SyncError> {
    connect_pop3_within(db, passwords, account, STEP_WAIT).await
}

pub(crate) async fn connect_pop3_within<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
    wait: Duration,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let saved = bounded(id, "the mail store", wait, db.read(move |c| servers::load_pop3(c, id))).await?;
    let password = read_secret(passwords, id, wait).await?;
    let (Some(saved), Some(password)) = (saved, password) else {
        let marked = db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth));
        bounded(id, "the mail store", wait, marked).await?;
        return Err(BackendError::NeedsReauth.into());
    };
    let provider_name = mailrs_discover::resolved_provider_name(account.provider_name());
    let client = Pop3Client::new(
        &server_of(&saved.pop3),
        mailrs_pop3::Login::new(saved.pop3.user_name.as_str(), password.as_str()),
    );
    let secret = password.clone();
    let smtp = SmtpClient::new(
        &server_of(&saved.smtp),
        &Login::new(saved.smtp.user_name.as_str(), password),
    )
    .map_err(BackendError::from)?;
    let mut services = AccountServices::pop3(
        db.clone(),
        id,
        client,
        smtp,
        Pop3Settings {
            address: account.email.clone(),
            provider_name,
        },
    );
    let found = bounded(id, "the mail store", wait, db.read(move |c| mailrs_store::services::load(c, id))).await?;
    // The password goes only to a server the person confirmed.
    for service in found.into_iter().filter(|f| f.confirmed) {
        services = attach_pop3(services, &service, &secret, account);
    }
    Ok(services)
}

/// `services` with the CalDAV or CardDAV server `service` names put behind
/// it. A server that cannot be set up is logged and left out, so the
/// account keeps its mail.
fn attach_pop3(
    services: AccountServices,
    service: &FoundService,
    secret: &str,
    account: &Account,
) -> AccountServices {
    let id = account.id;
    let kind = match service.kind {
        ServiceKind::CalDav => DavKind::Calendar,
        ServiceKind::CardDav => DavKind::AddressBook,
        ServiceKind::Sieve => return services,
    };
    let client = match DavClient::new(&service.url, kind, DavLogin::new(&service.user_name, secret)) {
        Ok(client) => Arc::new(client),
        Err(err) => {
            tracing::warn!(account = id, %err, "could not set up a calendar or contacts server");
            return services;
        }
    };
    match service.kind {
        ServiceKind::CalDav => match services.pop3_adapter() {
            Some(mail) => {
                let calendar = CalDav::new(client, mail, vec![account.email.clone()]);
                services.with_calendar(AnyCalendar::Pop3Dav(calendar))
            }
            None => services,
        },
        ServiceKind::CardDav => services.with_contacts(AnyContacts::Dav(CardDav::new(client))),
        ServiceKind::Sieve => services,
    }
}

/// `services` with the server `service` names put behind the service it
/// offers. A server that cannot be set up is logged and left out, so the
/// account keeps its mail.
fn attach(
    services: AccountServices,
    service: &FoundService,
    secret: &str,
    account: &Account,
    provider_name: &str,
) -> AccountServices {
    let id = account.id;
    match service.kind {
        ServiceKind::CalDav => {
            let Some(mail) = services.imap_adapter() else {
                return services;
            };
            match DavClient::new(&service.url, DavKind::Calendar, DavLogin::new(&service.user_name, secret)) {
                Ok(client) => {
                    let calendar = CalDav::new(Arc::new(client), mail, vec![account.email.clone()]);
                    services.with_calendar(AnyCalendar::Dav(calendar))
                }
                Err(err) => {
                    tracing::warn!(account = id, %err, "could not set up the calendar server");
                    services
                }
            }
        }
        ServiceKind::CardDav => {
            match DavClient::new(&service.url, DavKind::AddressBook, DavLogin::new(&service.user_name, secret)) {
                Ok(client) => services.with_contacts(AnyContacts::Dav(CardDav::new(Arc::new(client)))),
                Err(err) => {
                    tracing::warn!(account = id, %err, "could not set up the contacts server");
                    services
                }
            }
        }
        ServiceKind::Sieve => {
            let Some(mail) = services.imap_adapter() else {
                return services;
            };
            let (host, port) = match service.url.rsplit_once(':') {
                Some((host, port)) => (host, port.parse().unwrap_or(mailrs_sieve::PORT)),
                None => (service.url.as_str(), mailrs_sieve::PORT),
            };
            let client = ManageSieveClient::new(host, port, SieveLogin::new(&service.user_name, secret));
            let rules = SieveRules::new(
                Arc::new(client),
                mail,
                account.email.clone(),
                provider_name.to_string(),
            );
            services.with_rules(AnyRules::Sieve(rules.clone())).with_auto_reply(AnyAutoReply::Sieve(rules))
        }
    }
}

/// The servers to keep for an account that logged in as `imap_user` on
/// `imap` and as `smtp_user` on `smtp`. The two can differ: iCloud's IMAP
/// server takes the part before @ and its SMTP server the whole address.
/// The store keeps the user name that worked rather than the rule that
/// found it, so the next start sends the same one.
pub fn servers_for(imap: &Server, imap_user: &str, smtp: &Server, smtp_user: &str) -> Servers {
    let saved = |server: &Server, user: &str| Saved {
        host: server.host.clone(),
        port: server.port,
        security: match server.security {
            Security::Tls => servers::Security::Tls,
            Security::StartTls => servers::Security::StartTls,
        },
        user_name: user.to_string(),
    };
    Servers {
        imap: saved(imap, imap_user),
        smtp: saved(smtp, smtp_user),
    }
}

/// The servers to keep for a POP3 account that logged in as `pop3_user`
/// and `smtp_user`.
pub fn pop3_servers_for(pop3: &Server, pop3_user: &str, smtp: &Server, smtp_user: &str) -> Pop3Servers {
    let both = servers_for(pop3, pop3_user, smtp, smtp_user);
    Pop3Servers {
        pop3: both.imap,
        smtp: both.smtp,
    }
}

/// A server as the clients take it, from what the store keeps. The login
/// carries the user name, so the rule for finding one no longer matters.
pub fn server_of(saved: &Saved) -> Server {
    Server {
        host: saved.host.clone(),
        port: saved.port,
        security: match saved.security {
            servers::Security::Tls => Security::Tls,
            servers::Security::StartTls => Security::StartTls,
        },
        user_name: UserName::Address,
    }
}
