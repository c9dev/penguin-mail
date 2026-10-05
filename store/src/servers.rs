//! Where an IMAP or POP3 account's mail lives: one incoming and one outgoing
//! server, each with how to reach it and whom to log in as. The password
//! is in the keyring and never here.

use mailrs_domain::AccountId;
use rusqlite::{Connection, params};

use crate::{Result, StoreError};

/// How a connection is encrypted. There is no plain variant: nothing
/// stored here may be reached without TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte, on 993 or 465.
    Tls,
    /// A plain greeting upgraded with STARTTLS, on 143 or 587.
    StartTls,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::StartTls => "starttls",
        }
    }

    fn parse(stored: &str) -> Option<Security> {
        match stored {
            "tls" => Some(Security::Tls),
            "starttls" => Some(Security::StartTls),
            _ => None,
        }
    }
}

/// One server as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// The user name the server took at the last sign-in, as it was sent.
    pub user_name: String,
}

/// An IMAP account's two servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Servers {
    pub imap: Saved,
    pub smtp: Saved,
}

/// A POP3 account's two servers. It keeps a 'pop3' row where an IMAP
/// account keeps an 'imap' one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pop3Servers {
    pub pop3: Saved,
    pub smtp: Saved,
}

/// Keeps `servers` for `account_id`, replacing what was there.
pub fn save(conn: &Connection, account_id: AccountId, servers: &Servers) -> Result<()> {
    put(conn, account_id, "imap", &servers.imap)?;
    put(conn, account_id, "smtp", &servers.smtp)
}

/// Keeps a POP3 account's servers, replacing what was there.
pub fn save_pop3(conn: &Connection, account_id: AccountId, servers: &Pop3Servers) -> Result<()> {
    put(conn, account_id, "pop3", &servers.pop3)?;
    put(conn, account_id, "smtp", &servers.smtp)
}

/// An IMAP account's servers, or `None` unless both are there. A POP3
/// account answers `None` here and its servers through [`load_pop3`].
pub fn load(conn: &Connection, account_id: AccountId) -> Result<Option<Servers>> {
    let (mut imap, mut smtp, mut pop3) = (None, None, None);
    for (role, saved) in rows(conn, account_id)? {
        match role.as_str() {
            "imap" => imap = Some(saved),
            "smtp" => smtp = Some(saved),
            _ => pop3 = Some(saved),
        }
    }
    if pop3.is_some() {
        return Ok(None);
    }
    Ok(imap.zip(smtp).map(|(imap, smtp)| Servers { imap, smtp }))
}

/// A POP3 account's servers, or `None` unless both are there.
pub fn load_pop3(conn: &Connection, account_id: AccountId) -> Result<Option<Pop3Servers>> {
    let (mut pop3, mut smtp) = (None, None);
    for (role, saved) in rows(conn, account_id)? {
        match role.as_str() {
            "pop3" => pop3 = Some(saved),
            "smtp" => smtp = Some(saved),
            _ => {}
        }
    }
    Ok(pop3.zip(smtp).map(|(pop3, smtp)| Pop3Servers { pop3, smtp }))
}

fn put(conn: &Connection, account_id: AccountId, role: &str, server: &Saved) -> Result<()> {
    conn.execute(
        "INSERT INTO account_servers (account_id, role, host, port, security, user_name) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT (account_id, role) DO UPDATE SET host = excluded.host, \
         port = excluded.port, security = excluded.security, user_name = excluded.user_name",
        params![account_id, role, server.host, server.port, server.security.as_str(), server.user_name],
    )?;
    Ok(())
}

/// Every server row kept for `account_id`, by role.
fn rows(conn: &Connection, account_id: AccountId) -> Result<Vec<(String, Saved)>> {
    let mut stmt = conn.prepare(
        "SELECT role, host, port, security, user_name FROM account_servers WHERE account_id = ?1",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut kept = Vec::new();
    for row in rows {
        let (role, host, port, security, user_name) = row?;
        if !matches!(role.as_str(), "imap" | "smtp" | "pop3") {
            return Err(StoreError::Corrupt { column: "account_servers.role", value: role });
        }
        let port = u16::try_from(port).map_err(|_| StoreError::Corrupt {
            column: "account_servers.port",
            value: port.to_string(),
        })?;
        let security = Security::parse(&security).ok_or_else(|| StoreError::Corrupt {
            column: "account_servers.security",
            value: security.clone(),
        })?;
        kept.push((role, Saved { host, port, security, user_name }));
    }
    Ok(kept)
}
