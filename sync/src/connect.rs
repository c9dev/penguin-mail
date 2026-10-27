pub(crate) mod gmail_route;
mod storage;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use mailrs_dav::{DavClient, Kind as DavKind, Login as DavLogin};
use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::{Account, AccountId, AccountState, GmailSignIn, MailRoute, Provider};
use mailrs_gmail::{GmailClient, GmailError, Granted, OAuthClient, TokenStore};
use mailrs_graph::MicrosoftClient;
use mailrs_imap::{ImapClient, Login, SmtpClient};
use mailrs_jmap::oauth::OAuthSession;
use mailrs_pop3::Pop3Client;
use mailrs_store::servers::{self, Pop3Servers, Saved, Servers};
use mailrs_store::services::{FoundService, Miss, ServiceKind};
use mailrs_store::{Db, accounts};

use crate::config::Config;
use crate::passwords::{PasswordError, PasswordStore, Secrets};
use crate::services::{Beside, FoundServer};
use crate::sign_in::account_client;
use crate::sign_in::own_client::{self, OwnClient, OwnClientError};
use crate::{
    AccountClient, AccountServices, BackendError, ImapSettings, JmapSettings, LocalRules,
    MicrosoftSettings, Pop3Settings, SyncError,
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
            tracing::warn!(
                account,
                step,
                waited_secs = wait.as_secs_f32(),
                "a step of starting the account did not answer"
            );
            Err(SyncError::NoAnswer(step))
        }
    }
}

/// A secret read from `store` off the runtime, given up after `wait`.
/// The keyring can wait without end: its Secret Service client holds one
/// lock for the whole process while an unlock prompt is on screen, so a
/// prompt nobody answers holds every later read too. The blocking thread
/// stays behind until the keyring answers; the account does not.
pub(crate) async fn read_secret<P: PasswordStore + 'static>(
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

/// An IMAP or POP3 account's incoming password and, when it keeps one,
/// its outgoing server's own, read together under one wait as
/// [`read_secret`] reads one.
async fn read_passwords<P: PasswordStore + 'static>(
    store: Arc<P>,
    account: AccountId,
    wait: Duration,
) -> Result<(Option<String>, Option<String>), SyncError> {
    let read = async move {
        tokio::task::spawn_blocking(move || {
            Ok::<_, PasswordError>((store.load(account)?, store.load_outgoing(account)?))
        })
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))?
    };
    bounded(account, KEYRING, wait, read).await
}

/// What the outgoing server signs in with: its user name, and its own
/// password when the account keeps one, else the incoming `password`.
pub fn smtp_login(user: &str, password: String, outgoing: Option<String>) -> Login {
    Login::new(user, outgoing.unwrap_or(password))
}

/// The build's own sign-in clients, the own Google client the environment
/// names, and the one the person's setup chose. A Google sign-in takes the
/// environment's client first, then the setup's, then the one an account
/// signing in again kept, then the build's. A copy built from source
/// without the release values has neither built-in client, and its Google
/// and Microsoft accounts cannot refresh their tokens unless they signed in
/// with an own client.
#[derive(Clone, Default)]
pub struct Clients {
    pub google: Option<OAuthClient>,
    pub microsoft: Option<MicrosoftClient>,
    /// The own Google client the environment names, which wins over every
    /// other, or why the environment does not name one.
    pub own_google: Option<Result<OwnClient, OwnClientError>>,
    /// The own Google client the person's setup chose for this sign-in.
    pub chosen_google: Option<OwnClient>,
}

impl Clients {
    /// The clients compiled into this build, with no own client.
    pub fn built_in() -> Self {
        Clients {
            google: mailrs_gmail::built_in_client(),
            microsoft: mailrs_graph::built_in_client(),
            own_google: None,
            chosen_google: None,
        }
    }

    /// The clients compiled into this build, and the own Google client the
    /// environment names now. Read again for each sign-in, so the
    /// variables need not be set when Penguin Mail starts.
    pub fn current() -> Self {
        Clients {
            own_google: own_client::from_env(),
            ..Clients::built_in()
        }
    }

    /// These clients with `own` as the one the setup chose.
    pub fn with_chosen(self, own: Option<OwnClient>) -> Clients {
        Clients {
            chosen_google: own,
            ..self
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
    /// Google client left the keyring or `config.toml`.
    Client,
    /// Its refresh token or password, or an IMAP or POP3 account's
    /// servers, or a calendar account's server.
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
    /// The desktop's Online Accounts, which signs a Gmail account in for
    /// its mail when the account was linked through it.
    pub goa: Arc<dyn crate::goa::OnlineAccounts>,
}

impl Connector {
    /// The account's services, or what it lacks to have them. Nothing
    /// here talks to a server; the clients log in when sync first asks.
    pub async fn connect(&self, account: &Account) -> Result<Connected, SyncError> {
        let db = &self.db;
        let window_days = self.config.engine_config().window_days;
        let started = match account.provider {
            Provider::Gmail => match self.connect_gmail(account).await? {
                Ok(services) => Ok(services),
                Err(lacks) => return Ok(Connected::NeedsSignIn(lacks)),
            },
            Provider::Imap => {
                connect_imap(
                    db,
                    Arc::clone(&self.secrets.passwords),
                    account,
                    window_days,
                )
                .await
            }
            Provider::Pop3 => connect_pop3(db, Arc::clone(&self.secrets.passwords), account).await,
            Provider::Jmap => connect_jmap(db, &self.secrets, account, window_days).await,
            Provider::CalDav => {
                connect_caldav(db, Arc::clone(&self.secrets.passwords), account).await
            }
            Provider::Microsoft => {
                let Some(client) = self.clients.microsoft.clone() else {
                    tracing::warn!(account = account.id, "no Microsoft client in this build");
                    return Ok(Connected::NeedsSignIn(Lacks::Client));
                };
                connect_microsoft(
                    db,
                    Arc::clone(&self.secrets.microsoft),
                    client,
                    account,
                    window_days,
                )
                .await
            }
        };
        match started {
            Ok(services) => Ok(Connected::Ready(services)),
            Err(SyncError::Backend(BackendError::NeedsReauth)) => {
                Ok(Connected::NeedsSignIn(Lacks::Secret))
            }
            Err(err) => Err(err),
        }
    }

    /// A Gmail account's services, built by where its mail comes from.
    /// `Err` names what the account lacks to have them.
    async fn connect_gmail(
        &self,
        account: &Account,
    ) -> Result<Result<AccountServices, Lacks>, SyncError> {
        let id = account.id;
        let sign_in = self
            .db
            .read(move |c| accounts::gmail_sign_in(c, id))
            .await?;
        if let GmailSignIn::OnlineAccounts { goa_id } = sign_in {
            // Boxed: the Online Accounts path would make this future large
            // enough to overflow a test thread's stack.
            return Box::pin(gmail_route::goa_services(self, account, goa_id)).await;
        }
        match sign_in.route() {
            MailRoute::GmailApi => {
                let kept = Arc::clone(&self.secrets.google_clients);
                let Some(oauth) = account_client(
                    &self.db,
                    &self.config,
                    self.clients.google.clone(),
                    kept,
                    account,
                )
                .await?
                else {
                    tracing::warn!(account = account.id, "no Google client for this account");
                    return Ok(Err(Lacks::Client));
                };
                match connect_account(oauth, Arc::clone(&self.secrets.google), account, &self.db)
                    .await
                {
                    Ok(client) => Ok(Ok(AccountServices::google(client))),
                    Err(SyncError::Backend(BackendError::NeedsReauth)) => Ok(Err(Lacks::Secret)),
                    Err(err) => Err(err),
                }
            }
            MailRoute::Imap => gmail_route::connect_imap_route(self, account).await,
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
    let consent = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| accounts::consent(c, id)),
    )
    .await?;
    let granted = consent.granted.as_deref().map(Granted::parse);
    let db = db.clone();
    let client = GmailClient::for_account(oauth, refresh_token, &account.email)
        .with_granted(granted)
        .on_granted(move |granted| {
            let scope = granted.to_scope();
            let db = db.clone();
            tokio::spawn(async move {
                if let Err(err) = db
                    .write(move |c| accounts::set_granted(c, id, &scope))
                    .await
                {
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
    connect_microsoft_at(
        db,
        tokens,
        client,
        account,
        window_days,
        mailrs_graph::GRAPH_BASE,
    )
    .await
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
    let consent = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| accounts::consent(c, id)),
    )
    .await?;
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
                if let Err(err) = store
                    .write(move |c| accounts::set_granted(c, id, &scope))
                    .await
                {
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
    let saved = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| servers::load(c, id)),
    )
    .await?;
    let (password, outgoing) = read_passwords(passwords, id, wait).await?;
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
        &smtp_login(&saved.smtp.user_name, password, outgoing),
    )
    .map_err(BackendError::from)?;
    let services = AccountServices::imap(
        imap,
        smtp,
        ImapSettings {
            address: account.email.clone(),
            provider_name: provider_name.clone(),
            files_sent_mail,
            window_days,
        },
    );
    let found = confirmed(db, id, &secret, wait).await?;
    let beside = Beside {
        account_id: id,
        address: account.email.clone(),
        provider_name,
        found,
        local: Some(LocalRules::new(db.clone(), id)),
        local_kept: false,
    };
    Ok(services.with_beside(beside))
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
    let saved = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| servers::load_pop3(c, id)),
    )
    .await?;
    let (password, outgoing) = read_passwords(passwords, id, wait).await?;
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
        &smtp_login(&saved.smtp.user_name, password, outgoing),
    )
    .map_err(BackendError::from)?;
    let services = AccountServices::pop3(
        db.clone(),
        id,
        client,
        smtp,
        Pop3Settings {
            address: account.email.clone(),
            provider_name: provider_name.clone(),
        },
    );
    // Its rules are kept on this computer from the start.
    let found = confirmed(db, id, &secret, wait).await?;
    let beside = Beside {
        account_id: id,
        address: account.email.clone(),
        provider_name,
        found,
        local: None,
        local_kept: false,
    };
    Ok(services.with_beside(beside))
}

/// The servers found for the account that the person confirmed, each to
/// sign in with `password`, which goes to no other server.
async fn confirmed(
    db: &Db,
    id: AccountId,
    password: &str,
    wait: Duration,
) -> Result<Vec<FoundServer>, SyncError> {
    let found = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| mailrs_store::services::load(c, id)),
    )
    .await?;
    Ok(found
        .into_iter()
        .filter(|f| f.confirmed)
        .map(|service| FoundServer {
            service,
            secret: password.to_string(),
            bearer: None,
        })
        .collect())
}

/// The credentials a saved JMAP account signs in with, for a password or
/// a token. An OAuth row is refused: its secret is the refresh token,
/// which only the token endpoint may see. It signs in through its
/// [`OAuthSession`] instead.
pub fn auth_of(
    saved: &mailrs_store::jmap::JmapSaved,
    address: &str,
    secret: String,
) -> Result<mailrs_jmap::Auth, BackendError> {
    match saved.auth {
        mailrs_store::jmap::JmapAuth::Bearer => Ok(mailrs_jmap::Auth::Bearer(secret)),
        mailrs_store::jmap::JmapAuth::Basic => Ok(mailrs_jmap::Auth::Basic {
            user: saved
                .user_name
                .clone()
                .unwrap_or_else(|| address.to_string()),
            password: secret,
        }),
        mailrs_store::jmap::JmapAuth::OAuth => Err(BackendError::NeedsReauth),
    }
}

/// The services for a calendar account, from the server the store keeps
/// for it and the password in `passwords`: a calendar and no mail.
/// Without either, the store records that the account needs to sign in
/// again before this answers `NeedsReauth`, since no loop runs to say so.
/// The server tells the adapter which addresses the account goes by, so
/// none is handed in. Nothing here connects.
pub async fn connect_caldav<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
) -> Result<AccountServices, SyncError> {
    connect_caldav_within(db, passwords, account, STEP_WAIT).await
}

pub(crate) async fn connect_caldav_within<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
    wait: Duration,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let found = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| mailrs_store::services::load(c, id)),
    )
    .await?;
    let server = found
        .into_iter()
        .find(|f| f.kind == ServiceKind::CalDav && f.confirmed);
    let password = read_secret(passwords, id, wait).await?;
    let (Some(server), Some(password)) = (server, password) else {
        let marked = db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth));
        bounded(id, "the mail store", wait, marked).await?;
        return Err(BackendError::NeedsReauth.into());
    };
    let client = DavClient::new(
        &server.url,
        DavKind::Calendar,
        DavLogin::new(&server.user_name, password),
    )
    .map_err(|err| BackendError::Refused(err.to_string()))?;
    Ok(AccountServices::calendar_only(client))
}

/// This run's OAuth sessions, one per JMAP account, so a restart of the
/// account reuses the tokens the last start holds. Two sessions with one
/// refresh token could each rotate it, and Fastmail revokes a grant whose
/// old refresh token comes back. Clones share them.
#[derive(Clone, Default)]
pub struct JmapSessions(Arc<std::sync::Mutex<SessionsHeld>>);

#[derive(Default)]
struct SessionsHeld {
    sessions: std::collections::HashMap<AccountId, HeldSession>,
}

struct HeldSession {
    /// The session URL and the client the session was built for.
    made_for: (String, mailrs_store::jmap::SavedOAuth),
    session: Arc<OAuthSession>,
}

impl JmapSessions {
    fn held_state(&self) -> std::sync::MutexGuard<'_, SessionsHeld> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The account's session for `client` at `session_url`, made with
    /// `make` when it has none, or one made for another client.
    fn session_for(
        &self,
        id: AccountId,
        session_url: &str,
        client: &mailrs_store::jmap::SavedOAuth,
        make: impl FnOnce() -> OAuthSession,
    ) -> Arc<OAuthSession> {
        let made_for = (session_url.to_string(), client.clone());
        let mut held = self.held_state();
        if let Some(kept) = held.sessions.get(&id)
            && kept.made_for == made_for
        {
            return Arc::clone(&kept.session);
        }
        let session = Arc::new(make());
        held.sessions.insert(
            id,
            HeldSession {
                made_for,
                session: Arc::clone(&session),
            },
        );
        session
    }

    /// The session the account holds now, if any.
    pub fn held(&self, id: AccountId) -> Option<Arc<OAuthSession>> {
        self.held_state()
            .sessions
            .get(&id)
            .map(|kept| Arc::clone(&kept.session))
    }

    /// Drops the account's session before a new sign-in keeps its refresh
    /// token, or as the account goes, and retires it: a task still holding
    /// it saves nothing more over the new token or under a gone account.
    /// Answers the newest refresh token the session held.
    pub async fn forget(&self, id: AccountId) -> Option<String> {
        let kept = self.held_state().sessions.remove(&id);
        match kept {
            Some(kept) => Some(kept.session.retire().await),
            None => None,
        }
    }
}

/// What revoking a browser sign-in the app no longer keeps needs: a
/// removed account's, or one that reached someone else's account. No
/// `Debug`: it holds the refresh token.
pub struct JmapGrant {
    pub(crate) session_url: String,
    pub(crate) client_id: String,
    pub(crate) refresh_token: String,
}

/// Where a browser account's grant was made, read from its row before the
/// row goes. No `Debug`: it holds the client id.
pub struct JmapGrantRow {
    id: AccountId,
    session_url: String,
    client_id: String,
}

/// The grant of `account` from its row, or `None` for an account that
/// signs in with a password or a token. Read before the row is deleted;
/// it retires nothing, so an account whose row stays keeps running.
pub async fn jmap_grant(db: &Db, account: &Account) -> Option<JmapGrantRow> {
    if account.provider != Provider::Jmap {
        return None;
    }
    let id = account.id;
    let saved = db
        .read(move |c| mailrs_store::jmap::load(c, id))
        .await
        .ok()??;
    let client = saved
        .oauth
        .filter(|_| saved.auth == mailrs_store::jmap::JmapAuth::OAuth)?;
    Some(JmapGrantRow {
        id,
        session_url: saved.session_url,
        client_id: client.client_id,
    })
}

impl JmapGrantRow {
    /// Once the account's row is gone: retires its session, so the token is
    /// the newest one it held and nothing saves a later one, and answers
    /// the grant to revoke. Without a session, the keyring's token.
    pub async fn retire(self, secrets: &Secrets) -> Option<JmapGrant> {
        let id = self.id;
        let held = secrets
            .jmap_sessions
            .forget(id)
            .await
            .filter(|token| !token.is_empty());
        let refresh_token = match held {
            Some(token) => token,
            None => read_secret(Arc::clone(&secrets.jmap), id, STEP_WAIT)
                .await
                .ok()??,
        };
        Some(JmapGrant {
            session_url: self.session_url,
            client_id: self.client_id,
            refresh_token,
        })
    }
}

impl JmapGrant {
    /// Asks the server to revoke the refresh token, and the grant with it
    /// (RFC 7009), giving up after a few seconds. The log says only how it
    /// went, never the server's words or the token.
    pub async fn revoke(self) {
        match mailrs_jmap::oauth::revoke(&self.session_url, &self.client_id, &self.refresh_token)
            .await
        {
            Ok(true) => tracing::info!("revoked a browser sign-in the app no longer keeps"),
            Ok(false) => {
                tracing::info!("the server offers no revocation; the grant stays until it runs out")
            }
            Err(_) => tracing::warn!("could not revoke a browser sign-in the app no longer keeps"),
        }
    }
}

/// What a JMAP account signs in with once its secret is read.
pub(crate) enum JmapSignIn {
    /// A password or a token, as the row's `auth` says.
    Secret(String),
    /// The browser sign-in's tokens, which JMAP and the account's CalDAV
    /// and CardDAV servers share, so a rotation is kept once.
    OAuth(Arc<OAuthSession>),
}

/// Runs `keep` for each rotated refresh token: it writes the token to
/// `tokens` under `id`, off the runtime. Only the token goes in; the
/// session neither logs nor shows the error text it answers.
pub(crate) fn keep_rotations<P: PasswordStore + 'static>(
    tokens: Arc<P>,
    id: AccountId,
) -> impl Fn(String) -> futures::future::BoxFuture<'static, Result<(), String>> + Send + Sync + 'static
{
    move |token| {
        let tokens = Arc::clone(&tokens);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || tokens.save(id, &token))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        })
    }
}

/// A JMAP OAuth session as a calendar or contacts server's token source.
struct SessionTokens(Arc<OAuthSession>);

/// `session` as the token source of the account's CalDAV and CardDAV
/// clients, so they send the token JMAP sends and refresh through it.
pub(crate) fn session_tokens(session: Arc<OAuthSession>) -> Arc<dyn mailrs_dav::TokenSource> {
    Arc::new(SessionTokens(session))
}

impl mailrs_dav::TokenSource for SessionTokens {
    fn token<'a>(
        &'a self,
        refused: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let token = match refused {
                // Calls refused together share one refresh.
                Some(refused) => self.0.bearer_instead_of(refused).await,
                None => self.0.bearer(false).await,
            };
            token.map_err(|e| e.to_string())
        })
    }
}

/// The services for a JMAP account, from its session row and the secret in
/// `secrets.jmap`. Without either, the store records that the account needs to
/// sign in again before this answers `NeedsReauth`. Each service follows
/// the session's capabilities; a confirmed CalDAV, CardDAV or ManageSieve
/// server serves what the session lacks, signed in with the app password
/// in `secrets.passwords`, or the account's own password over Basic. An
/// account signed in through the browser keeps its refresh token in
/// `secrets.jmap` and one OAuth session in `secrets.jmap_sessions` for the
/// run; its CalDAV and CardDAV servers on the session's own site take the
/// same session's token, or an app password once one refused the token.
/// Nothing here connects; the client reads the session when sync first
/// asks.
pub async fn connect_jmap(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    window_days: i64,
) -> Result<AccountServices, SyncError> {
    connect_jmap_within(db, secrets, account, window_days, STEP_WAIT).await
}

pub(crate) async fn connect_jmap_within(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    window_days: i64,
    wait: Duration,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let jmap = Arc::clone(&secrets.jmap);
    let saved = bounded(
        id,
        "the mail store",
        wait,
        db.read(move |c| mailrs_store::jmap::load(c, id)),
    )
    .await?;
    let secret = read_secret(Arc::clone(&jmap), id, wait).await?;
    let sign_in = match (saved, secret) {
        (Some(saved), Some(secret)) => match (&saved.auth, &saved.oauth) {
            (mailrs_store::jmap::JmapAuth::OAuth, Some(client)) => {
                let session =
                    secrets
                        .jmap_sessions
                        .session_for(id, &saved.session_url, client, || {
                            let client = mailrs_jmap::oauth::OAuthClient {
                                client_id: client.client_id.clone(),
                                token_endpoint: client.token_endpoint.clone(),
                                resource: saved.session_url.clone(),
                            };
                            OAuthSession::new(client, secret).on_rotated(keep_rotations(jmap, id))
                        });
                Some((saved, JmapSignIn::OAuth(session)))
            }
            // An OAuth row that lost its client cannot refresh.
            (mailrs_store::jmap::JmapAuth::OAuth, None) => None,
            _ => Some((saved, JmapSignIn::Secret(secret))),
        },
        _ => None,
    };
    let Some((saved, sign_in)) = sign_in else {
        let marked = db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth));
        bounded(id, "the mail store", wait, marked).await?;
        return Err(BackendError::NeedsReauth.into());
    };
    jmap_services(
        db,
        Arc::clone(&secrets.passwords),
        account,
        saved,
        sign_in,
        window_days,
        wait,
    )
    .await
}

/// [`connect_jmap`] once the row and what signs it in are read.
pub(crate) async fn jmap_services<P: PasswordStore + 'static>(
    db: &Db,
    dav: Arc<P>,
    account: &Account,
    saved: mailrs_store::jmap::JmapSaved,
    sign_in: JmapSignIn,
    window_days: i64,
    wait: Duration,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let (auth, basic_password, bearer) = match sign_in {
        JmapSignIn::OAuth(session) => {
            let bearer = session_tokens(Arc::clone(&session));
            (mailrs_jmap::Auth::OAuth(session), None, Some(bearer))
        }
        JmapSignIn::Secret(secret) => {
            let basic = (saved.auth == mailrs_store::jmap::JmapAuth::Basic).then(|| secret.clone());
            (auth_of(&saved, &account.email, secret)?, basic, None)
        }
    };
    let client = mailrs_jmap::JmapClient::new(&saved.session_url, auth)
        .map_err(crate::services::jmap::backend)?;
    let token = saved.auth == mailrs_store::jmap::JmapAuth::Bearer;
    let table_says_no_reply = mailrs_discover::jmap_named(account.provider_name())
        .is_some_and(|p| p.token_lacks.iter().any(|c| c == mailrs_jmap::VACATION));
    let settings = JmapSettings {
        address: account.email.clone(),
        provider_name: account.provider_name().to_string(),
        window_days,
        labels: saved.labels,
        capabilities: saved.capabilities.clone(),
        auto_reply_needs_sign_in: token
            && table_says_no_reply
            && !saved.offers(mailrs_jmap::VACATION)
            && !saved.offers(mailrs_jmap::SIEVE),
    };
    let app_password = read_secret(dav, id, wait).await?;
    let read = db.read(move |c| {
        let refused = mailrs_store::services::all_misses(c)?
            .into_iter()
            .filter(|(account, _, miss)| *account == id && *miss == Miss::Refused)
            .map(|(_, kind, _)| kind)
            .collect::<Vec<_>>();
        let local = !mailrs_store::local_rules::list(c, id)?.is_empty();
        Ok((mailrs_store::services::load(c, id)?, refused, local))
    });
    let (found, refused, local_kept) = bounded(id, "the mail store", wait, read).await?;
    let found = dav_plan(
        found,
        &saved,
        bearer.is_some(),
        app_password,
        basic_password,
        &refused,
    )
    .into_iter()
    .filter_map(|(service, sign)| match (sign, &bearer) {
        (DavSign::Password(secret), _) => Some(FoundServer {
            service,
            secret,
            bearer: None,
        }),
        (DavSign::Bearer, Some(tokens)) => {
            let source = dav_bearer(Arc::clone(tokens), db.clone(), id, service.kind);
            Some(FoundServer {
                service,
                secret: String::new(),
                bearer: Some(source),
            })
        }
        (DavSign::Bearer, None) => None,
    })
    .collect();
    let beside = Beside {
        account_id: id,
        address: account.email.clone(),
        provider_name: account.provider_name().to_string(),
        found,
        local: Some(LocalRules::new(db.clone(), id)),
        // Local rules a switch from IMAP could not move into the script
        // go on running here, beside it, and the Rules dialog lists both.
        local_kept,
    };
    let fallback = storage::fastmail(&saved.session_url, bearer);
    let mut services = AccountServices::jmap_from(client, settings, beside);
    if let (crate::services::AnyMail::Jmap(mail), Some(read)) = (&mut services.mail, fallback) {
        *mail = mail.clone().with_storage_fallback(read);
    }
    Ok(services)
}

/// How a CalDAV, CardDAV or ManageSieve server beside a JMAP session
/// signs in.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum DavSign {
    /// The account's own password over Basic, or the app password kept
    /// beside a token or a browser sign-in.
    Password(String),
    /// The OAuth session's token.
    Bearer,
}

/// The servers kept for a JMAP account that serve what its session lacks,
/// and how each signs in. Only a confirmed server is used. An app password
/// wins: a server that takes it signs in with it, as beside a token. An
/// account signed in through the browser (`oauth`) with no app password
/// sends its token only to CalDAV and CardDAV servers on the session's own
/// site, since the token carries the mail scope, and leaves out a service
/// in `refused`, whose server refused a fresh token: it waits for an app
/// password rather than refreshing on every call. An account with neither
/// password nor token reaches no server.
pub(crate) fn dav_plan(
    found: Vec<FoundService>,
    saved: &mailrs_store::jmap::JmapSaved,
    oauth: bool,
    app_password: Option<String>,
    basic_password: Option<String>,
    refused: &[ServiceKind],
) -> Vec<(FoundService, DavSign)> {
    let sign = match (app_password.or(basic_password), oauth) {
        (Some(password), _) => DavSign::Password(password),
        (None, true) => DavSign::Bearer,
        (None, false) => return Vec::new(),
    };
    let bearer = sign == DavSign::Bearer;
    found
        .into_iter()
        .filter(|f| f.confirmed)
        .filter(|f| match f.kind {
            ServiceKind::CardDav => !saved.offers(mailrs_jmap::CONTACTS),
            ServiceKind::CalDav => !saved.offers(mailrs_jmap::CALENDARS),
            ServiceKind::Sieve => !saved.offers(mailrs_jmap::SIEVE) && !bearer,
        })
        .filter(|f| {
            !bearer
                || (mailrs_dav::shares_site(&saved.session_url, &f.url)
                    && !refused.contains(&f.kind))
        })
        .map(|f| (f, sign.clone()))
        .collect()
}

/// The token source for one CalDAV or CardDAV server of an account signed
/// in through the browser. Tokens come from `inner`, the account's one
/// session. When the server refuses a fresh token too, the source hands
/// out no more tokens for it this run, so no call refreshes again, and
/// the store keeps that the service refused the sign-in: the next start
/// leaves it out until the person gives an app password.
pub(crate) fn dav_bearer(
    inner: Arc<dyn mailrs_dav::TokenSource>,
    db: Db,
    account: AccountId,
    kind: ServiceKind,
) -> Arc<dyn mailrs_dav::TokenSource> {
    Arc::new(DavBearer {
        inner,
        db,
        account,
        kind,
        stopped: std::sync::atomic::AtomicBool::new(false),
    })
}

struct DavBearer {
    inner: Arc<dyn mailrs_dav::TokenSource>,
    db: Db,
    account: AccountId,
    kind: ServiceKind,
    stopped: std::sync::atomic::AtomicBool,
}

impl mailrs_dav::TokenSource for DavBearer {
    fn token<'a>(
        &'a self,
        refused: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        if self.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            return Box::pin(async { Err("the server refused the account's sign-in".to_string()) });
        }
        self.inner.token(refused)
    }

    fn refused_again(&self) {
        if self.stopped.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let (db, account, kind) = (self.db.clone(), self.account, self.kind);
        tracing::warn!(
            account,
            service = kind.as_str(),
            "the server refused the browser sign-in; it needs an app password"
        );
        tokio::spawn(async move {
            let kept = db
                .write(move |c| {
                    mailrs_store::services::save_miss(c, account, kind, Some(Miss::Refused))
                })
                .await;
            if let Err(err) = kept {
                tracing::warn!(account, %err, "could not keep that the server refused the sign-in");
            }
        });
    }
}

/// Forgets that the account's CalDAV or CardDAV server refused its token,
/// so the next connect sends the token there once more. A browser sign-in
/// calls this with its new grant: a server that refuses that one too after
/// the one shared refresh is marked again and waits for an app password.
pub(crate) async fn try_the_token_again(db: &Db, account: AccountId) -> Result<(), SyncError> {
    db.write(move |c| {
        for kind in [ServiceKind::CalDav, ServiceKind::CardDav] {
            mailrs_store::services::save_miss(c, account, kind, None)?;
        }
        Ok(())
    })
    .await?;
    Ok(())
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
pub fn pop3_servers_for(
    pop3: &Server,
    pop3_user: &str,
    smtp: &Server,
    smtp_user: &str,
) -> Pop3Servers {
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
