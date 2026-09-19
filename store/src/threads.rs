//! Thread queries. `messages::refresh_thread` maintains the rows.

use mailrs_domain::{AccountId, ThreadSummary};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

const COLUMNS: &str = "t.account_id, t.id, t.last_message_at, t.subject, t.snippet, t.from_display, \
                       t.message_count, t.unread, t.starred, t.has_attachments";

fn to_summary(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    Ok(ThreadSummary {
        account_id: row.get(0)?,
        id: row.get(1)?,
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

pub fn get_thread(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<Option<ThreadSummary>> {
    let sql = format!("SELECT {COLUMNS} FROM threads t WHERE t.account_id = ?1 AND t.id = ?2");
    Ok(conn.query_row(&sql, params![account_id, thread_id], to_summary).optional()?)
}
