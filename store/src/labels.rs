use mailrs_domain::{AccountId, Label, LabelKind};
use rusqlite::{Connection, params};

use crate::{Result, StoreError};

/// Replaces every label of the account.
pub fn replace_labels(conn: &Connection, account_id: AccountId, labels: &[Label]) -> Result<()> {
    conn.execute(
        "DELETE FROM labels WHERE account_id = ?1",
        params![account_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO labels (account_id, id, name, kind, color) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for label in labels {
        insert.execute(params![
            account_id,
            label.id,
            label.name,
            label.kind.as_str(),
            label.color
        ])?;
    }
    Ok(())
}

/// Adds a label or updates its name.
pub fn upsert_label(conn: &Connection, label: &Label) -> Result<()> {
    conn.execute(
        "INSERT INTO labels (account_id, id, name, kind, color) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (account_id, id) DO UPDATE SET name = excluded.name, kind = excluded.kind, \
         color = excluded.color",
        params![
            label.account_id,
            label.id,
            label.name,
            label.kind.as_str(),
            label.color
        ],
    )?;
    Ok(())
}

/// Removes a label and takes it off every message and thread. Returns the
/// threads that carried it.
pub fn delete_label(conn: &Connection, account_id: AccountId, id: &str) -> Result<Vec<String>> {
    let threads: Vec<String> = conn
        .prepare("SELECT thread_id FROM thread_labels WHERE account_id = ?1 AND label_id = ?2")?
        .query_map(params![account_id, id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for table in ["message_labels", "thread_labels"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account_id = ?1 AND label_id = ?2"),
            params![account_id, id],
        )?;
    }
    conn.execute(
        "DELETE FROM labels WHERE account_id = ?1 AND id = ?2",
        params![account_id, id],
    )?;
    Ok(threads)
}

/// System labels first, then user labels, each by name.
pub fn list_labels(conn: &Connection, account_id: AccountId) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, color FROM labels WHERE account_id = ?1 ORDER BY kind = 'user', name",
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
        let kind = kind.parse::<LabelKind>().map_err(|_| StoreError::Corrupt {
            column: "labels.kind",
            value: kind.clone(),
        })?;
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
