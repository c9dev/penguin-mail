use std::sync::Arc;

use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::{Account, AccountState};
use mailrs_gmail::{Granted, GmailClient, GmailError, OAuthClient, TokenStore};
use mailrs_imap::{ImapClient, Login, SmtpClient};
use mailrs_store::servers::{self, Saved, Servers};
use mailrs_store::{Db, accounts};

use crate::passwords::{PasswordError, PasswordStore};
use crate::{AccountClient, AccountServices, BackendError, ImapSettings, SyncError};

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
    let email = account.email.clone();
    let stored = tokio::task::spawn_blocking(move || tokens.load(&email))
        .await
        .map_err(|e| GmailError::Keyring(e.to_string()))??;
    let refresh_token = stored.ok_or(GmailError::NeedsReauth)?;
    let id = account.id;
    let consent = db.read(move |c| accounts::consent(c, id)).await?;
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
        client,
    })
}

/// The services for an IMAP account, from the servers the store keeps
/// for it and the password in `passwords`. Without either, the account
/// needs to sign in again: the store records that before this answers
/// `NeedsReauth`, since the engine never runs the account to say so.
/// Nothing here connects; the clients log in when sync first asks. The
/// provider's sent-copy rule comes from the provider table at each start,
/// so a corrected table reaches accounts added before the correction.
pub async fn connect_imap<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    account: &Account,
    window_days: i64,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let saved = db.read(move |c| servers::load(c, id)).await?;
    let password = tokio::task::spawn_blocking(move || passwords.load(id))
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    let (Some(saved), Some(password)) = (saved, password) else {
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await?;
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
    let smtp = SmtpClient::new(
        &server_of(&saved.smtp),
        &Login::new(saved.smtp.user_name.as_str(), password),
    )
    .map_err(BackendError::from)?;
    Ok(AccountServices::imap(
        imap,
        smtp,
        ImapSettings {
            address: account.email.clone(),
            provider_name,
            files_sent_mail,
            window_days,
        },
    ))
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
