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
        "INSERT INTO labels (account_id, id, name, kind) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for label in labels {
        insert.execute(params![
            account_id,
            label.id,
            label.name,
            label.kind.as_str()
        ])?;
    }
    Ok(())
}

/// System labels first, then user labels, each by name.
pub fn list_labels(conn: &Connection, account_id: AccountId) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind FROM labels WHERE account_id = ?1 ORDER BY kind = 'user', name",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (id, name, kind) = row?;
        let kind = kind.parse::<LabelKind>().map_err(|_| StoreError::Corrupt {
            column: "labels.kind",
            value: kind.clone(),
        })?;
        Ok(Label {
            account_id,
            id,
            name,
            kind,
        })
    })
    .collect()
}
