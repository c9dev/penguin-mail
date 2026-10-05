//! The raw bytes of mail that lives only on this computer: a POP3
//! account's messages, and what such an account sends and drafts. A
//! trigger deletes a copy with its message row, so Delete Forever and
//! removing the account take the bytes too.

use mailrs_domain::AccountId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Keeps `raw` as message `message_id`, replacing any copy held.
pub fn put(conn: &Connection, account_id: AccountId, message_id: &str, raw: &[u8]) -> Result<()> {
    conn.execute(
        "INSERT INTO local_messages (account_id, message_id, raw) VALUES (?1, ?2, ?3) \
         ON CONFLICT (account_id, message_id) DO UPDATE SET raw = excluded.raw",
        params![account_id, message_id, raw],
    )?;
    Ok(())
}

/// The bytes kept for `message_id`, or `None`.
pub fn get(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row(
            "SELECT raw FROM local_messages WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn delete(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM local_messages WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    Ok(())
}
