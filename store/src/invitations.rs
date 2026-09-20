//! What the app remembers about one event between openings: which version
//! of it arrived last, what that version changed, and the answer the user
//! gave it.
//!
//! An organizer sends every change to an event under the same UID with a
//! higher sequence, so one row per UID is enough. The row keeps what the
//! version it holds changed about the one before, so reopening the message
//! says the same thing the first opening did rather than falling silent
//! once the store has caught up. A new version clears the answer, because
//! the organizer is asking about a different meeting.

use mailrs_domain::invitation::Answer;
use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// The row for one event, as the store holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// What this version did to the one before it: `moved`, `updated` or
    /// `cancelled`. `None` when nothing came before it. The caller decides
    /// what counts; the store keeps the word.
    pub news: Option<String>,
    /// The start the version before this one had, when the event moved.
    pub moved_from: Option<EpochMillis>,
}

/// What the store holds for `uid`, if anything.
pub fn saved(conn: &Connection, account_id: AccountId, uid: &str) -> Result<Option<Saved>> {
    let row = conn
        .query_row(
            "SELECT uid, sequence, starts_at, all_day, summary, cancelled, answer, message_id, \
             news, moved_from FROM invitations WHERE account_id = ?1 AND uid = ?2",
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
                    news: row.get(8)?,
                    moved_from: row.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Records the version of an event that just arrived. A version at least as
/// new as the stored one replaces it; an older one changes nothing, which
/// is what happens when the user opens the first invitation again after an
/// update arrived. Reopening the same version keeps what the store already
/// says that version changed, so a second reading does not erase it.
pub fn remember(
    conn: &Connection,
    account_id: AccountId,
    seen: &Saved,
    now: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT INTO invitations \
         (account_id, uid, sequence, starts_at, all_day, summary, cancelled, answer, message_id, \
          seen_at, news, moved_from) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11) \
         ON CONFLICT (account_id, uid) DO UPDATE SET \
             sequence = excluded.sequence, \
             starts_at = excluded.starts_at, \
             all_day = excluded.all_day, \
             summary = excluded.summary, \
             cancelled = excluded.cancelled, \
             answer = CASE WHEN excluded.sequence > invitations.sequence \
                      THEN NULL ELSE invitations.answer END, \
             message_id = excluded.message_id, \
             seen_at = excluded.seen_at, \
             news = CASE WHEN excluded.sequence > invitations.sequence \
                    OR (excluded.cancelled = 1 AND invitations.cancelled = 0) \
                    THEN excluded.news ELSE invitations.news END, \
             moved_from = CASE WHEN excluded.sequence > invitations.sequence \
                          OR (excluded.cancelled = 1 AND invitations.cancelled = 0) \
                          THEN excluded.moved_from ELSE invitations.moved_from END \
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
            now,
            seen.news,
            seen.moved_from
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
