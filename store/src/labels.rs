//! The label list: the server mailboxes a listing named, read as labels
//! for the window and the assistant until they read server mailboxes.

use mailrs_domain::{AccountId, Label, LabelKind, MailboxKind};
use rusqlite::{Connection, params};

use crate::{Result, StoreError};

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
            Ok(MailboxKind::Group) => LabelKind::Group,
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
