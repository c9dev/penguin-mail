//! A Gmail account whose mail comes over IMAP, the calendar and contacts
//! from Google when the account holds a token. Also the Gmail account linked
//! through the desktop's Online Accounts, whose mail signs in with the token
//! Online Accounts hands out.

use std::pin::Pin;
use std::sync::Arc;

use mailrs_domain::Account;
use mailrs_imap::{ImapError, Login, LoginSource};
use mailrs_store::accounts;

use mailrs_discover::{Security, Server, UserName};
use mailrs_imap::{ImapClient, SmtpClient};
use mailrs_store::servers;

use super::{Connector, Lacks, STEP_WAIT, bounded, connect_account, read_secret, server_of};
use crate::goa::{self, GoaAccount, GoaError, GoaTokens};
use crate::services::{
    AnyCalendar, AnyContacts, AnyGmail, AnyIdentities, AnyMail, GmailImapParts, Google, NoMail,
    gmail_over_imap,
};
use crate::sign_in::client_of;
use crate::{AccountServices, AnyRules, BackendError, LocalRules, SyncError};

/// How a Gmail account on the IMAP route signs in to Gmail's servers.
pub(crate) enum GmailImapSignIn {
    /// The app password the account's servers row keeps.
    Password,
    /// A login per connection from `source`, for IMAP (XOAUTH2, else
    /// OAUTHBEARER) and SMTP (XOAUTH2) alike.
    Token(Arc<dyn LoginSource>),
}

/// Gmail's IMAP adapter and the address it sends from, or `None` while
/// the account has no servers or no app password saved. `Password` reads the
/// servers row and the app password; `Token` asks its source for a login
/// on each connection and talks to Gmail's own servers.
pub(crate) async fn gmail_imap_mail(
    connector: &Connector,
    account: &Account,
    sign_in: GmailImapSignIn,
) -> Result<Option<(AnyMail, AnyIdentities)>, SyncError> {
    let (imap, smtp) = match sign_in {
        GmailImapSignIn::Password => {
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
            let (Some(saved), Some(kept)) = (saved, kept) else {
                return Ok(None);
            };
            let imap_login = Login::new(saved.imap.user_name.as_str(), kept.as_str());
            let smtp_login = Login::new(saved.smtp.user_name.as_str(), kept.as_str());
            (
                ImapClient::new(server_of(&saved.imap), imap_login),
                SmtpClient::new(&server_of(&saved.smtp), &smtp_login)
                    .map_err(BackendError::from)?,
            )
        }
        GmailImapSignIn::Token(source) => {
            let gmail = |host: &str, port| Server {
                host: host.into(),
                port,
                security: Security::Tls,
                user_name: UserName::Address,
            };
            (
                ImapClient::from_source(gmail("imap.gmail.com", 993), Arc::clone(&source)),
                SmtpClient::from_source(&gmail("smtp.gmail.com", 465), source)
                    .map_err(BackendError::from)?,
            )
        }
    };
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
    imap_services(connector, account, GmailImapSignIn::Password).await
}

async fn imap_services(
    connector: &Connector,
    account: &Account,
    sign_in: GmailImapSignIn,
) -> Result<Result<AccountServices, Lacks>, SyncError> {
    let Some((mail, identities)) = gmail_imap_mail(connector, account, sign_in).await? else {
        return Ok(Err(Lacks::Secret));
    };
    let google = google_side(connector, account).await?;
    Ok(Ok(AccountServices::gmail_imap(GmailImapParts {
        mail,
        identities,
        rules: AnyRules::Local(LocalRules::new(connector.db.clone(), account.id)),
        google,
    })))
}

/// Online Accounts' access tokens as IMAP and SMTP logins.
pub(crate) struct GoaLogin {
    tokens: Arc<GoaTokens>,
    user: String,
}

impl std::fmt::Debug for GoaLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoaLogin")
            .field("user", &self.user)
            .finish()
    }
}

impl LoginSource for GoaLogin {
    fn login(
        &self,
        refused: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Login, ImapError>> + Send + '_>> {
        Box::pin(async move {
            let token = if refused {
                self.tokens.renew().await
            } else {
                self.tokens.token().await
            };
            token
                .map(|token| Login::bearer(self.user.clone(), token))
                .map_err(|err| match err {
                    GoaError::NotAuthorized | GoaError::NoAccount => ImapError::Auth {
                        text: "Online Accounts needs a new sign-in".into(),
                    },
                    other => ImapError::Network(other.to_string()),
                })
        })
    }
}

/// How `account`'s IMAP mail signs in when it is read through `goa_account`.
pub(crate) fn goa_imap_sign_in(
    connector: &Connector,
    account: &Account,
    goa_account: &GoaAccount,
) -> GmailImapSignIn {
    let tokens = Arc::new(GoaTokens::new(
        Arc::clone(&connector.goa),
        goa_account.id.clone(),
    ));
    GmailImapSignIn::Token(Arc::new(GoaLogin {
        tokens,
        user: account.email.clone(),
    }))
}

/// The services of a Gmail account linked through Online Accounts. The
/// mail goes over Gmail's IMAP and SMTP, signed in with Online Accounts'
/// token (XOAUTH2); the calendar and contacts
/// use the built-in client's own token, as for any account. An account
/// Online Accounts no longer holds, or has switched off or wants signed in
/// again, keeps its stored mail and its calendar and reads no new mail.
/// So does one whose Online Accounts does not answer; the app's watch
/// starts it again once Online Accounts answers.
pub(crate) async fn goa_services(
    connector: &Connector,
    account: &Account,
    goa_id: String,
) -> Result<Result<AccountServices, Lacks>, SyncError> {
    let list = match connector.goa.google_accounts().await {
        Ok(list) => list,
        Err(err) => {
            tracing::info!(
                account = account.id,
                error = %err,
                "Online Accounts does not answer; starting without mail"
            );
            return Ok(Ok(without_mail(connector, account).await?));
        }
    };
    let Some(found) = goa::find(&list, Some(&goa_id), &account.email) else {
        tracing::info!(
            account = account.id,
            "Online Accounts no longer holds this account"
        );
        return Ok(Ok(without_mail(connector, account).await?));
    };
    if goa_id != found.id {
        let (id, old, new) = (account.id, goa_id.clone(), found.id.clone());
        connector
            .db
            .write(move |c| accounts::relink_goa(c, id, &old, &new))
            .await?;
    }
    if found.mail_disabled || found.attention_needed {
        tracing::info!(
            account = account.id,
            mail_disabled = found.mail_disabled,
            "Online Accounts does not serve this account's mail now"
        );
        return Ok(Ok(without_mail(connector, account).await?));
    }
    let sign_in = goa_imap_sign_in(connector, account, found);
    imap_services(connector, account, sign_in).await
}

/// The calendar and contacts of `account` with no mail.
async fn without_mail(
    connector: &Connector,
    account: &Account,
) -> Result<AccountServices, SyncError> {
    let google = google_side(connector, account).await?;
    Ok(AccountServices {
        mail: AnyMail::NoMail(NoMail),
        calendar: google.clone().map(AnyCalendar::Google),
        contacts: google.map(AnyContacts::Google),
        rules: None,
        auto_reply: None,
        identities: AnyIdentities::NoMail(NoMail),
    })
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
