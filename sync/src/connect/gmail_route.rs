//! A Gmail account whose mail comes over IMAP: the mail from a seam, the
//! calendar and contacts from Google when the account holds a token.

use std::sync::Arc;

use mailrs_domain::Account;

use super::{Connector, Lacks, connect_account};
use crate::services::{AnyGmail, AnyIdentities, AnyMail, GmailImap, Google};
use crate::sign_in::client_of;
use crate::{AccountServices, AnyRules, BackendError, LocalRules, SyncError};

/// Gmail's IMAP adapter and the address it sends from, or `None` while
/// nothing builds them. The app-password plan builds the adapter here.
pub(crate) async fn gmail_imap_mail(
    _connector: &Connector,
    _account: &Account,
) -> Result<Option<(AnyMail, AnyIdentities)>, SyncError> {
    Ok(None)
}

/// The services of an account on the IMAP route. A missing client or
/// token, or a refused one, leaves the Google side off and writes no
/// state: the account's mail does not depend on it.
pub(super) async fn connect_imap_route(
    connector: &Connector,
    account: &Account,
) -> Result<Result<AccountServices, Lacks>, SyncError> {
    let Some((mail, identities)) = gmail_imap_mail(connector, account).await? else {
        return Ok(Err(Lacks::Secret));
    };
    let google = google_side(connector, account).await?;
    Ok(Ok(AccountServices::gmail_imap(GmailImap {
        mail,
        identities,
        rules: AnyRules::Local(LocalRules::new(connector.db.clone(), account.id)),
        google,
    })))
}

pub(crate) async fn google_side(
    connector: &Connector,
    account: &Account,
) -> Result<Option<Google<AnyGmail>>, SyncError> {
    let kept = Arc::clone(&connector.secrets.google_clients);
    let Some(oauth) = client_of(
        &connector.db,
        &connector.config,
        connector.clients.google.clone(),
        kept,
        account,
    )
    .await?
    else {
        return Ok(None);
    };
    match connect_account(
        oauth,
        Arc::clone(&connector.secrets.google),
        account,
        &connector.db,
    )
    .await
    {
        Ok(client) => Ok(Some(Google::new(Arc::new(client.into())))),
        Err(SyncError::Backend(BackendError::NeedsReauth)) => Ok(None),
        Err(err) => Err(err),
    }
}
