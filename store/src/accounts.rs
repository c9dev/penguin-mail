//! Accounts and their sync cursors.

use mailrs_domain::{Account, AccountId, AccountState, EpochMillis, Provider, SignInClient};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError};

/// Where sync left off for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCursor {
    /// Where the account's mail backend left its feed of changes, in the
    /// backend's own words. `None` until the first bootstrap.
    pub state: Option<String>,
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
    let mut stmt = conn.prepare("SELECT id, email, state, provider FROM accounts ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    rows.map(|row| {
        let (id, email, state, provider) = row?;
        to_account(id, email, state, provider)
    })
    .collect()
}

pub fn account_by_email(conn: &Connection, email: &str) -> Result<Option<Account>> {
    let row: Option<(AccountId, String, String, String)> = conn
        .query_row(
            "SELECT id, email, state, provider FROM accounts WHERE email = ?1",
            params![email],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    row.map(|(id, email, state, provider)| to_account(id, email, state, provider))
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

/// The Google client `id` signs in with.
pub fn sign_in_client(conn: &Connection, id: AccountId) -> Result<SignInClient> {
    let value: String = conn.query_row(
        "SELECT oauth_client FROM accounts WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    value.parse().map_err(|_| StoreError::Corrupt {
        column: "accounts.oauth_client",
        value: value.clone(),
    })
}

/// Records that `id` signed in with `client`.
pub fn set_sign_in_client(conn: &Connection, id: AccountId, client: SignInClient) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET oauth_client = ?2 WHERE id = ?1",
        params![id, client.as_str()],
    )?;
    Ok(())
}

/// When the account's inbox was last checked against Gmail's, if ever.
pub fn checked_at(conn: &Connection, id: AccountId) -> Result<Option<EpochMillis>> {
    Ok(conn
        .query_row(
            "SELECT checked_at FROM accounts WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<EpochMillis>>(0),
        )
        .optional()?
        .flatten())
}

pub fn set_checked_at(conn: &Connection, id: AccountId, at: EpochMillis) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET checked_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

pub fn sync_cursor(conn: &Connection, id: AccountId) -> Result<SyncCursor> {
    Ok(conn.query_row(
        "SELECT sync_state, backfill_cursor, backfill_done, sync_gen FROM accounts WHERE id = ?1",
        params![id],
        |row| {
            Ok(SyncCursor {
                state: row.get(0)?,
                backfill_cursor: row.get(1)?,
                backfill_done: row.get(2)?,
                sync_gen: row.get(3)?,
            })
        },
    )?)
}

pub fn set_sync_state(conn: &Connection, id: AccountId, state: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET sync_state = ?2 WHERE id = ?1",
        params![id, state],
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

/// Starts a new sync generation from the sync state `state`, clears the
/// backfill cursor, and returns the new generation number.
pub fn start_generation(conn: &Connection, id: AccountId, state: &str) -> Result<i64> {
    Ok(conn.query_row(
        "UPDATE accounts SET sync_state = ?2, backfill_cursor = NULL, backfill_done = 0, \
         sync_gen = sync_gen + 1 WHERE id = ?1 RETURNING sync_gen",
        params![id, state],
        |row| row.get(0),
    )?)
}

fn to_account(id: AccountId, email: String, state: String, provider: String) -> Result<Account> {
    let state = state
        .parse::<AccountState>()
        .map_err(|_| StoreError::Corrupt {
            column: "accounts.state",
            value: state.clone(),
        })?;
    let provider = provider
        .parse::<Provider>()
        .map_err(|_| StoreError::Corrupt {
            column: "accounts.provider",
            value: provider.clone(),
        })?;
    Ok(Account {
        id,
        email,
        state,
        provider,
    })
}
