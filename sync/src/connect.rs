use std::sync::Arc;

use mailrs_dav::{DavClient, Login as DavLogin};
use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::{Account, AccountState};
use mailrs_gmail::{Granted, GmailClient, GmailError, OAuthClient, TokenStore};
use mailrs_graph::MicrosoftClient;
use mailrs_imap::{ImapClient, Login, SmtpClient};
use mailrs_sieve::client::{Login as SieveLogin, ManageSieveClient};
use mailrs_store::servers::{self, Saved, Servers};
use mailrs_store::services::{FoundService, ServiceKind};
use mailrs_store::{Db, accounts};

use crate::passwords::{PasswordError, PasswordStore};
use crate::{
    AccountClient, AccountServices, AnyAutoReply, AnyCalendar, AnyContacts, AnyRules, BackendError,
    CalDav, CardDav, ImapSettings, LocalRules, MicrosoftSettings, SieveRules, SyncError,
};

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

/// The services for a Microsoft account, from the refresh token under its
/// id and the scopes the store last recorded. No token means the account
/// needs to sign in again, which the store records before this answers.
/// Each token Microsoft rotates goes back to the keyring off the runtime,
/// and each change of scopes back to the store.
pub async fn connect_microsoft<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    account: &Account,
    window_days: i64,
) -> Result<AccountServices, SyncError> {
    connect_microsoft_at(db, tokens, client, account, window_days, mailrs_graph::GRAPH_BASE).await
}

/// [`connect_microsoft`] against `base`, which tests point at a mock.
pub async fn connect_microsoft_at<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    account: &Account,
    window_days: i64,
    base: &str,
) -> Result<AccountServices, SyncError> {
    let id = account.id;
    let held = Arc::clone(&tokens);
    let refresh = tokio::task::spawn_blocking(move || held.load(id))
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))??;
    let Some(refresh) = refresh else {
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await?;
        return Err(BackendError::NeedsReauth.into());
    };
    let consent = db.read(move |c| accounts::consent(c, id)).await?;
    let granted = consent.granted.as_deref().map(mailrs_graph::Granted::parse);
    let saver = Arc::clone(&tokens);
    let store = db.clone();
    let session = mailrs_graph::Session::new(client, refresh)
        .with_granted(granted)
        .on_rotated(move |token| {
            let saver = Arc::clone(&saver);
            tokio::spawn(async move {
                let saved = tokio::task::spawn_blocking(move || saver.save(id, &token)).await;
                if !matches!(saved, Ok(Ok(()))) {
                    tracing::warn!(account = id, "could not keep Microsoft's new refresh token");
                }
            });
        })
        .on_granted(move |granted| {
            let (store, scope) = (store.clone(), granted.to_scope());
            tokio::spawn(async move {
                if let Err(err) = store.write(move |c| accounts::set_granted(c, id, &scope)).await {
                    tracing::warn!(account = id, %err, "could not save the granted scopes");
                }
            });
        });
    let graph = mailrs_graph::Graph::with_base(Arc::new(session), base)
        .map_err(crate::services::microsoft::backend)?;
    Ok(AccountServices::microsoft(
        graph,
        MicrosoftSettings {
            address: account.email.clone(),
            provider_name: account.provider_name().to_string(),
            window_days,
        },
    ))
}

/// The services for an IMAP account, from the servers the store keeps
/// for it and the password in `passwords`. Without either, the account
/// needs to sign in again: the store records that before this answers
/// `NeedsReauth`, since the engine never runs the account to say so.
/// It adds the calendar, contacts and rules servers found for the account,
/// each only once confirmed; an account whose server runs no rules keeps
/// them on this computer. Nothing here connects; the clients log in when
/// sync first asks. The
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
    let secret = password.clone();
    let smtp = SmtpClient::new(
        &server_of(&saved.smtp),
        &Login::new(saved.smtp.user_name.as_str(), password),
    )
    .map_err(BackendError::from)?;
    let mut services = AccountServices::imap(
        imap,
        smtp,
        ImapSettings {
            address: account.email.clone(),
            provider_name: provider_name.clone(),
            files_sent_mail,
            window_days,
        },
    );
    let found = db.read(move |c| mailrs_store::services::load(c, id)).await?;
    // The password goes only to a server the person confirmed.
    for service in found.into_iter().filter(|f| f.confirmed) {
        services = attach(services, &service, &secret, account, &provider_name);
    }
    if services.rules.is_none() {
        services = services.with_rules(AnyRules::Local(LocalRules::new(db.clone(), id)));
    }
    Ok(services)
}

/// `services` with the server `service` names put behind the service it
/// offers. A server that cannot be set up is logged and left out, so the
/// account keeps its mail.
fn attach(
    services: AccountServices,
    service: &FoundService,
    secret: &str,
    account: &Account,
    provider_name: &str,
) -> AccountServices {
    let id = account.id;
    match service.kind {
        ServiceKind::CalDav => {
            let Some(mail) = services.imap_adapter() else {
                return services;
            };
            match DavClient::new(&service.url, DavLogin::new(&service.user_name, secret)) {
                Ok(client) => {
                    let calendar = CalDav::new(Arc::new(client), mail, vec![account.email.clone()]);
                    services.with_calendar(AnyCalendar::Dav(calendar))
                }
                Err(err) => {
                    tracing::warn!(account = id, %err, "could not set up the calendar server");
                    services
                }
            }
        }
        ServiceKind::CardDav => {
            match DavClient::new(&service.url, DavLogin::new(&service.user_name, secret)) {
                Ok(client) => services.with_contacts(AnyContacts::Dav(CardDav::new(Arc::new(client)))),
                Err(err) => {
                    tracing::warn!(account = id, %err, "could not set up the contacts server");
                    services
                }
            }
        }
        ServiceKind::Sieve => {
            let Some(mail) = services.imap_adapter() else {
                return services;
            };
            let (host, port) = match service.url.rsplit_once(':') {
                Some((host, port)) => (host, port.parse().unwrap_or(mailrs_sieve::PORT)),
                None => (service.url.as_str(), mailrs_sieve::PORT),
            };
            let client = ManageSieveClient::new(host, port, SieveLogin::new(&service.user_name, secret));
            let rules = SieveRules::new(
                Arc::new(client),
                mail,
                account.email.clone(),
                provider_name.to_string(),
            );
            services.with_rules(AnyRules::Sieve(rules.clone())).with_auto_reply(AnyAutoReply::Sieve(rules))
        }
    }
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
