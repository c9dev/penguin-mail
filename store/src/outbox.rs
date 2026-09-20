//! Messages waiting to go out. Send Later puts one here with the hour it
//! chose, and Gmail holds the bytes as a draft. A message that could not be
//! sent now goes here too, with the bytes that were built for it, and
//! `send_at` is then when to try again. The two share a table because both
//! are a message waiting, and one pass sends whatever is due.
//!
//! A recorded `problem` tells them apart on screen: without one the message
//! is waiting for its hour and the Send Later mailbox lists it; with one it
//! is stuck and the Outbox lists it.

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Queued {
    /// This row, as the store numbered it. Zero until it is stored.
    pub id: i64,
    pub account_id: AccountId,
    /// The Gmail draft this message occupies, once it has reached Gmail.
    /// Sending deletes it.
    pub draft_id: Option<String>,
    /// The draft's current message, for opening it.
    pub message_id: Option<String>,
    pub thread_id: Option<String>,
    pub subject: String,
    /// Who it goes to, as shown in the list.
    pub recipients: String,
    /// When to try: the hour Send Later chose, or the next attempt.
    pub send_at: EpochMillis,
    /// The message as it will go out, kept here for one that never reached
    /// Gmail. `None` leaves the bytes to the Gmail draft.
    pub raw: Option<Vec<u8>>,
    /// What the composer reopens, written and read by the app alone. The
    /// store keeps it whole and never looks inside.
    pub composer: String,
    pub attempts: u32,
    /// Why the last attempt failed. `None` while the message is waiting for
    /// its hour and nothing has gone wrong.
    pub problem: Option<String>,
}

const COLUMNS: &str = "id, account_id, draft_id, message_id, thread_id, subject, recipients, \
                       send_at, raw, composer, attempts, problem";

fn to_queued(row: &Row<'_>) -> rusqlite::Result<Queued> {
    Ok(Queued {
        id: row.get(0)?,
        account_id: row.get(1)?,
        draft_id: row.get(2)?,
        message_id: row.get(3)?,
        thread_id: row.get(4)?,
        subject: row.get(5)?,
        recipients: row.get(6)?,
        send_at: row.get(7)?,
        raw: row.get(8)?,
        composer: row.get(9)?,
        attempts: row.get(10)?,
        problem: row.get(11)?,
    })
}

/// Stores a message: as a new row, or over the row it already has, which
/// is the one its id names or the one holding the same Gmail draft. Gives
/// back the row's id.
pub fn put(conn: &Connection, message: &Queued) -> Result<i64> {
    let already = match (message.id, &message.draft_id) {
        (id, _) if id > 0 => Some(id),
        (_, Some(draft_id)) => find_draft(conn, message.account_id, draft_id)?.map(|m| m.id),
        _ => None,
    };
    if let Some(id) = already {
        let changed = conn.execute(
            "UPDATE outbox SET draft_id = ?2, message_id = ?3, thread_id = ?4, subject = ?5, \
             recipients = ?6, send_at = ?7, raw = ?8, composer = ?9, attempts = ?10, \
             problem = ?11 WHERE id = ?1",
            params![
                id,
                message.draft_id,
                message.message_id,
                message.thread_id,
                message.subject,
                message.recipients,
                message.send_at,
                message.raw,
                message.composer,
                message.attempts,
                message.problem
            ],
        )?;
        if changed > 0 {
            return Ok(id);
        }
    }
    conn.execute(
        "INSERT INTO outbox (account_id, draft_id, message_id, thread_id, subject, recipients, \
         send_at, raw, composer, attempts, problem) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            message.account_id,
            message.draft_id,
            message.message_id,
            message.thread_id,
            message.subject,
            message.recipients,
            message.send_at,
            message.raw,
            message.composer,
            message.attempts,
            message.problem
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Everything waiting, soonest first.
pub fn list(conn: &Connection) -> Result<Vec<Queued>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM outbox ORDER BY send_at, account_id, id"
    ))?;
    let rows = stmt.query_map([], to_queued)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Messages waiting for the hour Send Later chose.
pub fn scheduled(conn: &Connection) -> Result<Vec<Queued>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|m| m.problem.is_none())
        .collect())
}

/// Messages that hit a problem and are waiting to be tried again.
pub fn stuck(conn: &Connection) -> Result<Vec<Queued>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|m| m.problem.is_some())
        .collect())
}

/// What should have gone out by `now`.
pub fn due(conn: &Connection, now: EpochMillis) -> Result<Vec<Queued>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|m| m.send_at <= now)
        .collect())
}

pub fn find(conn: &Connection, id: i64) -> Result<Option<Queued>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM outbox WHERE id = ?1"),
            params![id],
            to_queued,
        )
        .optional()?)
}

pub fn find_draft(
    conn: &Connection,
    account_id: AccountId,
    draft_id: &str,
) -> Result<Option<Queued>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM outbox WHERE account_id = ?1 AND draft_id = ?2"),
            params![account_id, draft_id],
            to_queued,
        )
        .optional()?)
}

/// The waiting message whose Gmail draft shows as `message_id`.
pub fn by_message(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<Queued>> {
    Ok(list(conn)?
        .into_iter()
        .find(|m| m.account_id == account_id && m.message_id.as_deref() == Some(message_id)))
}

/// Records that a waiting message was saved again under a new draft message.
pub fn set_message(
    conn: &Connection,
    account_id: AccountId,
    draft_id: &str,
    message_id: &str,
    thread_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE outbox SET message_id = ?3, thread_id = ?4 WHERE account_id = ?1 AND draft_id = ?2",
        params![account_id, draft_id, message_id, thread_id],
    )?;
    Ok(())
}

/// Records a failed attempt: why it failed, and when to try again.
pub fn failed(conn: &Connection, id: i64, problem: &str, next_try: EpochMillis) -> Result<()> {
    conn.execute(
        "UPDATE outbox SET problem = ?2, send_at = ?3, attempts = attempts + 1 WHERE id = ?1",
        params![id, problem, next_try],
    )?;
    Ok(())
}

/// Brings every stuck message forward to `now`, for when the network comes
/// back and sitting out the rest of the interval would serve nobody.
pub fn try_now(conn: &Connection, now: EpochMillis) -> Result<()> {
    conn.execute(
        "UPDATE outbox SET send_at = ?1 WHERE problem IS NOT NULL AND send_at > ?1",
        params![now],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM outbox WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn remove_draft(conn: &Connection, account_id: AccountId, draft_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM outbox WHERE account_id = ?1 AND draft_id = ?2",
        params![account_id, draft_id],
    )?;
    Ok(())
}
