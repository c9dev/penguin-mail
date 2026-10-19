//! Linking a Gmail account to the desktop's Online Accounts: checking
//! that its token answers, and keeping the account.

use std::sync::Arc;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, EpochMillis, GmailSignIn, Provider};
use mailrs_store::{StoreError, accounts};

use crate::Connector;
use crate::goa::{GoaAccount, GoaError, GoaTokens, OnlineAccounts};

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
    Goa(GoaError),
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

/// Asks Online Accounts for the account's token, so a sign-in it lost
/// shows before the account is kept. The mail itself goes over Gmail's
/// IMAP and SMTP with that token.
pub async fn check_token(
    goa: &Arc<dyn OnlineAccounts>,
    goa_account: &GoaAccount,
) -> Result<(), LinkError> {
    let tokens = GoaTokens::new(Arc::clone(goa), goa_account.id.clone());
    match tokens.token().await {
        Ok(_) => Ok(()),
        Err(GoaError::NotAuthorized | GoaError::NoAccount) => Err(LinkError::SignInAgain),
        Err(GoaError::Unreachable(_)) => Err(LinkError::Unreachable),
        Err(other) => Err(LinkError::Goa(other)),
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
/// an account on `Goa` and the IMAP route; an address the store already
/// holds is left as it is, for the caller to move.
pub async fn link(
    connector: &Connector,
    goa_account: &GoaAccount,
    now: EpochMillis,
) -> Result<Account, LinkError> {
    usable(goa_account)?;
    check_token(&connector.goa, goa_account).await?;
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
            accounts::set_gmail_sign_in(c, id, &GmailSignIn::OnlineAccounts { goa_id })?;
            // The row was written a line above in the same transaction.
            let account = accounts::account(c, id)?
                .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
            Ok(Ok(account))
        })
        .await?;
    match kept {
        Ok(account) => Ok(account),
        Err(provider) => Err(LinkError::Taken {
            address: goa_account.identity.trim().to_string(),
            provider,
        }),
    }
}
