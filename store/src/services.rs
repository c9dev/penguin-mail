//! The servers found for an IMAP account beside its mail: a CalDAV and a
//! CardDAV context URL and a ManageSieve host. Discovery fills them, and
//! the person can type the DAV ones in Preferences. When discovery finds
//! no DAV server, the store keeps why. The password is the
//! account's IMAP one, in the keyring and never here.

use mailrs_domain::AccountId;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    CalDav,
    CardDav,
    Sieve,
}

impl ServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceKind::CalDav => "caldav",
            ServiceKind::CardDav => "carddav",
            ServiceKind::Sieve => "sieve",
        }
    }

    fn parse(stored: &str) -> Option<ServiceKind> {
        match stored {
            "caldav" => Some(ServiceKind::CalDav),
            "carddav" => Some(ServiceKind::CardDav),
            "sieve" => Some(ServiceKind::Sieve),
            _ => None,
        }
    }
}

/// One server as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundService {
    pub kind: ServiceKind,
    /// The context URL a DAV principal is asked for at, or `host:port` for
    /// ManageSieve.
    pub url: String,
    /// The user name the server took when it was found.
    pub user_name: String,
    /// The host sits inside the address's domain, came from the provider
    /// table, or the person said yes to it. Only a confirmed server is
    /// used; the others wait in Preferences for a yes.
    pub confirmed: bool,
    /// Where it came from, for the Preferences row: `table`, `srv`,
    /// `well-known`, `probe` or `typed`.
    pub source: String,
}

#[derive(Serialize, Deserialize)]
struct Extra {
    user_name: String,
    confirmed: bool,
    source: String,
}

/// Keeps `found` for `account_id`, replacing the row of its kind.
pub fn save(conn: &Connection, account_id: AccountId, found: &FoundService) -> Result<()> {
    let extra = serde_json::to_string(&Extra {
        user_name: found.user_name.clone(),
        confirmed: found.confirmed,
        source: found.source.clone(),
    })
    .map_err(|err| StoreError::Corrupt { column: "account_services.extra", value: err.to_string() })?;
    conn.execute(
        "INSERT INTO account_services (account_id, service, url, extra) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (account_id, service) DO UPDATE SET url = excluded.url, extra = excluded.extra",
        params![account_id, found.kind.as_str(), found.url, extra],
    )?;
    Ok(())
}

/// Every server kept for `account_id`, CalDAV, CardDAV, then ManageSieve.
pub fn load(conn: &Connection, account_id: AccountId) -> Result<Vec<FoundService>> {
    let mut stmt =
        conn.prepare("SELECT service, url, extra FROM account_services WHERE account_id = ?1")?;
    let rows = stmt.query_map([account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (service, url, extra) = row?;
        let kind = ServiceKind::parse(&service)
            .ok_or_else(|| StoreError::Corrupt { column: "account_services.service", value: service.clone() })?;
        let extra: Extra = serde_json::from_str(&extra)
            .map_err(|_| StoreError::Corrupt { column: "account_services.extra", value: extra.clone() })?;
        found.push(FoundService {
            kind,
            url,
            user_name: extra.user_name,
            confirmed: extra.confirmed,
            source: extra.source,
        });
    }
    found.sort_by_key(|f| f.kind as u8);
    Ok(found)
}

/// Why the last search found no server of a kind. The variants rise in
/// the order a search keeps them: a refused login says more than finding
/// nothing, which says more than reaching nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Miss {
    /// No server answered, or none in time.
    Unreachable,
    /// The servers answered, and none of them as a calendar or contacts
    /// server.
    NotFound,
    /// A server refused the user name or the password.
    Refused,
}

impl Miss {
    fn as_str(self) -> &'static str {
        match self {
            Miss::Unreachable => "unreachable",
            Miss::NotFound => "not_found",
            Miss::Refused => "refused",
        }
    }

    fn parse(stored: &str) -> Option<Miss> {
        match stored {
            "unreachable" => Some(Miss::Unreachable),
            "not_found" => Some(Miss::NotFound),
            "refused" => Some(Miss::Refused),
            _ => None,
        }
    }
}

/// Keeps why the server of `kind` was not found, or with `None` forgets
/// it once one is.
pub fn save_miss(conn: &Connection, account_id: AccountId, kind: ServiceKind, miss: Option<Miss>) -> Result<()> {
    match miss {
        Some(miss) => conn.execute(
            "INSERT INTO service_misses (account_id, service, reason) VALUES (?1, ?2, ?3) \
             ON CONFLICT (account_id, service) DO UPDATE SET reason = excluded.reason",
            params![account_id, kind.as_str(), miss.as_str()],
        )?,
        None => conn.execute(
            "DELETE FROM service_misses WHERE account_id = ?1 AND service = ?2",
            params![account_id, kind.as_str()],
        )?,
    };
    Ok(())
}

/// Every kept miss, by account and then CalDAV before CardDAV.
pub fn all_misses(conn: &Connection) -> Result<Vec<(AccountId, ServiceKind, Miss)>> {
    let mut stmt = conn.prepare("SELECT account_id, service, reason FROM service_misses ORDER BY account_id, service")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, AccountId>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    let mut misses = Vec::new();
    for row in rows {
        let (account_id, service, reason) = row?;
        let kind = ServiceKind::parse(&service)
            .ok_or_else(|| StoreError::Corrupt { column: "service_misses.service", value: service.clone() })?;
        let miss = Miss::parse(&reason)
            .ok_or_else(|| StoreError::Corrupt { column: "service_misses.reason", value: reason.clone() })?;
        misses.push((account_id, kind, miss));
    }
    Ok(misses)
}

/// Forgets the server of `kind`, when the person clears a typed URL.
pub fn remove(conn: &Connection, account_id: AccountId, kind: ServiceKind) -> Result<()> {
    conn.execute(
        "DELETE FROM account_services WHERE account_id = ?1 AND service = ?2",
        params![account_id, kind.as_str()],
    )?;
    Ok(())
}
