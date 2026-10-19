//! Linking a Gmail account to the desktop's Online Accounts: finding out
//! what its token can read, and keeping the account.

use std::sync::Arc;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, EpochMillis, MailRoute, Provider, SignInClient};
use mailrs_gmail::{GmailClient, GmailError};
use mailrs_store::{StoreError, accounts};

use crate::Connector;
use crate::connect::gmail_route::GoaAccess;
use crate::goa::{GoaAccount, GoaTokens, OnlineAccounts};

/// How an Online Accounts account reads its mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linked {
    /// Through the Gmail API: GNOME's project has it on.
    Api,
    /// Through Gmail's IMAP and SMTP.
    Imap,
}

impl Linked {
    pub fn route(self) -> MailRoute {
        match self {
            Linked::Api => MailRoute::GmailApi,
            Linked::Imap => MailRoute::Imap,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("this computer's sign-in does not hold that address")]
    NotInOnlineAccounts,
    #[error("{}", sign_in_again())]
    SignInAgain,
    #[error("{}", mail_off())]
    MailOff,
    #[error("{}", unreachable())]
    Unreachable,
    /// The address belongs to an account another provider serves.
    #[error("{}", super::taken(.address, .provider))]
    Taken { address: String, provider: String },
    #[error(transparent)]
    Gmail(GmailError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn sign_in_again() -> String {
    // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language. Google is a name.
    gettext("Online Accounts couldn't sign in to Google. Sign in again there.")
}

fn mail_off() -> String {
    // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
    gettext("Mail is turned off for this account in Online Accounts.")
}

fn unreachable() -> String {
    // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
    gettext("Can't reach Online Accounts on this computer.")
}

/// Asks the Gmail API for the account's profile with Online Accounts'
/// token. Answered: the API route. Switched off in GNOME's project: IMAP.
pub async fn probe(
    goa: &Arc<dyn OnlineAccounts>,
    goa_account: &GoaAccount,
    gmail_base: &str,
) -> Result<Linked, LinkError> {
    let tokens = Arc::new(GoaTokens::new(Arc::clone(goa), goa_account.id.clone()));
    let client = GmailClient::with_source(
        Arc::new(GoaAccess(tokens)),
        mailrs_gmail::api_http_client(),
        Arc::new(mailrs_gmail::AccountQuota::standalone()),
    )
    .with_base_url(gmail_base);
    match client.profile().await {
        Ok(_) => Ok(Linked::Api),
        Err(GmailError::ApiDisabled { .. }) => Ok(Linked::Imap),
        Err(GmailError::NeedsReauth) => Err(LinkError::SignInAgain),
        Err(GmailError::Network(_)) => Err(LinkError::Unreachable),
        Err(other) => Err(LinkError::Gmail(other)),
    }
}

/// Refuses an Online Accounts account whose Mail switch is off, or that
/// Online Accounts wants signed in again.
pub fn usable(goa_account: &GoaAccount) -> Result<(), LinkError> {
    if goa_account.mail_disabled {
        return Err(LinkError::MailOff);
    }
    if goa_account.attention_needed {
        return Err(LinkError::SignInAgain);
    }
    Ok(())
}

/// Links `goa_account` to Penguin Mail. An account Online Accounts has
/// switched off or wants signed in again is refused. A new address becomes
/// an account on `Goa`; an address the store already holds is left as it
/// is and answered with its probe, for the caller to move.
pub async fn link(
    connector: &Connector,
    goa_account: &GoaAccount,
    gmail_base: &str,
    now: EpochMillis,
) -> Result<(Account, Linked), LinkError> {
    usable(goa_account)?;
    let linked = probe(&connector.goa, goa_account, gmail_base).await?;
    // Google reads an address without regard to case and the store keeps it
    // lower case, as every other sign-in does.
    let address = goa_account.identity.trim().to_lowercase();
    let goa_id = goa_account.id.clone();
    let kept = connector
        .db
        .write(move |c| {
            if let Some(held) = accounts::account_by_email(c, &address)? {
                return Ok(match held.provider {
                    Provider::Gmail => Ok(held),
                    _ => Err(held.provider_name().to_string()),
                });
            }
            let id = accounts::insert_account(c, &address, now)?;
            accounts::set_sign_in_client(c, id, SignInClient::Goa)?;
            accounts::set_mail_route(c, id, linked.route())?;
            accounts::set_goa_id(c, id, Some(&goa_id))?;
            // The row was written a line above in the same transaction.
            let account = accounts::account(c, id)?
                .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
            Ok(Ok(account))
        })
        .await?;
    match kept {
        Ok(account) => Ok((account, linked)),
        Err(provider) => Err(LinkError::Taken {
            address: goa_account.identity.trim().to_string(),
            provider,
        }),
    }
}
