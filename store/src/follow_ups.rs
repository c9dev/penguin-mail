//! Follow Up: conversations where the user wrote last and has waited days
//! for an answer.

use mailrs_domain::{AccountId, Address, EpochMillis};
use rusqlite::{Connection, Row, params};

use crate::Result;

const DAY: EpochMillis = 24 * 60 * 60 * 1000;
/// Mail sent more recently than this has not waited long enough.
pub const MIN_WAIT: EpochMillis = 3 * DAY;
/// Mail sent longer ago than this is too old to chase.
pub const MAX_WAIT: EpochMillis = 30 * DAY;

/// A sent message nobody has answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUp {
    pub account_id: AccountId,
    pub thread_id: String,
    pub message_id: String,
    pub subject: String,
    pub to: Vec<Address>,
    pub sent_at: EpochMillis,
}

fn to_follow_up(row: &Row<'_>) -> rusqlite::Result<FollowUp> {
    let to: String = row.get(4)?;
    Ok(FollowUp {
        account_id: row.get(0)?,
        thread_id: row.get(1)?,
        message_id: row.get(2)?,
        subject: row.get(3)?,
        to: serde_json::from_str(&to).unwrap_or_default(),
        sent_at: row.get(5)?,
    })
}

/// Conversations waiting on a reply at `now`, newest first: the newest
/// message other than drafts carries `SENT` and went out between
/// `MIN_WAIT` and `MAX_WAIT` ago. Threads in Trash or Spam stay out, and so
/// do threads dismissed after that message went out.
pub fn waiting(conn: &Connection, now: EpochMillis) -> Result<Vec<FollowUp>> {
    let mut stmt = conn.prepare_cached(&format!(
        "{WAITING} ORDER BY m.date DESC, m.account_id, m.thread_id"
    ))?;
    let rows = stmt.query_map(params![now - MAX_WAIT, now - MIN_WAIT], to_follow_up)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// How many conversations [`waiting`] would list, counted without reading
/// them, for the sidebar.
pub fn waiting_count(conn: &Connection, now: EpochMillis) -> Result<i64> {
    Ok(conn
        .prepare_cached(&format!("SELECT COUNT(*) FROM ({WAITING})"))?
        .query_row(params![now - MAX_WAIT, now - MIN_WAIT], |row| row.get(0))?)
}

/// The sent messages waiting on a reply between the two instants bound as
/// `?1` and `?2`, in no order.
const WAITING: &str = "SELECT m.account_id, m.thread_id, m.id, m.subject, m.to_addrs, m.date FROM messages m \
         WHERE m.date BETWEEN ?1 AND ?2 \
         AND EXISTS (SELECT 1 FROM message_mailboxes s CROSS JOIN mailboxes b ON b.key = s.mailbox \
             WHERE s.account_id = m.account_id AND s.message_id = m.id AND b.role = 'sent') \
         AND NOT EXISTS (SELECT 1 FROM messages n WHERE n.account_id = m.account_id \
             AND n.thread_id = m.thread_id AND (n.date > m.date OR (n.date = m.date AND n.id > m.id)) \
             AND NOT EXISTS (SELECT 1 FROM message_mailboxes d CROSS JOIN mailboxes b ON b.key = d.mailbox \
                 WHERE d.account_id = n.account_id AND d.message_id = n.id AND b.role = 'drafts')) \
         AND NOT EXISTS (SELECT 1 FROM thread_mailboxes t CROSS JOIN mailboxes b ON b.key = t.mailbox \
             WHERE t.account_id = m.account_id AND t.thread_id = m.thread_id \
             AND b.role IN ('trash', 'junk')) \
         AND NOT EXISTS (SELECT 1 FROM follow_up_dismissals f WHERE f.account_id = m.account_id \
             AND f.thread_id = m.thread_id AND f.dismissed_at >= m.date)";

/// Stops suggesting a thread until the user sends something newer in it.
pub fn dismiss(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
    now: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT INTO follow_up_dismissals (account_id, thread_id, dismissed_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (account_id, thread_id) DO UPDATE SET dismissed_at = excluded.dismissed_at",
        params![account_id, thread_id, now],
    )?;
    Ok(())
}

/// Takes back a dismissal.
pub fn restore(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM follow_up_dismissals WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )?;
    Ok(())
}
