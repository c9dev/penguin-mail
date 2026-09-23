//! The label list, read from the server mailboxes. Until the store's
//! interface stops naming mail by Gmail label, a label is a server
//! mailbox the server listed, and its kind is system for Gmail's own and
//! user for a person's.

use mailrs_domain::{AccountId, Label, LabelKind, MailboxKind, gmail};
use rusqlite::{Connection, params};

use crate::{Result, StoreError};

fn stored_kind(kind: LabelKind) -> &'static str {
    match kind {
        LabelKind::System => MailboxKind::System.as_str(),
        LabelKind::User => MailboxKind::Label.as_str(),
    }
}

/// Replaces every label of the account. A label the listing left out
/// goes, unless mail still carries it, in which case it stays as an
/// unlisted mailbox so the mail keeps its key.
pub fn replace_labels(conn: &Connection, account_id: AccountId, labels: &[Label]) -> Result<()> {
    for label in labels {
        upsert_label(conn, label)?;
    }
    let listed: Vec<&str> = labels.iter().map(|l| l.id.as_str()).collect();
    let listed = serde_json::to_string(&listed).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "UPDATE mailboxes SET named = 0 WHERE account_id = ?1 \
         AND id NOT IN (SELECT value FROM json_each(?2)) \
         AND EXISTS (SELECT 1 FROM message_mailboxes l WHERE l.mailbox = mailboxes.key)",
        params![account_id, listed],
    )?;
    conn.execute(
        "DELETE FROM mailboxes WHERE account_id = ?1 \
         AND id NOT IN (SELECT value FROM json_each(?2)) \
         AND NOT EXISTS (SELECT 1 FROM message_mailboxes l WHERE l.mailbox = mailboxes.key)",
        params![account_id, listed],
    )?;
    Ok(())
}

/// Adds a label or updates its name, kind and colour, and marks it listed.
pub fn upsert_label(conn: &Connection, label: &Label) -> Result<()> {
    conn.execute(
        "INSERT INTO mailboxes (account_id, id, name, role, kind, color, named) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1) \
         ON CONFLICT (account_id, id) DO UPDATE SET name = excluded.name, role = excluded.role, \
         kind = excluded.kind, color = excluded.color, named = 1",
        params![
            label.account_id,
            label.id,
            label.name,
            gmail::role_of(&label.id).map(|r| r.as_str()),
            stored_kind(label.kind),
            label.color
        ],
    )?;
    Ok(())
}

/// Removes a label and takes it off every message and thread. Returns the
/// threads that carried it.
pub fn delete_label(conn: &Connection, account_id: AccountId, id: &str) -> Result<Vec<String>> {
    let threads: Vec<String> = conn
        .prepare(
            "SELECT t.thread_id FROM thread_mailboxes t JOIN mailboxes b ON b.key = t.mailbox \
             WHERE b.account_id = ?1 AND b.id = ?2",
        )?
        .query_map(params![account_id, id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // Deleting the mailbox takes its message and thread rows with it.
    conn.execute(
        "DELETE FROM mailboxes WHERE account_id = ?1 AND id = ?2",
        params![account_id, id],
    )?;
    Ok(threads)
}

/// System labels first, then user labels, each by name. Only mailboxes a
/// listing named count.
pub fn list_labels(conn: &Connection, account_id: AccountId) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, color FROM mailboxes WHERE account_id = ?1 AND named = 1 \
         ORDER BY kind <> 'system', name",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (id, name, kind, color) = row?;
        let kind = match kind.parse::<MailboxKind>() {
            Ok(MailboxKind::System) => LabelKind::System,
            Ok(MailboxKind::Label | MailboxKind::Folder) => LabelKind::User,
            Err(_) => {
                return Err(StoreError::Corrupt {
                    column: "mailboxes.kind",
                    value: kind,
                });
            }
        };
        Ok(Label {
            account_id,
            id,
            name,
            kind,
            color,
        })
    })
    .collect()
}
