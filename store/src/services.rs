//! The servers found for an IMAP account beside its mail: a CalDAV and a
//! CardDAV context URL and a ManageSieve host. Discovery fills them, and
//! the person can type the DAV ones in Preferences. The password is the
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

/// Forgets the server of `kind`, when the person clears a typed URL.
pub fn remove(conn: &Connection, account_id: AccountId, kind: ServiceKind) -> Result<()> {
    conn.execute(
        "DELETE FROM account_services WHERE account_id = ?1 AND service = ?2",
        params![account_id, kind.as_str()],
    )?;
    Ok(())
}
