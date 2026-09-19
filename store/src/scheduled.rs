//! Messages waiting to be sent later. Each is a Gmail draft; this table
//! only says when to send it.

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, Row, params};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub account_id: AccountId,
    pub draft_id: String,
    /// The draft's current message, for opening it.
    pub message_id: String,
    pub thread_id: String,
    pub subject: String,
    /// Who it goes to, as shown in the list.
    pub recipients: String,
    pub send_at: EpochMillis,
}

const COLUMNS: &str = "account_id, draft_id, message_id, thread_id, subject, recipients, send_at";

fn to_scheduled(row: &Row<'_>) -> rusqlite::Result<Scheduled> {
    Ok(Scheduled {
        account_id: row.get(0)?,
        draft_id: row.get(1)?,
        message_id: row.get(2)?,
        thread_id: row.get(3)?,
        subject: row.get(4)?,
        recipients: row.get(5)?,
        send_at: row.get(6)?,
    })
}

/// Schedules a draft, or moves it to a new time.
pub fn schedule(conn: &Connection, item: &Scheduled) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO scheduled ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (account_id, draft_id) DO UPDATE SET message_id = excluded.message_id, \
             thread_id = excluded.thread_id, subject = excluded.subject, \
             recipients = excluded.recipients, send_at = excluded.send_at"
        ),
        params![
            item.account_id,
            item.draft_id,
            item.message_id,
            item.thread_id,
            item.subject,
            item.recipients,
            item.send_at
        ],
    )?;
    Ok(())
}

/// Everything waiting, soonest first.
pub fn list(conn: &Connection) -> Result<Vec<Scheduled>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM scheduled ORDER BY send_at, account_id, draft_id"
    ))?;
    let rows = stmt.query_map([], to_scheduled)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// What should have gone out by `now`.
pub fn due(conn: &Connection, now: EpochMillis) -> Result<Vec<Scheduled>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|s| s.send_at <= now)
        .collect())
}

pub fn find(conn: &Connection, account_id: AccountId, draft_id: &str) -> Result<Option<Scheduled>> {
    Ok(list(conn)?
        .into_iter()
        .find(|s| s.account_id == account_id && s.draft_id == draft_id))
}

/// The scheduled draft whose current message is `message_id`.
pub fn by_message(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<Scheduled>> {
    Ok(list(conn)?
        .into_iter()
        .find(|s| s.account_id == account_id && s.message_id == message_id))
}

/// Records that a scheduled draft was saved again under a new message.
pub fn set_message(
    conn: &Connection,
    account_id: AccountId,
    draft_id: &str,
    message_id: &str,
    thread_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE scheduled SET message_id = ?3, thread_id = ?4 WHERE account_id = ?1 AND draft_id = ?2",
        params![account_id, draft_id, message_id, thread_id],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, account_id: AccountId, draft_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM scheduled WHERE account_id = ?1 AND draft_id = ?2",
        params![account_id, draft_id],
    )?;
    Ok(())
}
