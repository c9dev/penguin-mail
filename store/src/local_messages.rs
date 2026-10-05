//! The raw bytes of mail that lives only on this computer: a POP3
//! account's messages, and what such an account sends and drafts. A
//! trigger deletes a copy with its message row, so Delete Forever and
//! removing the account take the bytes too.

use mailrs_domain::AccountId;
use rusqlite::{Connection, MAIN_DB, OptionalExtension, params};

use crate::Result;

/// Keeps `raw` as message `message_id`, replacing any copy held.
///
/// Bound as a parameter, the bytes would be copied by SQLite and copied
/// again into the row it builds, three copies of a large message at once.
/// The row is made with a zeroblob of the right length, which takes no
/// memory, and the bytes go into it through incremental blob I/O, a page
/// at a time.
pub fn put(conn: &Connection, account_id: AccountId, message_id: &str, raw: &[u8]) -> Result<()> {
    let length = i64::try_from(raw.len()).unwrap_or(i64::MAX);
    let row: i64 = conn.query_row(
        "INSERT INTO local_messages (account_id, message_id, raw) VALUES (?1, ?2, zeroblob(?3)) \
         ON CONFLICT (account_id, message_id) DO UPDATE SET raw = excluded.raw RETURNING rowid",
        params![account_id, message_id, length],
        |row| row.get(0),
    )?;
    if !raw.is_empty() {
        let mut blob = conn.blob_open(MAIN_DB, c"local_messages", c"raw", row, false)?;
        blob.write_at(raw, 0)?;
    }
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
