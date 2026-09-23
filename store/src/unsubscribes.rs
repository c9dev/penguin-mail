//! The mailing lists the person left, one row per sender.
//!
//! A list is known here by the address its mail comes from, lower-cased,
//! which is also what the newsletters list groups by. Leaving the same
//! list again keeps the latest way and time.

use std::collections::HashMap;

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// How a list was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The RFC 8058 request to the list's server.
    OneClick,
    /// The list's own unsubscribe page, submitted for the person.
    Page,
    /// A request mail sent to the list.
    Email,
}

impl How {
    /// The word the table and the assistant use.
    pub fn as_str(self) -> &'static str {
        match self {
            How::OneClick => "one_click",
            How::Page => "page",
            How::Email => "email",
        }
    }

    fn parse(word: &str) -> How {
        match word {
            "one_click" => How::OneClick,
            "email" => How::Email,
            _ => How::Page,
        }
    }
}

/// When and how one list was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Left {
    pub how: How,
    pub at: EpochMillis,
}

/// Notes that the person left the list `sender` writes from.
pub fn record(
    conn: &Connection,
    account_id: AccountId,
    sender: &str,
    how: How,
    at: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT INTO unsubscribes (account_id, sender, how, left_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (account_id, sender) DO UPDATE SET how = excluded.how, left_at = excluded.left_at",
        params![account_id, sender.to_lowercase(), how.as_str(), at],
    )?;
    Ok(())
}

/// When and how the person left the list `sender` writes from, if they
/// did.
pub fn left(conn: &Connection, account_id: AccountId, sender: &str) -> Result<Option<Left>> {
    let row = conn
        .query_row(
            "SELECT how, left_at FROM unsubscribes WHERE account_id = ?1 AND sender = ?2",
            params![account_id, sender.to_lowercase()],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(how, at)| Left {
        how: How::parse(&how),
        at,
    }))
}

/// Every list the account left, by sender.
pub fn all(conn: &Connection, account_id: AccountId) -> Result<HashMap<String, Left>> {
    let mut stmt =
        conn.prepare("SELECT sender, how, left_at FROM unsubscribes WHERE account_id = ?1")?;
    let rows = stmt.query_map(params![account_id], |row| {
        let how: String = row.get(1)?;
        Ok((
            row.get::<_, String>(0)?,
            Left {
                how: How::parse(&how),
                at: row.get(2)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
