//! The label list: the server mailboxes a listing named, read as labels
//! for the window and the assistant until they read server mailboxes.

use std::collections::{BTreeSet, HashMap};

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
            Ok(MailboxKind::Tag) => LabelKind::Tag,
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

/// The ids of the tags `account_id` keeps: server mailboxes that are marks
/// beside a folder rather than places. A move leaves them on the message.
pub fn tag_ids(conn: &Connection, account_id: AccountId) -> Result<BTreeSet<String>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM mailboxes WHERE account_id = ?1 AND kind = 'tag'")?;
    let ids = stmt.query_map(params![account_id], |row| row.get::<_, String>(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

/// Where the person put each label they moved, by id. A label never
/// moved has no entry, and the sidebar puts it after the others by name.
pub fn positions(conn: &Connection, account_id: AccountId) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT id, position FROM mailboxes \
         WHERE account_id = ?1 AND named = 1 AND position IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![account_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{MailboxKind, RemoteMailbox};

    use super::positions;
    use crate::{accounts, mailboxes};

    fn folder(id: &str) -> RemoteMailbox {
        RemoteMailbox {
            id: id.into(),
            name: id.into(),
            kind: MailboxKind::Folder,
            role: None,
            color: None,
            hidden: false,
        }
    }

    #[test]
    fn a_stored_order_survives_a_listing_and_follows_a_rename() {
        let conn = crate::open_in_memory().unwrap();
        let me = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        for id in ["Work", "Personal", "Travel"] {
            mailboxes::upsert(&conn, me, &folder(id)).unwrap();
        }
        mailboxes::set_positions(&conn, me, &["Personal".into(), "Work".into()]).unwrap();
        // A listing stores every mailbox again, and a rename moves the row.
        mailboxes::upsert(&conn, me, &folder("Personal")).unwrap();
        mailboxes::rename(&conn, me, "Work", &folder("Job")).unwrap();

        let order = positions(&conn, me).unwrap();
        assert_eq!(order.get("Personal"), Some(&0));
        assert_eq!(order.get("Job"), Some(&1));
        assert_eq!(order.get("Travel"), None, "a label never moved has no position");
    }
}
