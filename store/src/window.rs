//! Keeps the store to the sync window: recent mail plus everything in INBOX.

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, params};

use crate::Result;
use crate::messages::{delete_thread, refresh_thread};

/// Deletes threads whose newest message is older than `cutoff` and that are
/// not in INBOX. Returns their ids.
pub fn prune_window(
    conn: &Connection,
    account_id: AccountId,
    cutoff: EpochMillis,
) -> Result<Vec<String>> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT t.id FROM threads t WHERE t.account_id = ?1 AND t.last_message_at < ?2 \
             AND NOT EXISTS (SELECT 1 FROM thread_labels tl WHERE tl.account_id = t.account_id \
             AND tl.thread_id = t.id AND tl.label_id = 'INBOX') ORDER BY t.id",
        )?;
        stmt.query_map(params![account_id, cutoff], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        delete_thread(conn, account_id, id)?;
    }
    Ok(ids)
}

/// Deletes messages last written in a generation before `generation`, then
/// refreshes their threads. Returns the touched thread ids, sorted.
pub fn sweep_stale(
    conn: &Connection,
    account_id: AccountId,
    generation: i64,
) -> Result<Vec<String>> {
    let threads: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT thread_id FROM messages WHERE account_id = ?1 AND sync_gen < ?2 ORDER BY thread_id",
        )?;
        stmt.query_map(params![account_id, generation], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    conn.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND sync_gen < ?2",
        params![account_id, generation],
    )?;
    for thread in &threads {
        refresh_thread(conn, account_id, thread)?;
    }
    Ok(threads)
}
