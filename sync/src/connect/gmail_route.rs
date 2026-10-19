//! A Gmail account whose mail comes over IMAP: the mail from a seam, the
//! calendar and contacts from Google when the account holds a token. Also
//! the Gmail account linked through the desktop's Online Accounts, whose
//! mail signs in with the token Online Accounts hands out.

use std::pin::Pin;
use std::sync::{Arc, LazyLock};

use futures::future::BoxFuture;
use mailrs_domain::{Account, MailRoute};
use mailrs_gmail::{AccessSource, GmailClient, GmailError, Granted, QuotaPool};
use mailrs_imap::{ImapError, Login, LoginSource};
use mailrs_store::accounts;

use super::{Connector, Lacks, connect_account};
use crate::goa::{self, GNOME_GOOGLE_SCOPES, GoaAccount, GoaError, GoaTokens};
use crate::services::{
    AnyCalendar, AnyContacts, AnyGmail, AnyIdentities, AnyMail, GmailImap, Google, NoMail,
};
use crate::sign_in::client_of;
use crate::{AccountClient, AccountServices, AnyRules, BackendError, LocalRules, SyncError};

/// How a Gmail account on the IMAP route signs in to Gmail's servers.
// Nothing reads the source until the Gmail IMAP adapter lands.
#[expect(dead_code)]
pub(crate) enum GmailImapSignIn {
    /// The app password the account's servers row keeps.
    Password,
    /// A login per connection from `source`, for IMAP (OAUTHBEARER) and
    /// SMTP (XOAUTH2) alike.
    Token(Arc<dyn LoginSource>),
}

/// Gmail's IMAP adapter and the address it sends from, or `None` while
/// nothing builds them. The app-password plan builds the adapter here.
///
/// To wire once `gmail-app-password` merges: build the adapter with
/// `Dialect::Gmail`; for `Password` read the secret the way that branch
/// does, and for `Token(source)` make the IMAP client with
/// `ImapClient::with_source(dial, source.clone())` and the SMTP client with
/// `SmtpClient::from_source(&server, source)`. Then return `Some`.
pub(crate) async fn gmail_imap_mail(
    _connector: &Connector,
    _account: &Account,
    _sign_in: GmailImapSignIn,
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
    Ok(Ok(AccountServices::gmail_imap(GmailImap {
        mail,
        identities,
        rules: AnyRules::Local(LocalRules::new(connector.db.clone(), account.id)),
        google,
    })))
}

/// Google's quota for the accounts read through Online Accounts. GNOME's
/// project is not the built-in client's, so it has a pool of its own.
static GOA_QUOTA: LazyLock<QuotaPool> = LazyLock::new(QuotaPool::new);

/// Online Accounts' access tokens for the Gmail API.
pub(crate) struct GoaAccess(pub(crate) Arc<GoaTokens>);

impl AccessSource for GoaAccess {
    fn access(&self, renew: bool) -> BoxFuture<'_, Result<String, GmailError>> {
        Box::pin(async move {
            let token = if renew {
                self.0.renew().await
            } else {
                self.0.token().await
            };
            token.map_err(|err| match err {
                GoaError::NotAuthorized | GoaError::NoAccount => GmailError::NeedsReauth,
                other => GmailError::Network(other.to_string()),
            })
        })
    }
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
/// mail signs in with Online Accounts' token; the calendar and contacts
/// use the built-in client's own token, as for any account. An account
/// Online Accounts no longer holds, or has switched off or wants signed in
/// again, keeps its stored mail and its calendar and reads no new mail.
pub(crate) async fn goa_services(
    connector: &Connector,
    account: &Account,
    route: MailRoute,
    goa_id: Option<String>,
) -> Result<Result<AccountServices, Lacks>, SyncError> {
    let list = connector
        .goa
        .google_accounts()
        .await
        .map_err(|err| SyncError::Backend(BackendError::Offline(err.to_string())))?;
    let Some(found) = goa::find(&list, goa_id.as_deref(), &account.email) else {
        tracing::info!(
            account = account.id,
            "Online Accounts no longer holds this account"
        );
        return Ok(Ok(without_mail(connector, account).await?));
    };
    if goa_id.as_deref() != Some(found.id.as_str()) {
        let (id, new) = (account.id, found.id.clone());
        connector
            .db
            .write(move |c| accounts::set_goa_id(c, id, Some(&new)))
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
    match route {
        MailRoute::GmailApi => {
            let tokens = Arc::new(GoaTokens::new(Arc::clone(&connector.goa), found.id.clone()));
            let client = GmailClient::with_source(
                Arc::new(GoaAccess(tokens)),
                mailrs_gmail::api_http_client(),
                GOA_QUOTA.account(&account.email),
            )
            .with_granted(Some(Granted::parse(GNOME_GOOGLE_SCOPES)));
            let mut services = AccountServices::google(AccountClient {
                account_id: account.id,
                address: account.email.clone(),
                client,
            });
            let google = google_side(connector, account).await?;
            services.calendar = google.clone().map(AnyCalendar::Google);
            services.contacts = google.map(AnyContacts::Google);
            Ok(Ok(services))
        }
        MailRoute::Imap => {
            let sign_in = goa_imap_sign_in(connector, account, found);
            imap_services(connector, account, sign_in).await
        }
    }
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
        when_grant_ends: crate::WhenGrantEnds::SignInAgain,
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
