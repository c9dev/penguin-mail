//! The server mailboxes each account's server lists, as the adapter
//! describes them. Mail is filed under their integer keys; see
//! `messages::mailbox_key` for a mailbox met on a message before any
//! listing named it.

use mailrs_domain::{AccountId, MailboxKind, RemoteMailbox, Role};
use rusqlite::{Connection, params};

use crate::{Result, StoreError};

/// Adds a listed mailbox or updates it, and marks it listed.
pub fn upsert(conn: &Connection, account_id: AccountId, mailbox: &RemoteMailbox) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO mailboxes (account_id, id, name, role, kind, color, hidden, named) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1) \
         ON CONFLICT (account_id, id) DO UPDATE SET name = excluded.name, role = excluded.role, \
         kind = excluded.kind, color = excluded.color, hidden = excluded.hidden, named = 1",
    )?
    .execute(params![
        account_id,
        mailbox.id,
        mailbox.name,
        mailbox.role.map(Role::as_str),
        mailbox.kind.as_str(),
        mailbox.color,
        mailbox.hidden,
    ])?;
    Ok(())
}

/// Makes `listed` the account's whole list, as a bootstrap does. A mailbox
/// the listing left out goes, unless mail is still filed under it, in
/// which case it stays unlisted so the mail keeps its key.
pub fn replace_listed(
    conn: &Connection,
    account_id: AccountId,
    listed: &[RemoteMailbox],
) -> Result<()> {
    for mailbox in listed {
        upsert(conn, account_id, mailbox)?;
    }
    let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
    let ids = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "UPDATE mailboxes SET named = 0 WHERE account_id = ?1 \
         AND id NOT IN (SELECT value FROM json_each(?2)) \
         AND EXISTS (SELECT 1 FROM message_mailboxes l WHERE l.mailbox = mailboxes.key)",
        params![account_id, ids],
    )?;
    conn.execute(
        "DELETE FROM mailboxes WHERE account_id = ?1 \
         AND id NOT IN (SELECT value FROM json_each(?2)) \
         AND NOT EXISTS (SELECT 1 FROM message_mailboxes l WHERE l.mailbox = mailboxes.key)",
        params![account_id, ids],
    )?;
    Ok(())
}

/// Removes a mailbox and takes it off every message and thread, as when it
/// was deleted on the server. Returns the threads that were in it.
pub fn delete(conn: &Connection, account_id: AccountId, id: &str) -> Result<Vec<String>> {
    let threads: Vec<String> = conn
        .prepare(
            "SELECT t.thread_id FROM thread_mailboxes t JOIN mailboxes b ON b.key = t.mailbox \
             WHERE b.account_id = ?1 AND b.id = ?2",
        )?
        .query_map(params![account_id, id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // The mailbox's message and thread rows go with it.
    conn.execute(
        "DELETE FROM mailboxes WHERE account_id = ?1 AND id = ?2",
        params![account_id, id],
    )?;
    Ok(threads)
}

/// The mailboxes the server listed, by id.
pub fn listed(conn: &Connection, account_id: AccountId) -> Result<Vec<RemoteMailbox>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, role, kind, color, hidden FROM mailboxes \
         WHERE account_id = ?1 AND named = 1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (id, name, role, kind, color, hidden) = row?;
        let role = role
            .map(|r| {
                r.parse::<Role>().map_err(|_| StoreError::Corrupt {
                    column: "mailboxes.role",
                    value: r,
                })
            })
            .transpose()?;
        let kind = kind
            .parse::<MailboxKind>()
            .map_err(|_| StoreError::Corrupt {
                column: "mailboxes.kind",
                value: kind.clone(),
            })?;
        Ok(RemoteMailbox {
            id,
            name,
            kind,
            role,
            color,
            hidden,
        })
    })
    .collect()
}
