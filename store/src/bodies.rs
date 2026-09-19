//! Message bodies, cached separately from metadata and evicted least recently read first.

use mailrs_domain::{AccountId, Attachment, EpochMillis, MessageBody};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Stores a body and its attachment list. The message must already be stored.
pub fn put_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    body: &MessageBody,
    now: EpochMillis,
) -> Result<()> {
    let size = body.html.as_ref().map_or(0, String::len) + body.text.as_ref().map_or(0, String::len);
    conn.execute(
        "INSERT INTO bodies (account_id, message_id, html, text, size, fetched_at, accessed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
         ON CONFLICT (account_id, message_id) DO UPDATE SET html = excluded.html, text = excluded.text, \
         size = excluded.size, fetched_at = excluded.fetched_at, accessed_at = excluded.accessed_at",
        params![account_id, message_id, body.html, body.text, size as i64, now],
    )?;
    conn.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO attachments (account_id, message_id, part_id, filename, mime_type, size, attachment_id, \
         content_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for a in &body.attachments {
        insert.execute(params![
            account_id,
            message_id,
            a.part_id,
            a.filename,
            a.mime_type,
            a.size,
            a.attachment_id,
            a.content_id
        ])?;
    }
    Ok(())
}

/// Returns a cached body and marks it as read at `now`.
pub fn get_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    now: EpochMillis,
) -> Result<Option<MessageBody>> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT html, text FROM bodies WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((html, text)) = row else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE bodies SET accessed_at = ?3 WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id, now],
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT part_id, filename, mime_type, size, attachment_id, content_id FROM attachments \
         WHERE account_id = ?1 AND message_id = ?2 ORDER BY part_id",
    )?;
    let attachments = stmt
        .query_map(params![account_id, message_id], |row| {
            Ok(Attachment {
                part_id: row.get(0)?,
                filename: row.get(1)?,
                mime_type: row.get(2)?,
                size: row.get(3)?,
                attachment_id: row.get(4)?,
                content_id: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(MessageBody { html, text, attachments }))
}

/// Deletes the least recently read bodies until the rest fit in `max_bytes`.
/// Returns how many it deleted.
pub fn evict_bodies(conn: &Connection, max_bytes: i64) -> Result<usize> {
    let rows: Vec<(AccountId, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT account_id, message_id, size FROM bodies ORDER BY accessed_at DESC, account_id, message_id",
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut total = 0;
    let mut evicted = 0;
    for (account_id, message_id, size) in rows {
        total += size;
        if total > max_bytes {
            conn.execute(
                "DELETE FROM bodies WHERE account_id = ?1 AND message_id = ?2",
                params![account_id, message_id],
            )?;
            conn.execute(
                "DELETE FROM attachments WHERE account_id = ?1 AND message_id = ?2",
                params![account_id, message_id],
            )?;
            evicted += 1;
        }
    }
    Ok(evicted)
}
