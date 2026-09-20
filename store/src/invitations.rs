//! What the app remembers about one event between openings: which version
//! of it arrived last, when it started then, and the answer the user gave.
//!
//! An organizer sends every change to an event under the same UID with a
//! higher sequence, so one row per UID is enough. Reopening the message
//! reads the row back and the card shows the answer again; a new version
//! replaces it and clears the answer, because the organizer is asking
//! again.

use mailrs_domain::invitation::Answer;
use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// The row for one event, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub uid: String,
    pub sequence: i64,
    /// When the event started in the version that arrived last.
    pub starts_at: Option<EpochMillis>,
    pub all_day: bool,
    pub summary: String,
    pub cancelled: bool,
    /// The answer the user sent, if they have answered this version.
    pub answer: Option<Answer>,
    /// The message this version arrived in.
    pub message_id: String,
}

/// What the store holds for `uid`, if anything.
pub fn saved(conn: &Connection, account_id: AccountId, uid: &str) -> Result<Option<Saved>> {
    let row = conn
        .query_row(
            "SELECT uid, sequence, starts_at, all_day, summary, cancelled, answer, message_id \
             FROM invitations WHERE account_id = ?1 AND uid = ?2",
            params![account_id, uid],
            |row| {
                Ok(Saved {
                    uid: row.get(0)?,
                    sequence: row.get(1)?,
                    starts_at: row.get(2)?,
                    all_day: row.get(3)?,
                    summary: row.get(4)?,
                    cancelled: row.get(5)?,
                    answer: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|a| a.parse().ok()),
                    message_id: row.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Records the version of an event that just arrived. A version at least as
/// new as the stored one replaces it; an older one changes nothing, which
/// is what happens when the user opens the first invitation again after an
/// update arrived. A newer version clears the answer, since the organizer
/// is asking about a different meeting.
pub fn remember(
    conn: &Connection,
    account_id: AccountId,
    seen: &Saved,
    now: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT INTO invitations \
         (account_id, uid, sequence, starts_at, all_day, summary, cancelled, answer, message_id, seen_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9) \
         ON CONFLICT (account_id, uid) DO UPDATE SET \
             sequence = excluded.sequence, \
             starts_at = excluded.starts_at, \
             all_day = excluded.all_day, \
             summary = excluded.summary, \
             cancelled = excluded.cancelled, \
             answer = CASE WHEN excluded.sequence > invitations.sequence \
                      THEN NULL ELSE invitations.answer END, \
             message_id = excluded.message_id, \
             seen_at = excluded.seen_at \
         WHERE excluded.sequence >= invitations.sequence",
        params![
            account_id,
            seen.uid,
            seen.sequence,
            seen.starts_at,
            seen.all_day,
            seen.summary,
            seen.cancelled,
            seen.message_id,
            now
        ],
    )?;
    Ok(())
}

/// Records the answer the user sent for `uid`. The row must be there
/// already, which [`remember`] sees to when the message opens.
pub fn answer(conn: &Connection, account_id: AccountId, uid: &str, answer: Answer) -> Result<()> {
    conn.execute(
        "UPDATE invitations SET answer = ?3 WHERE account_id = ?1 AND uid = ?2",
        params![account_id, uid, answer.as_str()],
    )?;
    Ok(())
}
