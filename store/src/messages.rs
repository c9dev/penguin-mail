//! Message rows, their labels, and the derived thread rows.
//!
//! Callers change messages, then call `refresh_thread` for each touched
//! thread inside the same transaction.

use std::collections::HashSet;

use mailrs_domain::{AccountId, Address, MessageMeta};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError};

pub fn upsert_message(conn: &Connection, m: &MessageMeta, sync_gen: i64) -> Result<()> {
    let to = serde_json::to_string(&m.to).unwrap_or_else(|_| "[]".into());
    let cc = serde_json::to_string(&m.cc).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO messages (account_id, id, thread_id, rfc822_msgid, from_name, from_addr, to_addrs, \
         cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT (account_id, id) DO UPDATE SET thread_id = excluded.thread_id, \
         rfc822_msgid = excluded.rfc822_msgid, from_name = excluded.from_name, \
         from_addr = excluded.from_addr, to_addrs = excluded.to_addrs, cc_addrs = excluded.cc_addrs, \
         subject = excluded.subject, date = excluded.date, snippet = excluded.snippet, \
         size = excluded.size, has_attachments = excluded.has_attachments, sync_gen = excluded.sync_gen",
        params![
            m.account_id,
            m.id,
            m.thread_id,
            m.rfc822_msgid,
            m.from.as_ref().and_then(|a| a.name.as_deref()),
            m.from.as_ref().map(|a| a.email.as_str()),
            to,
            cc,
            m.subject,
            m.date,
            m.snippet,
            m.size,
            m.has_attachments,
            sync_gen,
        ],
    )?;
    set_labels(conn, m.account_id, &m.id, &m.label_ids)
}

/// Replaces a stored message's labels.
pub fn set_labels(conn: &Connection, account_id: AccountId, message_id: &str, labels: &[String]) -> Result<()> {
    conn.execute(
        "DELETE FROM message_labels WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_labels (account_id, message_id, label_id) VALUES (?1, ?2, ?3)",
    )?;
    for label in labels {
        insert.execute(params![account_id, message_id, label])?;
    }
    Ok(())
}

pub fn thread_id_of(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT thread_id FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Adds labels to a stored message. Returns its thread, or `None` when the
/// message is not stored.
pub fn add_labels(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    labels: &[String],
) -> Result<Option<String>> {
    let Some(thread_id) = thread_id_of(conn, account_id, message_id)? else {
        return Ok(None);
    };
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_labels (account_id, message_id, label_id) VALUES (?1, ?2, ?3)",
    )?;
    for label in labels {
        insert.execute(params![account_id, message_id, label])?;
    }
    Ok(Some(thread_id))
}

/// Removes labels from a stored message. Returns its thread, or `None` when
/// the message is not stored.
pub fn remove_labels(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    labels: &[String],
) -> Result<Option<String>> {
    let Some(thread_id) = thread_id_of(conn, account_id, message_id)? else {
        return Ok(None);
    };
    let mut delete = conn.prepare_cached(
        "DELETE FROM message_labels WHERE account_id = ?1 AND message_id = ?2 AND label_id = ?3",
    )?;
    for label in labels {
        delete.execute(params![account_id, message_id, label])?;
    }
    Ok(Some(thread_id))
}

/// Deletes a message with its labels and body. Returns its thread, or `None`
/// when the message was not stored.
pub fn delete_message(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<Option<String>> {
    let thread_id = thread_id_of(conn, account_id, message_id)?;
    if thread_id.is_some() {
        conn.execute("DELETE FROM messages WHERE account_id = ?1 AND id = ?2", params![account_id, message_id])?;
    }
    Ok(thread_id)
}

pub fn delete_thread(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<()> {
    conn.execute("DELETE FROM messages WHERE account_id = ?1 AND thread_id = ?2", params![account_id, thread_id])?;
    conn.execute("DELETE FROM threads WHERE account_id = ?1 AND id = ?2", params![account_id, thread_id])?;
    Ok(())
}

/// A stored message's labels, sorted.
pub fn labels_of(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT label_id FROM message_labels WHERE account_id = ?1 AND message_id = ?2 ORDER BY label_id",
    )?;
    let rows = stmt.query_map(params![account_id, message_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
}

/// The subset of `ids` that is stored.
pub fn existing_ids(conn: &Connection, account_id: AccountId, ids: &[String]) -> Result<HashSet<String>> {
    let mut found = HashSet::new();
    for id in ids {
        if thread_id_of(conn, account_id, id)?.is_some() {
            found.insert(id.clone());
        }
    }
    Ok(found)
}

struct MessageRow {
    id: String,
    thread_id: String,
    rfc822_msgid: Option<String>,
    from_name: Option<String>,
    from_addr: Option<String>,
    to: String,
    cc: String,
    subject: String,
    date: i64,
    snippet: String,
    size: i64,
    has_attachments: bool,
}

/// A thread's stored messages, oldest first.
pub fn thread_messages(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<Vec<MessageMeta>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, rfc822_msgid, from_name, from_addr, to_addrs, cc_addrs, subject, date, \
         snippet, size, has_attachments FROM messages WHERE account_id = ?1 AND thread_id = ?2 \
         ORDER BY date ASC, id",
    )?;
    let rows = stmt
        .query_map(params![account_id, thread_id], |row| {
            Ok(MessageRow {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                rfc822_msgid: row.get(2)?,
                from_name: row.get(3)?,
                from_addr: row.get(4)?,
                to: row.get(5)?,
                cc: row.get(6)?,
                subject: row.get(7)?,
                date: row.get(8)?,
                snippet: row.get(9)?,
                size: row.get(10)?,
                has_attachments: row.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|r| {
            let label_ids = labels_of(conn, account_id, &r.id)?;
            Ok(MessageMeta {
                account_id,
                to: parse_addresses("messages.to_addrs", &r.to)?,
                cc: parse_addresses("messages.cc_addrs", &r.cc)?,
                from: r.from_addr.map(|email| Address { name: r.from_name, email }),
                id: r.id,
                thread_id: r.thread_id,
                rfc822_msgid: r.rfc822_msgid,
                subject: r.subject,
                date: r.date,
                snippet: r.snippet,
                size: r.size,
                has_attachments: r.has_attachments,
                label_ids,
            })
        })
        .collect()
}

fn parse_addresses(column: &'static str, json: &str) -> Result<Vec<Address>> {
    serde_json::from_str(json).map_err(|_| StoreError::Corrupt { column, value: json.to_string() })
}

/// Recomputes a thread's summary row and label set from its messages, and
/// deletes the thread when no messages remain.
pub fn refresh_thread(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<()> {
    let (count, last, has_attachments): (i64, Option<i64>, Option<bool>) = conn.query_row(
        "SELECT COUNT(*), MAX(date), MAX(has_attachments) FROM messages WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if count == 0 {
        conn.execute("DELETE FROM threads WHERE account_id = ?1 AND id = ?2", params![account_id, thread_id])?;
        return Ok(());
    }
    let subject: String = conn.query_row(
        "SELECT subject FROM messages WHERE account_id = ?1 AND thread_id = ?2 ORDER BY date ASC, id LIMIT 1",
        params![account_id, thread_id],
        |row| row.get(0),
    )?;
    let (snippet, from): (String, String) = conn.query_row(
        "SELECT snippet, COALESCE(from_name, from_addr, '') FROM messages \
         WHERE account_id = ?1 AND thread_id = ?2 ORDER BY date DESC, id DESC LIMIT 1",
        params![account_id, thread_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let has_label = |label: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM message_labels ml JOIN messages m \
             ON m.account_id = ml.account_id AND m.id = ml.message_id \
             WHERE m.account_id = ?1 AND m.thread_id = ?2 AND ml.label_id = ?3)",
            params![account_id, thread_id, label],
            |row| row.get(0),
        )?)
    };
    let unread = has_label("UNREAD")?;
    let starred = has_label("STARRED")?;
    conn.execute(
        "INSERT INTO threads (account_id, id, last_message_at, subject, snippet, from_display, message_count, \
         unread, starred, has_attachments) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
         ON CONFLICT (account_id, id) DO UPDATE SET last_message_at = excluded.last_message_at, \
         subject = excluded.subject, snippet = excluded.snippet, from_display = excluded.from_display, \
         message_count = excluded.message_count, unread = excluded.unread, starred = excluded.starred, \
         has_attachments = excluded.has_attachments",
        params![
            account_id,
            thread_id,
            last.unwrap_or(0),
            subject,
            snippet,
            from,
            count,
            unread,
            starred,
            has_attachments.unwrap_or(false),
        ],
    )?;
    conn.execute(
        "DELETE FROM thread_labels WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )?;
    conn.execute(
        "INSERT INTO thread_labels (account_id, thread_id, label_id) \
         SELECT DISTINCT ?1, ?2, ml.label_id FROM message_labels ml JOIN messages m \
         ON m.account_id = ml.account_id AND m.id = ml.message_id \
         WHERE m.account_id = ?1 AND m.thread_id = ?2",
        params![account_id, thread_id],
    )?;
    Ok(())
}
