//! Senders whose remote images may load.
//!
//! A remote image tells the sender their mail was opened, so nothing loads
//! until the reader says so. This is where that answer is kept. Like
//! templates, the list belongs to the person rather than to one account.
//!
//! An entry names one address, or a whole domain when the reader allowed
//! everyone who writes from it. A mailing list sends from a different local
//! part every time, which is what the domain entry is for.

use mailrs_domain::EpochMillis;
use rusqlite::{Connection, Row, params};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSender {
    /// A lower-case address, or a lower-case domain when `whole_domain`.
    pub sender: String,
    pub whole_domain: bool,
    pub allowed_at: EpochMillis,
}

fn to_sender(row: &Row<'_>) -> rusqlite::Result<ImageSender> {
    Ok(ImageSender {
        sender: row.get(0)?,
        whole_domain: row.get(1)?,
        allowed_at: row.get(2)?,
    })
}

/// Records that `sender` may load images. Allowing the same one twice
/// moves its date rather than failing.
pub fn allow(conn: &Connection, sender: &str, whole_domain: bool, now: EpochMillis) -> Result<()> {
    let sender = sender.trim().to_lowercase();
    if sender.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO image_senders (sender, whole_domain, allowed_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (sender) DO UPDATE SET whole_domain = excluded.whole_domain, \
         allowed_at = excluded.allowed_at",
        params![sender, whole_domain, now],
    )?;
    Ok(())
}

/// Takes `sender` off the list. Nothing happens when it was never on it.
pub fn forget(conn: &Connection, sender: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM image_senders WHERE sender = ?1",
        params![sender.trim().to_lowercase()],
    )?;
    Ok(())
}

/// Every allowed sender, newest first.
pub fn list(conn: &Connection) -> Result<Vec<ImageSender>> {
    let mut stmt = conn.prepare(
        "SELECT sender, whole_domain, allowed_at FROM image_senders \
         ORDER BY allowed_at DESC, sender",
    )?;
    let rows = stmt
        .query_map([], to_sender)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
