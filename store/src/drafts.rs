//! Gmail's two names for one draft. A draft has an id of its own, and the
//! message inside it has another, and `drafts.list` is the only call that
//! maps one to the other. It charges per page of the whole account, so the
//! pairs are kept here: an account pays for that listing once rather than
//! every time somebody opens a draft.
//!
//! A pair is worth no more than the message behind it. Editing a draft
//! elsewhere gives it a new message and leaves the old id naming text
//! nobody holds; sending or deleting one leaves it naming nothing at all.
//! So [`draft_of`] answers only while the store still shows that message
//! carrying the DRAFT label. History replay is what keeps that answer
//! honest: it takes the label off a draft that was sent and the message
//! off one that was deleted.

use mailrs_domain::AccountId;
use mailrs_domain::system_label::DRAFT;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Records that `draft_id` currently holds `message_id`. Either half
/// replaces the row it collides with, because Gmail gives one draft one
/// message and one message one draft.
pub fn remember(
    conn: &Connection,
    account_id: AccountId,
    draft_id: &str,
    message_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO drafts (account_id, message_id, draft_id) VALUES (?1, ?2, ?3)",
        params![account_id, message_id, draft_id],
    )?;
    Ok(())
}

/// Drops the pair for a draft that has been sent or deleted.
pub fn forget(conn: &Connection, account_id: AccountId, draft_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM drafts WHERE account_id = ?1 AND draft_id = ?2",
        params![account_id, draft_id],
    )?;
    Ok(())
}

/// Everything Gmail listed, as the account's whole set of pairs. Each one
/// is a draft id with the message inside it. Rows for drafts the listing
/// left out go, since the listing is Gmail's own answer to what the
/// account has.
pub fn replace_all(
    conn: &Connection,
    account_id: AccountId,
    drafts: &[(String, String)],
) -> Result<()> {
    conn.execute(
        "DELETE FROM drafts WHERE account_id = ?1",
        params![account_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT OR REPLACE INTO drafts (account_id, message_id, draft_id) VALUES (?1, ?2, ?3)",
    )?;
    for (draft_id, message_id) in drafts {
        insert.execute(params![account_id, message_id, draft_id])?;
    }
    Ok(())
}

/// The draft holding `message_id`, for reopening it in the composer.
/// `None` when no pair is held, and also when the stored message is no
/// longer a draft, which is how a pair Gmail has moved on from is caught
/// without asking Gmail anything.
pub fn draft_of(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.draft_id FROM drafts d JOIN message_labels l \
         ON l.account_id = d.account_id AND l.message_id = d.message_id \
         WHERE d.account_id = ?1 AND d.message_id = ?2 AND l.label_id = ?3",
    )?;
    Ok(stmt
        .query_row(params![account_id, message_id, DRAFT], |row| row.get(0))
        .optional()?)
}
