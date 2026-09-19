//! Thread queries. `messages::refresh_thread` maintains the rows.

use mailrs_domain::{AccountId, ThreadSummary};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

/// Which threads a list shows: one label, across all accounts or one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadFilter {
    pub account_id: Option<AccountId>,
    pub label_id: String,
}

impl ThreadFilter {
    /// Every account. Use system labels such as `INBOX`; user label ids differ per account.
    pub fn unified(label_id: impl Into<String>) -> Self {
        ThreadFilter {
            account_id: None,
            label_id: label_id.into(),
        }
    }

    pub fn account(account_id: AccountId, label_id: impl Into<String>) -> Self {
        ThreadFilter {
            account_id: Some(account_id),
            label_id: label_id.into(),
        }
    }
}

const COLUMNS: &str = "t.account_id, t.id, t.last_message_at, t.subject, t.snippet, t.from_display, \
                       t.message_count, t.unread, t.starred, t.has_attachments";

const FILTERED: &str = "FROM threads t JOIN thread_labels tl \
                        ON tl.account_id = t.account_id AND tl.thread_id = t.id \
                        WHERE tl.label_id = ?1 AND (?2 IS NULL OR t.account_id = ?2)";

fn to_summary(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    Ok(ThreadSummary {
        account_id: row.get(0)?,
        id: row.get(1)?,
        message_id: None,
        last_message_at: row.get(2)?,
        subject: row.get(3)?,
        snippet: row.get(4)?,
        from: row.get(5)?,
        message_count: row.get(6)?,
        unread: row.get(7)?,
        starred: row.get(8)?,
        has_attachments: row.get(9)?,
    })
}

pub fn get_thread(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<Option<ThreadSummary>> {
    let sql = format!("SELECT {COLUMNS} FROM threads t WHERE t.account_id = ?1 AND t.id = ?2");
    Ok(conn
        .query_row(&sql, params![account_id, thread_id], to_summary)
        .optional()?)
}

/// Newest first. Ties break on account and thread id so pages never overlap.
pub fn list_threads(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let sql = format!(
        "SELECT {COLUMNS} {FILTERED} ORDER BY t.last_message_at DESC, t.account_id, t.id LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![filter.label_id, filter.account_id, limit, offset],
        to_summary,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn count_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) {FILTERED}");
    Ok(
        conn.query_row(&sql, params![filter.label_id, filter.account_id], |row| {
            row.get(0)
        })?,
    )
}

/// Columns for one message shown as a list row.
const MESSAGE_COLUMNS: &str = "m.account_id, m.thread_id, m.id, m.date, m.subject, m.snippet, \
     COALESCE(m.from_name, m.from_addr, ''), m.has_attachments, \
     EXISTS (SELECT 1 FROM message_labels u WHERE u.account_id = m.account_id AND u.message_id = m.id \
             AND u.label_id = 'UNREAD'), \
     EXISTS (SELECT 1 FROM message_labels s WHERE s.account_id = m.account_id AND s.message_id = m.id \
             AND s.label_id = 'STARRED')";

const MESSAGES_FILTERED: &str = "FROM messages m JOIN message_labels ml \
     ON ml.account_id = m.account_id AND ml.message_id = m.id \
     WHERE ml.label_id = ?1 AND (?2 IS NULL OR m.account_id = ?2)";

fn to_message_row(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    Ok(ThreadSummary {
        account_id: row.get(0)?,
        id: row.get(1)?,
        message_id: Some(row.get(2)?),
        last_message_at: row.get(3)?,
        subject: row.get(4)?,
        snippet: row.get(5)?,
        from: row.get(6)?,
        message_count: 1,
        has_attachments: row.get(7)?,
        unread: row.get(8)?,
        starred: row.get(9)?,
    })
}

/// Messages rather than threads, newest first, for when conversation
/// grouping is off.
pub fn list_messages(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} {MESSAGES_FILTERED} ORDER BY m.date DESC, m.account_id, m.id LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![filter.label_id, filter.account_id, limit, offset],
        to_message_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn unread_messages(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) {MESSAGES_FILTERED} AND EXISTS (SELECT 1 FROM message_labels u \
         WHERE u.account_id = m.account_id AND u.message_id = m.id AND u.label_id = 'UNREAD')"
    );
    Ok(
        conn.query_row(&sql, params![filter.label_id, filter.account_id], |row| {
            row.get(0)
        })?,
    )
}

pub fn unread_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) {FILTERED} AND t.unread = 1");
    Ok(
        conn.query_row(&sql, params![filter.label_id, filter.account_id], |row| {
            row.get(0)
        })?,
    )
}
