//! Signing an account in on its provider's own page: Google's consent or
//! Microsoft's sign-in, in the person's browser, then the account, its
//! refresh token and its services.

use std::sync::Arc;

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, Provider};
use mailrs_gmail::{GMAIL_API_BASE, GmailError, Granted, SIGN_IN_SCOPES};
use mailrs_graph::GraphError;

use super::{MicrosoftSignInError, NewMicrosoft, SignInError, google_signed_in, microsoft_signed_in};
use crate::{AccountServices, Connector, SyncError, connect_account, connect_microsoft, now_millis};

/// A sign-in that runs on the provider's page in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Google,
    Microsoft,
}

impl Browser {
    /// The browser sign-in an account of `provider` goes through, or
    /// `None` for an IMAP or POP3 account, which signs in with a password.
    pub fn of(provider: Provider) -> Option<Browser> {
        match provider {
            Provider::Gmail => Some(Browser::Google),
            Provider::Microsoft => Some(Browser::Microsoft),
            Provider::Imap | Provider::Pop3 => None,
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
            Wanted::Only(wanted) if !wanted.eq_ignore_ascii_case(signed_in) => Err(BrowserSignInError::OtherAddress {
                signed_in: signed_in.to_string(),
                wanted: wanted.clone(),
            }),
            _ => Ok(()),
        }
    }
}

/// An account signed in on its provider's page, with the services to
/// start its sync on.
pub struct SignedIn {
    pub account: Account,
    /// The name the provider knows the person by. Only Microsoft gives one.
    pub name: Option<String>,
    pub services: AccountServices,
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
    #[error(transparent)]
    Connect(#[from] SyncError),
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

fn other_address(signed_in: &str, wanted: &str) -> String {
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
        GraphError::AdminApproval => gettext("Your organization's administrator must approve Penguin Mail."),
        GraphError::Declined | GraphError::MailNotGranted => {
            gettext("Penguin Mail cannot work without access to your mail. Sign in again and allow it.")
        }
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

async fn with_google(
    connector: &Connector,
    wanted: Wanted,
    open: impl FnOnce(&str),
) -> Result<SignedIn, BrowserSignInError> {
    let Connector { db, clients, secrets, .. } = connector;
    let oauth = clients.google.clone().ok_or(BrowserSignInError::NoClient(Browser::Google))?;
    let authorized = mailrs_gmail::authorize(&oauth, GMAIL_API_BASE, open)
        .await
        .map_err(BrowserSignInError::Google)?;
    wanted.check(&authorized.email)?;
    let granted = authorized.granted.as_ref().map(Granted::to_scope);
    let account = google_signed_in(
        db,
        Arc::clone(&secrets.google),
        &authorized.email,
        &authorized.refresh_token,
        now_millis(),
        granted.as_deref(),
        &SIGN_IN_SCOPES.join(" "),
    )
    .await?;
    let client = connect_account(oauth, Arc::clone(&secrets.google), &account, db).await?;
    Ok(SignedIn {
        account,
        name: None,
        services: AccountServices::google(client),
    })
}

async fn with_microsoft(
    connector: &Connector,
    wanted: Wanted,
    open: impl FnOnce(&str),
) -> Result<SignedIn, BrowserSignInError> {
    let Connector { db, config, clients, secrets } = connector;
    let client = clients.microsoft.clone().ok_or(BrowserSignInError::NoClient(Browser::Microsoft))?;
    let authorized = mailrs_graph::authorize(&client, mailrs_graph::GRAPH_BASE, wanted.address(), open)
        .await
        .map_err(BrowserSignInError::Microsoft)?;
    wanted.check(&authorized.email)?;
    let new = NewMicrosoft {
        address: authorized.email.clone(),
        provider_name: authorized.tenant.provider_name().to_string(),
        refresh_token: authorized.refresh_token,
        granted: authorized.granted.as_ref().map(mailrs_graph::Granted::to_scope),
    };
    let tokens = Arc::clone(&secrets.microsoft);
    let account = microsoft_signed_in(db, Arc::clone(&tokens), new, now_millis(), &mailrs_graph::SCOPES.join(" ")).await?;
    let window_days = config.engine_config().window_days;
    let services = connect_microsoft(db, tokens, client, &account, window_days).await?;
    Ok(SignedIn {
        account,
        name: authorized.name,
        services,
    })
}
