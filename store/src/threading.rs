//! Threads for mail from a server that keeps none, found the way
//! Thunderbird finds them: a copy of a stored message, or a reply that
//! arrived first, shares its thread; then the nearest message the
//! References and In-Reply-To headers name; then, for a subject with a
//! reply prefix, the nearest message with the same base subject within 30
//! days. A message that finds none starts a thread named by its own id.
//! Gmail supplies its own threads, and nothing sends its mail here.

use mailrs_domain::{AccountId, EpochMillis, MessageMeta, subject};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// The headers that link a message to the ones it answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    pub in_reply_to: Option<String>,
    /// Oldest first, as the header lists them.
    pub references: Vec<String>,
}

const WINDOW: EpochMillis = 30 * 24 * 60 * 60 * 1000;

/// A Message-ID without its angle brackets or spaces.
fn bare(id: &str) -> &str {
    id.trim().trim_start_matches('<').trim_end_matches('>')
}

/// The thread `meta` joins, or its own id.
pub(crate) fn thread_for(
    conn: &Connection,
    account_id: AccountId,
    meta: &MessageMeta,
    links: &Links,
) -> Result<String> {
    if let Some(own) = meta.rfc822_msgid.as_deref().map(bare) {
        if let Some(thread) = by_msgid(conn, account_id, own)? {
            return Ok(thread);
        }
        if let Some(thread) = naming(conn, account_id, own)? {
            return Ok(thread);
        }
    }
    let named = links.references.iter().rev().chain(links.in_reply_to.iter());
    for id in named {
        if let Some(thread) = by_msgid(conn, account_id, bare(id))? {
            return Ok(thread);
        }
    }
    let base = subject::base(&meta.subject);
    if subject::replies(&meta.subject)
        && !base.is_empty()
        && let Some(thread) = by_subject(conn, account_id, &base, meta.date)?
    {
        return Ok(thread);
    }
    Ok(meta.id.clone())
}

/// Records what `meta` links to and its base subject, so mail that comes
/// later can find it. Only messages threaded here carry a base subject.
pub(crate) fn remember(
    conn: &Connection,
    account_id: AccountId,
    meta: &MessageMeta,
    links: &Links,
) -> Result<()> {
    conn.prepare_cached("UPDATE messages SET base_subject = ?3 WHERE account_id = ?1 AND id = ?2")?
        .execute(params![account_id, meta.id, subject::base(&meta.subject)])?;
    conn.prepare_cached("DELETE FROM message_links WHERE account_id = ?1 AND message_id = ?2")?
        .execute(params![account_id, meta.id])?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_links (account_id, message_id, msgid) VALUES (?1, ?2, ?3)",
    )?;
    for id in links.references.iter().chain(links.in_reply_to.iter()) {
        insert.execute(params![account_id, meta.id, bare(id)])?;
    }
    Ok(())
}

/// The thread of a message threaded here whose Message-ID is `msgid`,
/// stored with or without its angle brackets.
fn by_msgid(conn: &Connection, account_id: AccountId, msgid: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "SELECT thread_id FROM messages WHERE account_id = ?1 AND base_subject IS NOT NULL \
             AND rfc822_msgid IN (?2, ?3) LIMIT 1",
        )?
        .query_row(params![account_id, msgid, format!("<{msgid}>")], |row| row.get(0))
        .optional()?)
}

/// The thread of a stored message that names `msgid` among its links: a
/// reply that arrived before the message it answers.
fn naming(conn: &Connection, account_id: AccountId, msgid: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "SELECT m.thread_id FROM message_links l CROSS JOIN messages m \
             ON m.account_id = l.account_id AND m.id = l.message_id \
             WHERE l.account_id = ?1 AND l.msgid = ?2 LIMIT 1",
        )?
        .query_row(params![account_id, msgid], |row| row.get(0))
        .optional()?)
}

/// The thread of the message threaded here nearest in time to `date`
/// whose base subject is `base`, within 30 days either side.
fn by_subject(
    conn: &Connection,
    account_id: AccountId,
    base: &str,
    date: EpochMillis,
) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "SELECT thread_id FROM messages WHERE account_id = ?1 AND base_subject = ?2 \
             AND date BETWEEN ?3 AND ?4 ORDER BY abs(date - ?5) LIMIT 1",
        )?
        .query_row(
            params![account_id, base, date - WINDOW, date + WINDOW, date],
            |row| row.get(0),
        )
        .optional()?)
}
