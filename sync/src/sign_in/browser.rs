//! Signing an account in on its provider's own page: Google's consent or
//! Microsoft's sign-in, in the person's browser, then the account, its
//! refresh token and its services.

use std::sync::Arc;

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, GmailSignIn, MailRoute, Provider, SignInClient};
use mailrs_gmail::{Authorized, GMAIL_API_BASE, GmailError, Granted, OAuthClient, ScopeList};
use mailrs_graph::GraphError;
use mailrs_jmap::JmapError;
use mailrs_store::accounts;

use super::own_client::{OwnClient, OwnClientError, keep_project};
use super::{
    JmapSignInError, KeptClient, MicrosoftSignInError, NewMicrosoft, SignInError, google_signed_in,
    google_signed_in_as, kept_own_client, microsoft_signed_in, own_client_of,
};
use crate::passwords::Secrets;
use crate::{
    AccountServices, Clients, Connected, Connector, SyncError, connect_account, connect_microsoft,
    now_millis,
};

/// A sign-in that runs on the provider's page in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Google,
    Microsoft,
}

impl Browser {
    /// The browser sign-in an account of `provider` goes through, or
    /// `None` for an IMAP, POP3, JMAP or calendar account, which signs in
    /// again through the Add Account form.
    pub fn of(provider: Provider) -> Option<Browser> {
        match provider {
            Provider::Gmail => Some(Browser::Google),
            Provider::Microsoft => Some(Browser::Microsoft),
            Provider::Imap | Provider::Pop3 | Provider::Jmap | Provider::CalDav => None,
        }
    }
}

/// Which address a browser sign-in is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// Whoever signs in, as when adding a Google account from the banner.
    Anyone,
    /// This address fills Microsoft's address field, and another one is
    /// still taken. Google's page takes no address.
    Suggested(String),
    /// Only this address: a sign-in that comes back as another is refused.
    Only(String),
}

impl Wanted {
    fn address(&self) -> Option<&str> {
        match self {
            Wanted::Anyone => None,
            Wanted::Suggested(address) | Wanted::Only(address) => Some(address),
        }
    }

    fn check(&self, signed_in: &str) -> Result<(), BrowserSignInError> {
        match self {
            Wanted::Only(wanted) if !wanted.eq_ignore_ascii_case(signed_in) => {
                Err(BrowserSignInError::OtherAddress {
                    signed_in: signed_in.to_string(),
                    wanted: wanted.clone(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// An account signed in on its provider's page, with the services to
/// start its sync on, or what it still lacks for them.
pub struct SignedIn {
    pub account: Account,
    /// The name the provider knows the person by. Only Microsoft gives one.
    pub name: Option<String>,
    pub services: Connected,
}

fn already_held() -> String {
    format!(
        "{} {}",
        gettext("This address is already in Penguin Mail."),
        super::remove_and_add_again()
    )
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserSignInError {
    /// This build has no client for the provider, so no page was opened.
    #[error("{}", no_client(*.0))]
    NoClient(Browser),
    #[error("{}", other_address(.signed_in, .wanted))]
    OtherAddress { signed_in: String, wanted: String },
    #[error("{}", google_refused(.0))]
    Google(GmailError),
    #[error("{}", microsoft_refused(.0))]
    Microsoft(GraphError),
    #[error(transparent)]
    Kept(#[from] SignInError),
    #[error(transparent)]
    KeptMicrosoft(#[from] MicrosoftSignInError),
    /// A JMAP server's sign-in failed.
    #[error("{0}")]
    Jmap(JmapError),
    #[error(transparent)]
    KeptJmap(#[from] JmapSignInError),
    /// The JMAP server publishes no sign-in a browser can run.
    #[error("{}", gettext("This server does not offer a sign-in in the browser."))]
    NotOffered,
    /// The JMAP server signed in without a refresh token, so the account
    /// could not stay signed in.
    #[error(
        "{}",
        gettext(
            "The server did not let Penguin Mail stay signed in. Sign in with a password or a token instead."
        )
    )]
    NoRefreshToken,
    /// The built-in client signs in no account that reads its mail through
    /// the API, and adds none: its token carries no mail, and would replace
    /// one that does. An account on the IMAP route keeps its route too.
    // Translators: Gmail is a name; keep it as it is.
    #[error("{}", gettext("Choose how Penguin Mail signs in to Gmail"))]
    NeedsRoute,
    /// The address already belongs to an account that signs in another
    /// way, and this sign-in would change it.
    #[error("{}", already_held())]
    AlreadyHeld,
    /// Nobody came back from the browser within the wait.
    #[error("{}", gettext("Gave up waiting for the browser after ten minutes."))]
    GaveUp,
    /// The person canceled the wait for the browser.
    #[error("{}", gettext("Canceled."))]
    Canceled,
    #[error(transparent)]
    Connect(#[from] SyncError),
    /// The account signed in with an own Google client that is no longer
    /// on this computer, and nothing else may take its place.
    // Translators: Penguin Mail and Google are names; keep them as they are.
    #[error(
        "{}",
        gettext(
            "Penguin Mail can't find your Google project on this computer. Give it the key again to sign in."
        )
    )]
    OwnClientMissing,
    /// The environment names half an own Google client.
    #[error(transparent)]
    OwnClient(#[from] OwnClientError),
}

fn no_client(browser: Browser) -> String {
    match browser {
        Browser::Google => gettext(
            "This copy of Penguin Mail was built without Google sign-in. \
             Get a release from github.com/c9dev/penguin-mail/releases.",
        ),
        Browser::Microsoft => gettext(
            "This copy of Penguin Mail was built without Microsoft sign-in. \
             Get a release from github.com/c9dev/penguin-mail/releases.",
        ),
    }
}

pub(crate) fn other_address(signed_in: &str, wanted: &str) -> String {
    fill(
        &gettext(
            "You signed in as {account}. Choose {wanted} to reconnect that \
             account.",
        ),
        &[("account", signed_in), ("wanted", wanted)],
    )
}

fn google_refused(err: &GmailError) -> String {
    match err {
        GmailError::MailNotGranted => gettext(
            "Penguin Mail cannot work without access to your mail. Sign in \
             again and leave the Gmail permission ticked.",
        ),
        err => err.to_string(),
    }
}

fn microsoft_refused(err: &GraphError) -> String {
    match err {
        GraphError::AdminApproval => {
            gettext("Your organization's administrator must approve Penguin Mail.")
        }
        GraphError::Declined | GraphError::MailNotGranted => gettext(
            "Penguin Mail cannot work without access to your mail. Sign in again and allow it.",
        ),
        GraphError::MailboxOnPremises => gettext(
            "This mailbox is on your organization's own Exchange server, which Penguin Mail cannot reach.",
        ),
        err => err.to_string(),
    }
}

/// Runs `browser`'s sign-in: `open` gets the provider's page, and the run
/// finishes when the browser comes back. Then the account is added, or
/// found when it is already here, its refresh token goes to the keyring
/// in `connector`, and its services are built. Each sign-in asks for every
/// permission Penguin Mail uses. The caller bounds the wait.
pub async fn in_browser(
    connector: &Connector,
    browser: Browser,
    wanted: Wanted,
    open: impl FnOnce(&str),
) -> Result<SignedIn, BrowserSignInError> {
    match browser {
        Browser::Google => with_google(connector, wanted, open).await,
        Browser::Microsoft => with_microsoft(connector, wanted, open).await,
    }
}

/// Where the client a Google sign-in goes through came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSource {
    /// The own client the environment names.
    Environment,
    /// The own client the person's setup chose.
    Setup,
    /// The own client the account signing in again kept.
    Kept,
    /// The client compiled into this build.
    BuiltIn,
}

/// The client a Google sign-in goes through, the own client behind it,
/// and where it came from.
#[derive(Clone)]
pub struct ClientChoice {
    pub oauth: OAuthClient,
    /// `None` only for the built-in client.
    pub own: Option<OwnClient>,
    pub source: ClientSource,
}

impl ClientChoice {
    fn own(own: &OwnClient, source: ClientSource) -> Self {
        ClientChoice {
            oauth: own.oauth(),
            own: Some(own.clone()),
            source,
        }
    }

    /// The kind of client the account records once signed in.
    pub fn kind(&self) -> SignInClient {
        match self.source {
            ClientSource::BuiltIn => SignInClient::BuiltIn,
            ClientSource::Environment | ClientSource::Setup | ClientSource::Kept => {
                SignInClient::Own
            }
        }
    }
}

/// The client a Google sign-in goes through: the own client the
/// environment names, then the one the setup chose, then `kept`, the one
/// the account signing in again kept, then the build's. Half an own client
/// in the environment is an error rather than a quiet fall back, since the
/// person set the variables to avoid the others.
pub fn google_sign_in(
    clients: &Clients,
    kept: Option<&OwnClient>,
) -> Result<ClientChoice, BrowserSignInError> {
    match (&clients.own_google, &clients.chosen_google, kept) {
        (Some(Ok(own)), _, _) => Ok(ClientChoice::own(own, ClientSource::Environment)),
        (Some(Err(half)), _, _) => Err(half.clone().into()),
        (None, Some(chosen), _) => Ok(ClientChoice::own(chosen, ClientSource::Setup)),
        (None, None, Some(kept)) => Ok(ClientChoice::own(kept, ClientSource::Kept)),
        (None, None, None) => clients
            .google
            .clone()
            .map(|oauth| ClientChoice {
                oauth,
                own: None,
                source: ClientSource::BuiltIn,
            })
            .ok_or(BrowserSignInError::NoClient(Browser::Google)),
    }
}

/// Keeps the setup's client as the person's project, for any later Google
/// account, once an account signed in through it. A client from the
/// environment or an account's keyring entry is never kept. A keyring that
/// refuses is logged and does not fail the sign-in, which already holds.
pub async fn keep_chosen_project(secrets: &Secrets, choice: &ClientChoice) {
    let (ClientSource::Setup, Some(own)) = (choice.source, &choice.own) else {
        return;
    };
    if let Err(err) = keep_project(secrets, own).await {
        tracing::warn!(%err, "could not keep the Google project for later accounts");
    }
}

async fn with_google(
    connector: &Connector,
    wanted: Wanted,
    open: impl FnOnce(&str),
) -> Result<SignedIn, BrowserSignInError> {
    let Connector {
        db,
        config,
        clients,
        secrets,
        ..
    } = connector;
    // A Gmail account whose mail comes over IMAP keeps how its mail signs
    // in: the browser only brings its calendar token, through the client
    // that token refreshes with, whatever own client the environment or
    // the setup names.
    if let Some(held) = held_on_imap(db, wanted.address()).await? {
        let account = calendar_consent(connector, &held, open).await?;
        let services = connector.connect(&account).await?;
        return Ok(SignedIn {
            account,
            name: None,
            services,
        });
    }
    // An account signing in again keeps its own client. The environment
    // and the setup win over it, so it is read only when neither is set.
    // One whose client is gone stops here and never takes the built-in one.
    let kept = match (&wanted, &clients.own_google, &clients.chosen_google) {
        (Wanted::Only(address), None, None) => {
            match kept_own_client(db, config, secrets, address).await? {
                KeptClient::Found(own) => Some(own),
                KeptClient::Missing => return Err(BrowserSignInError::OwnClientMissing),
                KeptClient::NotOwn => None,
            }
        }
        _ => None,
    };
    let choice = google_sign_in(clients, kept.as_ref())?;
    if choice.oauth.scope_list() == ScopeList::BuiltIn {
        // Only a Gmail account that reads its mail over IMAP, which took
        // its calendar consent above, may sign in through the built-in
        // client. Any other, and a new address, needs a route first.
        return Err(BrowserSignInError::NeedsRoute);
    }
    let authorized = mailrs_gmail::authorize(&choice.oauth, GMAIL_API_BASE, open)
        .await
        .map_err(BrowserSignInError::Google)?;
    let account = signed_in_through(connector, &choice, &wanted, authorized).await?;
    let client = connect_account(choice.oauth, Arc::clone(&secrets.google), &account, db).await?;
    Ok(SignedIn {
        account,
        name: None,
        services: Connected::Ready(AccountServices::google(client)),
    })
}

/// Keeps the account `authorized` signed in as through `choice`, when it
/// is the one `wanted`, and then the setup's project.
pub(crate) async fn signed_in_through(
    connector: &Connector,
    choice: &ClientChoice,
    wanted: &Wanted,
    authorized: Authorized,
) -> Result<Account, BrowserSignInError> {
    wanted.check(&authorized.email)?;
    // Whoever signs in may be an account on the IMAP route, which this
    // token would put on the API. It keeps its route: removing the
    // account and adding it again changes that.
    if held_on_imap(&connector.db, Some(&authorized.email))
        .await?
        .is_some()
    {
        return Err(BrowserSignInError::AlreadyHeld);
    }
    let granted = authorized.granted.as_ref().map(Granted::to_scope);
    let account = google_signed_in(
        &connector.db,
        &connector.secrets,
        &authorized.email,
        &authorized.refresh_token,
        now_millis(),
        granted.as_deref(),
        &choice.oauth.scopes().join(" "),
        choice.own.as_ref(),
    )
    .await?;
    keep_chosen_project(&connector.secrets, choice).await;
    Ok(account)
}

/// The Gmail account at `address` when its mail comes over IMAP.
async fn held_on_imap(
    db: &mailrs_store::Db,
    address: Option<&str>,
) -> Result<Option<Account>, SyncError> {
    let Some(address) = address.map(str::to_string) else {
        return Ok(None);
    };
    Ok(db
        .read(move |c| match accounts::account_by_email(c, &address)? {
            Some(held) if held.provider == Provider::Gmail => {
                let on_imap = accounts::gmail_sign_in(c, held.id)?.route() == MailRoute::Imap;
                Ok(on_imap.then_some(held))
            }
            _ => Ok(None),
        })
        .await?)
}

/// Asks Google for the calendar, contacts and Drive access of `account`, a
/// Gmail account that reads its mail over IMAP. Its mail route stays as it
/// is. An account on the API route is refused before the browser opens,
/// since this token carries no mail.
///
/// The consent goes through the client the account's Google token
/// refreshes with: an account on an app password whose token its own
/// project issued asks that project again, so the project stays in the
/// keyring. Any other, and one whose project is no longer on this
/// computer, asks the built-in client.
/// The own client a calendar consent for `id`, which signs in as
/// `sign_in`, goes through, or `None` for the built-in client. An account
/// on Online Accounts that came from its own project keeps the project
/// while the keyring keeps it.
async fn consent_own(
    config: &super::Config,
    secrets: &Secrets,
    id: mailrs_domain::AccountId,
    sign_in: &GmailSignIn,
) -> Result<Option<OwnClient>, SyncError> {
    let kept = Arc::clone(&secrets.google_clients);
    match (sign_in.on_online_accounts(), sign_in.client()) {
        (true, _) => super::kept_own(kept, id).await,
        (false, SignInClient::Own) => own_client_of(config, kept, id).await,
        (false, SignInClient::BuiltIn) => Ok(None),
    }
}

/// Whether [`calendar_consent`] for `account` goes through the built-in
/// client, so Google shows the built-in client's consent screen.
pub async fn consent_through_built_in(
    connector: &Connector,
    account: &Account,
) -> Result<bool, SyncError> {
    let id = account.id;
    let sign_in = connector
        .db
        .read(move |c| accounts::gmail_sign_in(c, id))
        .await?;
    Ok(
        consent_own(&connector.config, &connector.secrets, id, &sign_in)
            .await?
            .is_none(),
    )
}

pub async fn calendar_consent(
    connector: &Connector,
    account: &Account,
    open: impl FnOnce(&str),
) -> Result<Account, BrowserSignInError> {
    let Connector {
        db,
        config,
        clients,
        secrets,
        ..
    } = connector;
    let (id, provider) = (account.id, account.provider);
    let sign_in = db
        .read(move |c| accounts::gmail_sign_in(c, id))
        .await
        .map_err(SyncError::from)?;
    // This token carries no mail, so only an account whose mail comes over
    // IMAP may take it.
    if provider != Provider::Gmail || sign_in.route() != MailRoute::Imap {
        return Err(BrowserSignInError::NeedsRoute);
    }
    let own = consent_own(config, secrets, id, &sign_in).await?;
    let (oauth, client) = match &own {
        Some(own) => (own.oauth(), SignInClient::Own),
        None => (
            clients
                .google
                .clone()
                .ok_or(BrowserSignInError::NoClient(Browser::Google))?,
            SignInClient::BuiltIn,
        ),
    };
    let authorized =
        mailrs_gmail::authorize_hinted(&oauth, GMAIL_API_BASE, Some(&account.email), open)
            .await
            .map_err(BrowserSignInError::Google)?;
    Wanted::Only(account.email.clone()).check(&authorized.email)?;
    let granted = authorized.granted.as_ref().map(Granted::to_scope);
    // An Online Accounts account stays on Online Accounts, and any other
    // records the client that issued the token.
    let kept = |before: &GmailSignIn| match before {
        GmailSignIn::OnlineAccounts { .. } => before.clone(),
        other => other.with_google_token(client),
    };
    let asked = oauth.scopes().join(" ");
    let token = super::GoogleToken {
        email: &account.email,
        refresh_token: &authorized.refresh_token,
        granted: granted.as_deref(),
        asked: &asked,
    };
    Ok(google_signed_in_as(db, secrets, token, now_millis(), kept, own.as_ref()).await?)
}

async fn with_microsoft(
    connector: &Connector,
    wanted: Wanted,
    open: impl FnOnce(&str),
) -> Result<SignedIn, BrowserSignInError> {
    let Connector {
        db,
        config,
        clients,
        secrets,
        ..
    } = connector;
    let client = clients
        .microsoft
        .clone()
        .ok_or(BrowserSignInError::NoClient(Browser::Microsoft))?;
    let authorized =
        mailrs_graph::authorize(&client, mailrs_graph::GRAPH_BASE, wanted.address(), open)
            .await
            .map_err(BrowserSignInError::Microsoft)?;
    wanted.check(&authorized.email)?;
    let new = NewMicrosoft {
        address: authorized.email.clone(),
        provider_name: authorized.tenant.provider_name().to_string(),
        refresh_token: authorized.refresh_token,
        granted: authorized
            .granted
            .as_ref()
            .map(mailrs_graph::Granted::to_scope),
    };
    let tokens = Arc::clone(&secrets.microsoft);
    let account = microsoft_signed_in(
        db,
        Arc::clone(&tokens),
        new,
        now_millis(),
        &mailrs_graph::SCOPES.join(" "),
    )
    .await?;
    let window_days = config.engine_config().window_days;
    let services = connect_microsoft(db, tokens, client, &account, window_days).await?;
    Ok(SignedIn {
        account,
        name: authorized.name,
        services: Connected::Ready(services),
    })
}
