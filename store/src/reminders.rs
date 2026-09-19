//! Remind Me: archived conversations waiting to come back to the inbox.

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, Row, params};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub account_id: AccountId,
    pub thread_id: String,
    pub subject: String,
    pub remind_at: EpochMillis,
}

fn to_reminder(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    Ok(Reminder {
        account_id: row.get(0)?,
        thread_id: row.get(1)?,
        subject: row.get(2)?,
        remind_at: row.get(3)?,
    })
}

/// Sets a reminder, or moves an existing one to a new time.
pub fn set(conn: &Connection, reminder: &Reminder) -> Result<()> {
    conn.execute(
        "INSERT INTO reminders (account_id, thread_id, subject, remind_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (account_id, thread_id) DO UPDATE SET subject = excluded.subject, \
         remind_at = excluded.remind_at",
        params![
            reminder.account_id,
            reminder.thread_id,
            reminder.subject,
            reminder.remind_at
        ],
    )?;
    Ok(())
}

/// Everything waiting, soonest first.
pub fn list(conn: &Connection) -> Result<Vec<Reminder>> {
    let mut stmt = conn.prepare(
        "SELECT account_id, thread_id, subject, remind_at FROM reminders \
         ORDER BY remind_at, account_id, thread_id",
    )?;
    let rows = stmt.query_map([], to_reminder)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Reminders whose time has come by `now`.
pub fn due(conn: &Connection, now: EpochMillis) -> Result<Vec<Reminder>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|r| r.remind_at <= now)
        .collect())
}

pub fn remove(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM reminders WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )?;
    Ok(())
}
