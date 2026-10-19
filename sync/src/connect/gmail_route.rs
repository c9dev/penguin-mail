//! A Gmail account whose mail comes over IMAP: the mail from a seam, the
//! calendar and contacts from Google when the account holds a token.

use std::sync::Arc;

use mailrs_domain::Account;

use mailrs_imap::{ImapClient, Login, SmtpClient};
use mailrs_store::servers;

use super::{Connector, Lacks, STEP_WAIT, bounded, connect_account, read_secret, server_of};
use crate::services::{AnyGmail, AnyIdentities, AnyMail, GmailImap, Google, gmail_over_imap};
use crate::sign_in::client_of;
use crate::{AccountServices, AnyRules, BackendError, LocalRules, SyncError};

/// Gmail's IMAP adapter and the address it sends from, or `None` while
/// the account has no servers or no app password saved. The sign-in is a
/// `mailrs_imap::Login`; another source of one, such as a token from the
/// desktop's online accounts, goes through [`gmail_over_imap`] the same way.
pub(crate) async fn gmail_imap_mail(
    connector: &Connector,
    account: &Account,
) -> Result<Option<(AnyMail, AnyIdentities)>, SyncError> {
    let id = account.id;
    let db = connector.db.clone();
    let saved = bounded(
        id,
        "the mail store",
        STEP_WAIT,
        db.read(move |c| servers::load(c, id)),
    )
    .await?;
    let kept = read_secret(Arc::clone(&connector.secrets.passwords), id, STEP_WAIT).await?;
    let (Some(saved), Some(password)) = (saved, kept) else {
        return Ok(None);
    };
    let imap = ImapClient::new(
        server_of(&saved.imap),
        Login::new(saved.imap.user_name.as_str(), password.as_str()),
    );
    let smtp = SmtpClient::new(
        &server_of(&saved.smtp),
        &Login::new(saved.smtp.user_name.as_str(), password.as_str()),
    )
    .map_err(BackendError::from)?;
    let window_days = connector.config.engine_config().window_days;
    Ok(Some(gmail_over_imap(
        imap,
        smtp,
        &account.email,
        window_days,
    )))
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
