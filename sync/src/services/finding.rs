//! Finding the servers beside an IMAP account's mail: its calendar and
//! contacts over CalDAV and CardDAV, and its rules over ManageSieve. The
//! hints come from discovery (the provider table, RFC 6764, the
//! well-known URLs, port 4190 on the IMAP host); each is tried with the
//! account's login, the IMAP user name first and the whole address
//! second, ten seconds a try. A hint outside the address's domain is kept
//! unconfirmed and gets the password only after the person says yes.

use std::future::Future;
use std::time::Duration;

use mailrs_dav::{DavApi, DavClient, Kind};
use mailrs_discover::{Hint, Net, SieveHint, Source};
use mailrs_domain::AccountId;
use mailrs_sieve::client::{ManageSieveApi, ManageSieveClient, SieveError};
use mailrs_sieve::script::Extensions;
use mailrs_store::Db;
use mailrs_store::services::{self, FoundService, ServiceKind};

use crate::{BackendError, SyncError};

const PROBE_LIMIT: Duration = Duration::from_secs(10);

pub trait ServiceProbe: Send + Sync {
    fn dav(&self, url: &str, user: &str, password: &str, kind: Kind) -> impl Future<Output = Result<(), BackendError>> + Send;
    fn sieve(&self, host: &str, port: u16, user: &str, password: &str) -> impl Future<Output = Result<Extensions, BackendError>> + Send;
}

/// The real clients.
pub struct RealProbe;

impl ServiceProbe for RealProbe {
    async fn dav(&self, url: &str, user: &str, password: &str, kind: Kind) -> Result<(), BackendError> {
        let client = DavClient::new(url, kind, mailrs_dav::Login::new(user, password)).map_err(|e| BackendError::Refused(e.to_string()))?;
        let homes = client.homes().await.map_err(|e| match e {
            mailrs_dav::DavError::Unauthorized => BackendError::NeedsReauth,
            mailrs_dav::DavError::Network(detail) => BackendError::Offline(detail),
            other => BackendError::Refused(other.to_string()),
        })?;
        let home = match kind {
            Kind::Calendar => homes.calendar,
            Kind::AddressBook => homes.addressbook,
        };
        home.map(drop).ok_or(BackendError::Unsupported)
    }

    async fn sieve(&self, host: &str, port: u16, user: &str, password: &str) -> Result<Extensions, BackendError> {
        let client = ManageSieveClient::new(host, port, mailrs_sieve::client::Login::new(user, password));
        client.capabilities().await.map(|c| c.sieve).map_err(|e| match e {
            SieveError::Auth(_) => BackendError::NeedsReauth,
            SieveError::Network(detail) => BackendError::Offline(detail),
            other => BackendError::Refused(other.to_string()),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoundServices {
    pub caldav: Option<FoundService>,
    pub carddav: Option<FoundService>,
    pub sieve: Option<FoundService>,
}

fn source_word(source: Source) -> &'static str {
    match source {
        Source::Table => "table",
        Source::Srv => "srv",
        Source::WellKnown => "well-known",
        Source::Probe => "probe",
        Source::Manual => "typed",
        _ => "discovery",
    }
}

/// The IMAP user name, then the whole address when it differs.
fn users(imap_user: &str, address: &str) -> Vec<String> {
    let mut users = vec![imap_user.to_string()];
    if !imap_user.eq_ignore_ascii_case(address) {
        users.push(address.to_string());
    }
    users
}

async fn dav<P: ServiceProbe>(probe: &P, hints: &[Hint], users: &[String], password: &str, kind: Kind, service: ServiceKind) -> Option<FoundService> {
    let mut waiting = None;
    for hint in hints {
        if hint.confirm {
            waiting.get_or_insert_with(|| FoundService {
                kind: service,
                url: hint.url.clone(),
                user_name: users[0].clone(),
                confirmed: false,
                source: source_word(hint.source).into(),
            });
            continue;
        }
        for user in users {
            let tried = tokio::time::timeout(PROBE_LIMIT, probe.dav(&hint.url, user, password, kind)).await;
            if matches!(tried, Ok(Ok(()))) {
                return Some(FoundService { kind: service, url: hint.url.clone(), user_name: user.clone(), confirmed: true, source: source_word(hint.source).into() });
            }
        }
    }
    waiting
}

async fn sieve<P: ServiceProbe>(probe: &P, hints: &[SieveHint], users: &[String], password: &str) -> Option<FoundService> {
    for hint in hints.iter().filter(|h| !h.confirm) {
        for user in users {
            let tried = tokio::time::timeout(PROBE_LIMIT, probe.sieve(&hint.host, hint.port, user, password)).await;
            if let Ok(Ok(extensions)) = tried {
                if !extensions.usable() {
                    return None;
                }
                return Some(FoundService {
                    kind: ServiceKind::Sieve,
                    url: format!("{}:{}", hint.host, hint.port),
                    user_name: user.clone(),
                    confirmed: true,
                    source: source_word(hint.source).into(),
                });
            }
        }
    }
    None
}

pub async fn find_services<N: Net, P: ServiceProbe>(net: &N, probe: &P, address: &str, provider_name: &str, imap_host: &str, imap_user: &str, password: &str) -> FoundServices {
    let hints = mailrs_discover::dav_hints(net, address, provider_name, imap_host).await;
    let users = users(imap_user, address);
    let (caldav, carddav, sieve) = tokio::join!(
        dav(probe, &hints.caldav, &users, password, Kind::Calendar, ServiceKind::CalDav),
        dav(probe, &hints.carddav, &users, password, Kind::AddressBook, ServiceKind::CardDav),
        sieve(probe, &hints.sieve, &users, password),
    );
    FoundServices { caldav, carddav, sieve }
}

/// Keeps what was found, leaving a URL the person typed as it is and a
/// server found before when nothing was found now. Answers whether a
/// confirmed server is new or moved, so the account's services need
/// building again.
pub async fn keep_found(db: &Db, account_id: AccountId, found: &FoundServices) -> Result<bool, SyncError> {
    let found = found.clone();
    Ok(db
        .write(move |c| {
            let held = services::load(c, account_id)?;
            let mut changed = false;
            for new in [found.caldav, found.carddav, found.sieve].into_iter().flatten() {
                let old = held.iter().find(|h| h.kind == new.kind);
                if old.is_some_and(|o| o.source == "typed") || old == Some(&new) {
                    continue;
                }
                changed |= new.confirmed;
                services::save(c, account_id, &new)?;
            }
            Ok(changed)
        })
        .await?)
}

/// A URL the person typed for `kind`, checked with the login and kept.
pub async fn use_typed<P: ServiceProbe>(probe: &P, db: &Db, account_id: AccountId, kind: ServiceKind, url: &str, user: &str, password: &str) -> Result<FoundService, SyncError> {
    let url = url.trim();
    if url.starts_with("http://") {
        return Err(BackendError::Refused(mailrs_domain::translate::gettext("Penguin Mail reaches calendar and contact servers over HTTPS only.")).into());
    }
    let url = match url.contains("://") {
        true => url.to_string(),
        false => format!("https://{url}"),
    };
    let url = match url.trim_end_matches('/').matches('/').count() {
        // "https://host" becomes "https://host/".
        2 => format!("{}/", url.trim_end_matches('/')),
        _ => url,
    };
    let dav_kind = match kind {
        ServiceKind::CalDav => Kind::Calendar,
        ServiceKind::CardDav => Kind::AddressBook,
        ServiceKind::Sieve => return Err(BackendError::Unsupported.into()),
    };
    tokio::time::timeout(PROBE_LIMIT, probe.dav(&url, user, password, dav_kind))
        .await
        .map_err(|_| BackendError::Offline("the server took too long".into()))??;
    let found = FoundService { kind, url, user_name: user.to_string(), confirmed: true, source: "typed".into() };
    let kept = found.clone();
    db.write(move |c| services::save(c, account_id, &kept)).await?;
    Ok(found)
}

/// The person said yes to the server kept for `kind`: try the login
/// there now, and use it once it works.
pub async fn confirm_found<P: ServiceProbe>(probe: &P, db: &Db, account_id: AccountId, kind: ServiceKind, password: &str) -> Result<(), SyncError> {
    let held = db.read(move |c| services::load(c, account_id)).await?;
    let mut row = held.into_iter().find(|h| h.kind == kind).ok_or(BackendError::NotFound)?;
    match kind {
        ServiceKind::CalDav | ServiceKind::CardDav => {
            let dav_kind = if kind == ServiceKind::CalDav { Kind::Calendar } else { Kind::AddressBook };
            tokio::time::timeout(PROBE_LIMIT, probe.dav(&row.url, &row.user_name, password, dav_kind))
                .await
                .map_err(|_| BackendError::Offline("the server took too long".into()))??;
        }
        ServiceKind::Sieve => return Err(BackendError::Unsupported.into()),
    }
    row.confirmed = true;
    db.write(move |c| services::save(c, account_id, &row)).await?;
    Ok(())
}
