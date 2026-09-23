//! Message bodies, cached separately from metadata and evicted least recently read first.

use mailrs_domain::{AccountId, Attachment, EpochMillis, MessageBody, Protection};
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
    let size =
        body.html.as_ref().map_or(0, String::len) + body.text.as_ref().map_or(0, String::len);
    conn.execute(
        "INSERT INTO bodies (account_id, message_id, html, text, size, fetched_at, accessed_at, \
         list_unsubscribe, one_click_unsubscribe, calendar, protection, provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (account_id, message_id) DO UPDATE SET html = excluded.html, text = excluded.text, \
         size = excluded.size, fetched_at = excluded.fetched_at, accessed_at = excluded.accessed_at, \
         list_unsubscribe = excluded.list_unsubscribe, \
         one_click_unsubscribe = excluded.one_click_unsubscribe, calendar = excluded.calendar, \
         protection = excluded.protection, provenance = excluded.provenance",
        params![
            account_id,
            message_id,
            body.html,
            body.text,
            size as i64,
            now,
            body.list_unsubscribe,
            body.one_click_unsubscribe,
            body.calendar,
            body.protection.map(Protection::as_str),
            // Three short answers read and shown together, so they travel
            // as one JSON column rather than three of their own.
            (!body.provenance.is_empty())
                .then(|| serde_json::to_string(&body.provenance).ok())
                .flatten()
        ],
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
    let body = peek_body(conn, account_id, message_id)?;
    if body.is_some() {
        conn.execute(
            "UPDATE bodies SET accessed_at = ?3 WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id, now],
        )?;
    }
    Ok(body)
}

/// Returns a cached body without recording a read, for readers that
/// cannot write.
pub fn peek_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<MessageBody>> {
    type BodyRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<BodyRow> = conn
        .query_row(
            "SELECT html, text, list_unsubscribe, one_click_unsubscribe, calendar, protection, \
             provenance FROM bodies WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        html,
        text,
        list_unsubscribe,
        one_click_unsubscribe,
        calendar,
        protection,
        provenance,
    )) = row
    else {
        return Ok(None);
    };
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
    Ok(Some(MessageBody {
        html,
        text,
        attachments,
        list_unsubscribe,
        one_click_unsubscribe,
        calendar,
        protection: protection.and_then(|stored| stored.parse().ok()),
        provenance: provenance
            .and_then(|stored| serde_json::from_str(&stored).ok())
            .unwrap_or_default(),
    }))
}

/// Records reads of cached bodies: `(message id, when)` pairs. Bodies that
/// are gone or were read later are left alone.
pub fn touch_bodies(
    conn: &Connection,
    account_id: AccountId,
    reads: &[(String, EpochMillis)],
) -> Result<()> {
    let mut update = conn.prepare_cached(
        "UPDATE bodies SET accessed_at = ?3 \
         WHERE account_id = ?1 AND message_id = ?2 AND accessed_at < ?3",
    )?;
    for (message_id, at) in reads {
        update.execute(params![account_id, message_id, at])?;
    }
    Ok(())
}

/// Bodies that do not fit in `?1` bytes once the most recently read ones
/// are kept. `bodies_by_access` covers every column this reads.
const OVER_CAP: &str = "SELECT account_id, message_id FROM (SELECT account_id, message_id, \
     SUM(size) OVER (ORDER BY accessed_at DESC, account_id, message_id ROWS UNBOUNDED PRECEDING) \
     AS kept FROM bodies) WHERE kept > ?1";

/// Deletes the least recently read bodies until the rest fit in `max_bytes`.
/// Returns how many it deleted. Does nothing past a sum over the index while
/// the cache fits.
pub fn evict_bodies(conn: &Connection, max_bytes: i64) -> Result<usize> {
    let total: i64 = conn
        .prepare_cached("SELECT TOTAL(size) FROM bodies")?
        .query_row([], |row| row.get::<_, f64>(0))? as i64;
    if total <= max_bytes {
        return Ok(0);
    }
    conn.execute(
        &format!("DELETE FROM attachments WHERE (account_id, message_id) IN ({OVER_CAP})"),
        [max_bytes],
    )?;
    Ok(conn.execute(
        &format!("DELETE FROM bodies WHERE (account_id, message_id) IN ({OVER_CAP})"),
        [max_bytes],
    )?)
}
