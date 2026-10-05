//! The raw bytes of mail that lives only on this computer: a POP3
//! account's messages, and what such an account sends and drafts. A
//! trigger deletes a copy with its message row, so Delete Forever and
//! removing the account take the bytes too.

use mailrs_domain::AccountId;
use rusqlite::blob::Blob;
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

/// The bytes kept for `message_id`, or `None`. Read through incremental
/// blob I/O into one buffer of their length: a column read would have
/// SQLite assemble the whole value in its own memory first, and the
/// caller copy it from there.
pub fn get(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<Option<Vec<u8>>> {
    let Some(blob) = open(conn, account_id, message_id)? else {
        return Ok(None);
    };
    let mut raw = vec![0; blob.len()];
    blob.read_at_exact(&mut raw, 0)?;
    Ok(Some(raw))
}

/// The bytes kept for `message_id`, open to read a piece at a time with
/// `Read` and `Seek`, or `None`.
pub fn open<'c>(
    conn: &'c Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<Blob<'c>>> {
    let row: Option<i64> = conn
        .query_row(
            "SELECT rowid FROM local_messages WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(row
        .map(|row| conn.blob_open(MAIN_DB, c"local_messages", c"raw", row, true))
        .transpose()?)
}

pub fn delete(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM local_messages WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    Ok(())
}
