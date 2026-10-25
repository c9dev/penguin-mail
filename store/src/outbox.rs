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

use crate::{Result, StoreError};

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
    /// The server's hold, for a message the server sends itself at its
    /// hour. The app sends nothing for it while this is set.
    pub held: Option<String>,
}

const COLUMNS: &str = "id, account_id, draft_id, message_id, thread_id, subject, recipients, \
                       send_at, raw, composer, attempts, problem, held";

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
        held: row.get(12)?,
    })
}

/// Stores a message: as a new row, or over the row it already has, which
/// is the one its id names, the one holding the same Gmail draft, or the
/// one holding the same server hold. Gives back the row's id. Writing over
/// a row lets go of any claim on it, since the caller that claimed it is
/// done with it.
///
/// A message that finds its row by draft alone may not change that row's
/// hold: the server would still send what it holds, and the row would
/// lose the handle that calls it back. That answers [`StoreError::Held`].
/// The caller that names the row by its id has called the hold back.
pub fn put(conn: &Connection, message: &Queued) -> Result<i64> {
    let by_draft = match &message.draft_id {
        Some(draft_id) => find_draft(conn, message.account_id, draft_id)?,
        None => None,
    };
    if message.id <= 0
        && let Some(row) = &by_draft
        && row.held.is_some()
        && row.held != message.held
    {
        return Err(StoreError::Held);
    }
    let by_draft = by_draft.map(|m| m.id);
    let by_hold = match &message.held {
        Some(handle) if by_draft.is_none() => {
            find_held(conn, message.account_id, handle)?.map(|m| m.id)
        }
        _ => None,
    };
    let already = Some(message.id)
        .filter(|id| *id > 0)
        .or(by_draft)
        .or(by_hold);
    if let Some(id) = already {
        let changed = conn.execute(
            "UPDATE outbox SET draft_id = ?2, message_id = ?3, thread_id = ?4, subject = ?5, \
             recipients = ?6, send_at = ?7, raw = ?8, composer = ?9, attempts = ?10, \
             problem = ?11, held = ?12 WHERE id = ?1",
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
                message.problem,
                message.held
            ],
        )?;
        if changed > 0 {
            release(conn, id)?;
            return Ok(id);
        }
    }
    conn.execute(
        "INSERT INTO outbox (account_id, draft_id, message_id, thread_id, subject, recipients, \
         send_at, raw, composer, attempts, problem, held) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
            message.problem,
            message.held
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

/// How many messages wait for their hour and how many are stuck, counted
/// without reading their bytes, for the sidebar.
pub fn counts(conn: &Connection) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FILTER (WHERE problem IS NULL), \
         COUNT(*) FILTER (WHERE problem IS NOT NULL) FROM outbox",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
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

/// The waiting message the server holds under `handle`.
pub fn find_held(conn: &Connection, account_id: AccountId, handle: &str) -> Result<Option<Queued>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM outbox WHERE account_id = ?1 AND held = ?2"),
            params![account_id, handle],
            to_queued,
        )
        .optional()?)
}

/// The held copies no waiting row stands for, by account and message id:
/// messages carrying `keyword`, the app's held keyword, that no row names
/// by its draft or its message, and that no row whose hold lost its
/// answer waits to find by their `Message-ID`. Another install of the app
/// held them.
pub fn unclaimed_holds(conn: &Connection, keyword: &str) -> Result<Vec<(AccountId, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.account_id, m.id FROM message_keywords k \
         JOIN messages m ON m.account_id = k.account_id AND m.id = k.message_id \
         WHERE k.keyword = ?1 AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.account_id = m.account_id \
         AND (o.draft_id = m.id OR o.message_id = m.id \
         OR (m.rfc822_msgid IS NOT NULL AND o.held LIKE 'unconfirmed:%' \
         AND substr(o.held, -length(m.rfc822_msgid) - 1) = ':' || m.rfc822_msgid)))",
    )?;
    let rows = stmt.query_map(params![keyword], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

/// Records that the draft `old` of a waiting message was saved again, as
/// the draft `draft_id` holding `message_id`. The draft holds the bytes
/// from then on, so the row drops any it kept. A held row is left alone:
/// its draft is the server's copy, which nothing saves over.
pub fn set_message(
    conn: &Connection,
    account_id: AccountId,
    old: &str,
    draft_id: &str,
    message_id: &str,
    thread_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE outbox SET draft_id = ?3, message_id = ?4, thread_id = ?5, raw = NULL \
         WHERE account_id = ?1 AND draft_id = ?2 AND held IS NULL",
        params![account_id, old, draft_id, message_id, thread_id],
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

/// Counts a try at the message `id` as it starts, so a try the app never
/// heard back from, because it stopped, still makes the next one look in
/// Sent first.
pub fn count_try(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE outbox SET attempts = attempts + 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Takes the message `id` for sending at `now` and gives it back, or
/// `None` when it has gone or another caller claimed it after
/// `lapsed_before`. One statement checks and sets the claim, so of two
/// callers only one gets the row.
pub fn claim(
    conn: &Connection,
    id: i64,
    now: EpochMillis,
    lapsed_before: EpochMillis,
) -> Result<Option<Queued>> {
    let taken = conn.execute(
        "INSERT INTO outbox_claims (id, claimed_at) \
         SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM outbox WHERE id = ?1) \
         ON CONFLICT (id) DO UPDATE SET claimed_at = excluded.claimed_at \
         WHERE outbox_claims.claimed_at < ?3",
        params![id, now, lapsed_before],
    )?;
    match taken {
        0 => Ok(None),
        _ => find(conn, id),
    }
}

/// Lets go of a claim without changing the message, for a caller that
/// took it and then did not try.
pub fn release(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM outbox_claims WHERE id = ?1", params![id])?;
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
