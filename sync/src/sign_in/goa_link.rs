//! Linking a Gmail account to the desktop's Online Accounts: checking
//! that its token answers, and keeping the account.

use std::sync::Arc;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, EpochMillis, GmailSignIn};
use mailrs_store::{StoreError, accounts};

use super::change::{Change, ChangeError, Whose, change_sign_in};
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
    #[error("{0}")]
    Keyring(String),
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
/// holds is taken.
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
    let email = address.clone();
    let held = connector
        .db
        .read(move |c| accounts::account_by_email(c, &email))
        .await?;
    if let Some(held) = held {
        return Err(LinkError::Taken {
            address: goa_account.identity.trim().to_string(),
            provider: held.provider_name().to_string(),
        });
    }
    let to = GmailSignIn::OnlineAccounts {
        goa_id: goa_account.id.clone(),
    };
    let whose = Whose::New {
        email: address,
        now,
    };
    let id = change_sign_in(&connector.db, &connector.secrets, whose, Change::to(to)).await?;
    connector
        .db
        .read(move |c| accounts::account(c, id))
        .await?
        .ok_or(LinkError::Store(StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

impl From<ChangeError> for LinkError {
    fn from(err: ChangeError) -> Self {
        match err {
            ChangeError::Store(err) => LinkError::Store(err),
            ChangeError::Taken { address, provider } => LinkError::Taken { address, provider },
            // A link brings no secret, so the keyring never refuses one.
            other => LinkError::Keyring(other.to_string()),
        }
    }
}
