//! Accounts and their sync cursors.

use mailrs_domain::{Account, AccountId, AccountState, EpochMillis};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError};

/// Where sync left off for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCursor {
    /// Gmail history id to replay from. `None` until the first bootstrap.
    pub history_id: Option<u64>,
    /// Page token for the next window page.
    pub backfill_cursor: Option<String>,
    pub backfill_done: bool,
    pub sync_gen: i64,
}

/// Adds an account, or returns the existing id for that email.
pub fn insert_account(conn: &Connection, email: &str, now: EpochMillis) -> Result<AccountId> {
    conn.execute(
        "INSERT OR IGNORE INTO accounts (email, added_at) VALUES (?1, ?2)",
        params![email, now],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM accounts WHERE email = ?1",
        params![email],
        |row| row.get(0),
    )?)
}

pub fn list_accounts(conn: &Connection) -> Result<Vec<Account>> {
    let mut stmt = conn.prepare("SELECT id, email, state FROM accounts ORDER BY id")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.map(|row| {
        let (id, email, state) = row?;
        to_account(id, email, state)
    })
    .collect()
}

pub fn account_by_email(conn: &Connection, email: &str) -> Result<Option<Account>> {
    let row: Option<(AccountId, String, String)> = conn
        .query_row(
            "SELECT id, email, state FROM accounts WHERE email = ?1",
            params![email],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(id, email, state)| to_account(id, email, state))
        .transpose()
}

/// Deletes the account and, through foreign keys, all of its mail.
pub fn delete_account(conn: &Connection, id: AccountId) -> Result<()> {
    conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn set_state(conn: &Connection, id: AccountId, state: AccountState) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET state = ?2 WHERE id = ?1",
        params![id, state.as_str()],
    )?;
    Ok(())
}

pub fn sync_cursor(conn: &Connection, id: AccountId) -> Result<SyncCursor> {
    Ok(conn.query_row(
        "SELECT history_id, backfill_cursor, backfill_done, sync_gen FROM accounts WHERE id = ?1",
        params![id],
        |row| {
            Ok(SyncCursor {
                history_id: row.get::<_, Option<i64>>(0)?.map(|h| h as u64),
                backfill_cursor: row.get(1)?,
                backfill_done: row.get(2)?,
                sync_gen: row.get(3)?,
            })
        },
    )?)
}

pub fn set_history_id(conn: &Connection, id: AccountId, history_id: u64) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET history_id = ?2 WHERE id = ?1",
        params![id, history_id as i64],
    )?;
    Ok(())
}

pub fn set_backfill(
    conn: &Connection,
    id: AccountId,
    cursor: Option<&str>,
    done: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET backfill_cursor = ?2, backfill_done = ?3 WHERE id = ?1",
        params![id, cursor, done],
    )?;
    Ok(())
}

/// Starts a new sync generation from `history_id`, clears the backfill
/// cursor, and returns the new generation number.
pub fn start_generation(conn: &Connection, id: AccountId, history_id: u64) -> Result<i64> {
    Ok(conn.query_row(
        "UPDATE accounts SET history_id = ?2, backfill_cursor = NULL, backfill_done = 0, \
         sync_gen = sync_gen + 1 WHERE id = ?1 RETURNING sync_gen",
        params![id, history_id as i64],
        |row| row.get(0),
    )?)
}

fn to_account(id: AccountId, email: String, state: String) -> Result<Account> {
    let state = state
        .parse::<AccountState>()
        .map_err(|_| StoreError::Corrupt {
            column: "accounts.state",
            value: state.clone(),
        })?;
    Ok(Account { id, email, state })
}
