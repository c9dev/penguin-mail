use std::sync::Arc;

use mailrs_domain::Account;
use mailrs_gmail::{GmailClient, GmailError, OAuthClient, TokenStore};

use crate::{AccountClient, SyncError};

/// A Gmail client for `account`, built from its refresh token in `tokens`.
/// Fails with `NeedsReauth` when no token is stored.
pub async fn connect_account(
    oauth: OAuthClient,
    tokens: Arc<dyn TokenStore>,
    account: &Account,
) -> Result<AccountClient, SyncError> {
    let email = account.email.clone();
    let stored = tokio::task::spawn_blocking(move || tokens.load(&email))
        .await
        .map_err(|e| GmailError::Keyring(e.to_string()))??;
    let refresh_token = stored.ok_or(GmailError::NeedsReauth)?;
    Ok(AccountClient {
        account_id: account.id,
        client: GmailClient::for_account(oauth, refresh_token, &account.email),
    })
}
